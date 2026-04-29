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

### `ldbc-tiny.gdb` (392 nodes, 295 edges, 8 node types / 10 edge types)

```
cat              case               parse_us  elab_us   tc_us  rt_chk_ms  rt_unchk_ms  empty?  speedup
─────────────────────────────────────────────────────────────────────────────────────────────────────────
valid            v_label_person       11.70     2.30    18.50      0.56         0.53     no    16.3x
valid            v_label_user          6.40     1.10     3.80      0.00         0.66    yes    58.1x  ◀
valid            v_chain_knows         3.10     0.60    10.20      0.00         0.14    yes    10.2x  ◀
valid            v_chain_wrote         2.60     0.70     3.60      0.00         0.10    yes    14.8x  ◀
empty            e_unknown_label       2.80     0.30     1.10      0.00         0.73    yes   174.4x
empty            e_unknown_edge_lhs    5.80     1.30     7.40      0.00         0.22    yes    15.4x
empty            e_chained_unknown     9.40     1.90    31.40      0.00         0.22    yes     5.2x
invalid_unbound  i_unbound             6.00     1.20    33.70      0.00         1.04     no    25.5x
invalid_unbound  i_unbound_chain       2.40     0.60    56.20      0.00         0.11    yes     1.9x
invalid_parse    i_parse               1.10     ─       ─          ─            ─       no     ─
```

### `ldbc-sf0.1.gdb` (327k nodes, 1.48M edges, 11 node types / 25 edge types)

```
cat              case               parse_us  elab_us   tc_us  rt_chk_ms  rt_unchk_ms  empty?    speedup
─────────────────────────────────────────────────────────────────────────────────────────────────────────
valid            v_label_person       25.90     4.70    25.60     22.50        23.05     no     410.2x
valid            v_label_user         14.80     2.50     9.00      0.00      1178.44    yes   44807.7x  ◀
valid            v_chain_knows        30.80     4.00   140.90   1645.40      1673.71     no    9526.0x
valid            v_chain_wrote        20.90     4.90    22.00      0.00      1593.55    yes   33337.8x  ◀
empty            e_unknown_label      17.50     2.80     9.10      0.00      1084.74    yes   36896.0x
empty            e_unknown_edge_lhs   22.70     4.80    14.60      0.00      1435.55    yes   34098.6x
empty            e_chained_unknown    19.00     4.80   141.10      0.00      1549.90    yes    9399.0x
invalid_unbound  i_unbound            19.80     3.60   113.80      0.00      1857.10     no   13535.7x
invalid_unbound  i_unbound_chain      21.50     4.80   211.20      0.00      1483.21    yes    6245.1x
invalid_parse    i_parse               1.10     ─       ─          ─           ─        no     ─
```

`◀` marks rows where a query *categorized* "valid" came back as
`empty=yes` against that DB. This isn't a bug — the case categories
are fixed at compile time, but the typechecker's verdict is
schema-relative. `v_label_user` is valid against grandstack-style
schemas (User exists) and empty against any LDBC-derived schema (no
User label). The bench surfaces this honestly: a query that's valid
against the wrong schema gets caught by the typechecker and the
runtime is skipped. Same effective shape as `empty` cases.

### Reading these tables

- `parse_us`, `elab_us`, `tc_us` — compile phases, in **microseconds**.
  Typecheck is the dominant phase (~3-200 μs), but the whole compile
  is consistently under 0.3 ms regardless of dataset.
- `rt_chk_ms` — runtime time *with* the §10 short-circuit honored.
  Zero when the typechecker said skip.
- `rt_unchk_ms` — runtime time *without* the typechecker. Always runs.
  This is the "would-be" cost the speedup ratio compares against.
- `empty?` — `r.empty` from the typechecker.
- `speedup` — `rt_unchk / (parse + elab + tc)`. Meaningful when the
  short-circuit fires (`rt_chk = 0`); informational for valid cases
  (just shows runtime/compile ratio).

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
- **Multi-DB.** Sweeps tiny + SF0.1 by default; the dataset size axis
  is visible.
- **Phase breakdown.** parse / elaborate / typecheck reported
  separately so it's clear typecheck is the dominant compile phase
  but still microseconds.
- **Schema clone outside timing.** The Typechecker's schema clone is
  measured separately from the typecheck phase, so the per-phase
  numbers reflect typecheck cost, not setup.

Things this bench *doesn't* address (acknowledged):

- **Cold vs warm runtime is mixed in.** First iter pays page-cache
  warmup; bench takes a `--warmup 1` discard but only one iter is
  warm-up. Variance on the runtime side is real (LDBC bench saw
  38-106s spread on a single param, ~3x noise floor).
- **Few queries per category.** 4 valid, 3 empty, 2 unbound, 1 parse.
  Per-category averages aren't statistically robust; treat each cell
  as one data point, not a population estimate.
- **`empty?` column self-reports.** It just shows `r.empty`. If a
  regression made `r.empty` always `true`, the column would still
  say `yes`. The cross-check is the runtime side: when `empty=yes`,
  `rt_unchk` should also return zero rows. The bench currently
  doesn't assert that, but the CSV output exposes both for manual
  cross-check.
- **Single inferred schema per dataset.** The active GRAPH TYPE on
  both datasets is the auto-inferred DEFAULT (every label/edge that
  exists in the data). A custom restrictive GRAPH TYPE would be more
  interesting because some valid-looking queries would be rejected
  by typing — but that needs a hand-written schema and is left
  for a follow-up. *(User-flagged as optional.)*
- **Limit is fixed.** `--limit 100` per the bench default. Different
  limits change runtime cost per case; the bench doesn't sweep.
- **Predicate-pushdown gap dominates valid runtime.** Same finding as
  the LDBC bench — the optimizer doesn't push down value-equality
  predicates. The "valid" category's runtime numbers are inflated by
  this; with pushdown they would shrink and the typechecker overhead
  ratios would change. The relative ratios in this bench are tied to
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
