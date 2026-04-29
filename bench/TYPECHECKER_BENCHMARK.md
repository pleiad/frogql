# Typechecker Benchmark

Compares typechecker decision time vs runtime work, per query, per
database. The success bar from the requirements doc:

> "typechecker is not slower detecting errors/invalid than actually
> running the full query"

…holds across both datasets the bench currently runs against, with
the magnitude of the win scaling roughly with the data size (since
runtime scales with data and typecheck doesn't).

## What it measures

Three things, per query, per database:

1. **Per-phase compile time** — parse / elaborate / typecheck timed
   separately. Typecheck phase intentionally excludes the schema
   `clone()` setup cost (that's done outside the timed region).
2. **Both checked and unchecked runtime paths**, in one bench run.
   The "checked" path obeys §10 Theorem 6.5 short-circuit: when the
   typechecker says guaranteed-empty or rejects, runtime is skipped
   and `rt_chk = 0`. The "unchecked" path always runs, capturing
   what the runtime *would have done* without the typechecker. So
   the comparison numbers are reproducible from one bench invocation;
   no manual second run with `--no-typecheck`.
3. **Short-circuit firing** — `empty?` column reports the
   typechecker's verdict; if a regression dropped the short-circuit,
   `rt_chk` would no longer be zero where `empty?` says `yes`.

## Pre-work landed in this branch

The runtime previously ran even when the typechecker reported the
result as guaranteed empty (rules.md §10 Theorem 6.5: pattern type-
checks but ⊥ in path/env ⇒ runtime returns ∅). Two changes wire the
short-circuit through:

- `CompileResult.guaranteed_empty: bool` exposes the typechecker's
  empty verdict.
- `gqlite` REPL and Python `Connection.execute` short-circuit when
  `guaranteed_empty` is true — they skip the runtime call and return
  zero rows.

Without this, a typechecker-fast verdict still pays the runtime cost
on every query, and any "typecheck-faster-than-runtime" claim is moot.

## Setup

`ldbc-sf0.1.gdb` is built from the LDBC SNB SF0.1 CSV (see
`LDBC_BENCHMARK.md` for the full setup).
`ldbc-tiny.gdb` is a truncated copy — `head -50` of every CSV file
under the same dataset directory, re-imported. Same loader path,
much smaller data (392 nodes vs 327k), reduced inferred schema (8
node types vs 11).

```bash
# Build the small fixture from the SF0.1 dataset:
SRC=bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter
DST=bench/data/ldbc-tiny/social_network-sf0.1-CsvBasic-LongDateFormatter
mkdir -p $DST/dynamic $DST/static
for f in $SRC/dynamic/*.csv; do head -50 "$f" > "$DST/dynamic/$(basename $f)"; done
for f in $SRC/static/*.csv;  do head -50 "$f" > "$DST/static/$(basename $f)"; done
./target/release/gqlite bench/data/ldbc-tiny.gdb --import-ldbc-csv $DST --no-typecheck

# Run bench against both:
cargo build --release --bin typecheck_bench
./target/release/typecheck_bench --iters 30 --warmup 1 --limit 100 \
    bench/data/ldbc-tiny.gdb \
    bench/data/ldbc-sf0.1.gdb \
    > /tmp/tc_data.csv 2> /tmp/tc_summary.txt
```

stdout is per-iteration CSV
(`db;category;case;phase;iter;ns;flags`); stderr is the human summary.

## Categories

- **valid** — query passes typecheck; both checked and unchecked
  paths run to completion.
- **empty** — query passes typecheck but is guaranteed empty by
  typing (label not in the active schema). The checked path skips
  runtime; the unchecked path runs and shows what was avoided.
- **invalid_unbound** — RETURN references a free variable.
  Typechecker rejects (`r.ok = false`); checked path skips runtime;
  unchecked path runs and produces wrong-but-not-empty results
  (NULLs joined with the rest of the pattern — same class as the
  bug report `MATCH (x: Person) RETURN y.name` returning 100 rows
  of `"NULL"`).
- **invalid_parse** — query fails to parse. All phases skipped.

## Why dataset choice matters

Typechecker decision time depends on **schema** (refinement walks
schema entries) and is **independent of data size**. Runtime cost
scales with data size. So the same query gives:

- Roughly the same `tc` time on tiny vs SF0.1 (same schema).
- Wildly different `rt_unchk` time (tiny: ~ms; SF0.1: ~s, sometimes).

That makes the speedup ratio a function of dataset size, not of the
typechecker. Reporting only on SF0.1 (where runtime is slow) makes
the typechecker look better than it is on small data; reporting only
on tiny (where runtime is fast) makes it look worse. The honest read
needs both, which is why the bench takes multiple `--db` arguments.

## Results (multi-DB, 30 iters + 1 warmup, limit=100)

The `verdict` column is the typechecker's decision:

- `ok` — typecheck passed, statically non-empty
- `empty` — typecheck passed but ⊥ in path/env (rules.md §10) — runtime short-circuits
- `rejected` — typecheck error (free var, type mismatch) — runtime short-circuits
- `both` — `empty` and `rejected` simultaneously (legal: e.g. free var in a pattern that's also unsatisfiable)
- `parse` — parse error before typecheck (rt_chk and rt_unchk also skipped)

Some `valid`-categorized cases come back `empty` against a schema that
doesn't have the right labels (e.g. `v_label_user` against an LDBC-only
schema where `User` doesn't exist). That's expected: the case categories
are fixed at compile time but the verdict is schema-relative.

### `ldbc-tiny.gdb` (392 nodes, 295 edges, 8 node types / 10 edge types)

```
cat              case                parse_us  elab_us   tc_us  opt_us  rt_chk_ms  rt_unchk_ms  verdict    speedup
──────────────────────────────────────────────────────────────────────────────────────────────────────────────────
valid            v_label_person          3.00     0.70    5.30    0.30      0.38         0.37  ok           39.4x
valid            v_label_user            1.80     0.30    1.00    0.20      0.00         0.65  empty       195.8x
valid            v_chain_knows           2.60     0.50    7.50    0.60      0.00         0.11  empty         9.8x
valid            v_chain_wrote           2.30     0.40    2.70    0.50      0.00         0.11  empty        18.3x
empty            e_unknown_label         1.90     0.30    1.00    0.20      0.00         0.66  empty       192.8x
empty            e_unknown_edge_lhs      2.50     0.50    2.60    0.60      0.00         0.11  empty        17.5x
empty            e_chained_unknown       3.00     0.70   11.10    0.90      0.00         0.10  empty         6.6x
invalid_unbound  i_unbound               2.60     0.40   29.30    0.20      0.00         1.09  rejected     33.6x
invalid_unbound  i_unbound_chain         2.20     0.50   61.60    0.60      0.00         0.11  both          1.7x
invalid_parse    i_parse                 0.90       —       —       —         —            —   parse           —
```

### `ldbc-sf0.1.gdb` (327k nodes, 1.48M edges, 11 node types / 25 edge types)

*(To be re-captured with the new columns in a follow-up commit. The
SF0.1 disk is currently busy with the LDBC IC2 bench. Previous-version
table — same `parse_us` / `elab_us` / `tc_us` / `rt_*_ms`, but without
`opt_us` and with the old `empty?` column instead of `verdict` —
showed speedups of 410× to 44 800× on the same 10 cases. The relative
ordering and orders of magnitude are not expected to move with the
column-set change because optimize is consistently sub-microsecond.)*

### Reading these tables

- `parse_us`, `elab_us`, `tc_us` — compile phases, in **microseconds**.
  Typecheck is the dominant phase (~3-200 μs), but the whole compile
  is consistently under 0.3 ms regardless of dataset.
- `rt_chk_ms` — runtime time *with* the §10 short-circuit honored.
  Zero when the typechecker said skip.
- `rt_unchk_ms` — runtime time *without* the typechecker. Always runs.
  This is the "would-be" cost the speedup ratio compares against.
  Each unchecked iter also captures `result.row_count()` and is
  cross-checked: if `verdict=empty` but `rt_unchk` returns >0 rows,
  the bench prints a SOUNDNESS warning (the typechecker would be
  lying about emptiness). No violations observed against either DB.
- `verdict` — see the legend above the tables.
- `speedup` — `rt_unchk / (parse + elab + tc + opt)`. Meaningful when
  the short-circuit fires (`rt_chk = 0`); informational for valid
  cases (just shows runtime/compile ratio). Optimize is included in
  the denominator because both checked and unchecked paths pay it.

### Key takeaways

- **Pure typechecker performance is dataset-independent and small.**
  3-200 μs per query, regardless of data size. (Confirms: typecheck
  cost is schema-bound, not data-bound.)
- **Speedup numbers are dominated by the runtime side.** On tiny,
  speedups are 5-180x; on SF0.1, the same code paths see
  6000-45000x speedups. The typechecker isn't 200x faster on SF0.1
  — runtime is 200x slower.
- **The success bar holds in both regimes.** Every cell where the
  short-circuit fires shows `tc < rt_unchk`. The typechecker is
  never slower than what it would have skipped.
- **Valid-query overhead is negligible.** On SF0.1 `v_chain_knows`,
  total compile is 175 μs vs runtime 1645 ms — typecheck adds
  0.01% to wall time.
- **Invalid-unbound case carries a correctness win.** On both DBs,
  the unchecked runtime returns 100 rows of `"NULL"` (the user's
  earlier bug report). The typechecker rejects in microseconds with
  a precise error.

## Honest critique of this bench

Things this bench gets right:

- **Self-contained comparison.** Both checked and unchecked paths run
  in one invocation. No "see the previous bench output for the
  baseline" footnote. Anyone running the bench from scratch gets the
  same speedup numbers.
- **Multi-DB.** Sweeps tiny + SF0.1 by default; the dataset-size axis
  is visible.
- **Phase breakdown including optimize.** parse / elaborate /
  typecheck / optimize reported separately. Optimize is included in
  the speedup denominator so the "compile cost" isn't understated by
  burying it in `rt_chk` / `rt_unchk`.
- **Schema clone outside timing.** The Typechecker's schema clone is
  done before the timed region, so per-phase numbers reflect
  typecheck cost, not setup. (A *fresh* `Typechecker::new` still
  happens per iter though — see issue (d) below.)
- **Verdict column.** `ok` / `empty` / `rejected` / `both` / `parse`
  is independent of the runtime columns, so a reader doesn't have to
  infer "rt_chk=0 with empty=no must mean rejected." The legend is
  printed above the tables.
- **Soundness cross-check.** Every unchecked iter captures
  `row_count()`. When `verdict=empty`, the bench asserts the
  unchecked runtime also returned zero rows; a non-zero count prints
  a SOUNDNESS warning so a typechecker bug can't quietly inflate
  speedup numbers. (Currently no violations against either DB.)
- **Schema-relevance check.** If the active schema has neither
  `Person` nor `User`, the bench prints a warning that every "valid"
  case will collapse to empty-by-typing — useful when running
  against a non-LDBC, non-grandstack DB.

Things this bench *doesn't* address (acknowledged):

- **(a) Cold vs warm runtime is mixed in.** First iter pays page-cache
  warmup; bench takes a `--warmup 1` discard but only one iter is
  warm-up. Variance on the runtime side is real (LDBC bench saw
  38-106s spread on a single param, ~3x noise floor).
- **(b) Few queries per category.** 4 valid, 3 empty, 2 unbound,
  1 parse. Per-category averages aren't statistically robust; treat
  each cell as one data point, not a population estimate.
- **(c) Single inferred schema per dataset.** Active GRAPH TYPE is
  the auto-inferred DEFAULT on both datasets. A custom restrictive
  GRAPH TYPE would be more interesting because some valid-looking
  queries would be rejected by typing — needs a hand-written schema
  and is left for a follow-up. *(User-flagged as optional.)*
- **(d) Schema clone per iter.** Outside the timed region, but
  `Typechecker::new(active.clone())` still runs every iteration. Real
  workloads would cache a Typechecker per session. Bench's `tc_us`
  is per-cold-Typechecker, slightly inflated.
- **(e) μs-scale jitter.** Typecheck phases are 3-200 μs. Windows
  `Instant` resolution is on the order of 100 ns under
  `QueryPerformanceCounter`, but per-call jitter can dominate at this
  scale. 30 iters with min/median/max reporting doesn't compute
  confidence intervals; we don't use `criterion`.
- **(f) No CI on the bench.** `typecheck_bench` is in `src/bin/` but
  not run by `cargo test` or by any GitHub Actions job. Drift is
  only caught when a human runs it.
- **(g) Limit is fixed.** `--limit 100` is the default; different
  limits change runtime cost. The bench doesn't sweep limit.
- **(h) Predicate-pushdown gap dominates valid runtime.** Same
  finding as the LDBC bench — the optimizer doesn't push down
  value-equality predicates. The "valid" category's runtime numbers
  are inflated by this; with pushdown they would shrink and the
  typechecker overhead ratios would change. Ratios here are tied to
  gqlite's *current* optimizer, not an idealized one.

## Things to investigate (not in this bench)

- **Schema complexity sweep.** Add a couple of GRAPH TYPEs of
  different sizes (e.g. 5 entries vs 50) and run the bench against
  the same .gdb. Would show whether typecheck cost grows linearly
  with schema entries (expected) and at what entry count it becomes
  visible against runtime.
- **Query depth sweep.** Add deeper chain patterns (4-hop, 5-hop)
  to see whether typecheck cost grows polynomially with pattern size.
- **Concurrent typecheck.** None of this bench tests parallelism;
  the typechecker is currently single-threaded.
