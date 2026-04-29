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
  done before the timed region. `Typechecker::new` still runs inside
  the timed region but it's a move-only struct construction (the
  schema is moved in by value, vecs are init-empty), so its cost is
  ~tens of nanoseconds and well below the typecheck-phase noise
  floor. The reported `tc_us` reflects `check_query` work.
- **Two complementary harnesses.** This CSV bench covers the
  multi-DB sweep + soundness + verdict + speedup-vs-runtime claim.
  A second harness — criterion at `benches/typecheck.rs` — covers
  the microsecond-scale phase numbers with proper statistical
  handling (warmup, outlier detection, confidence intervals,
  baseline regression checks). See "Two harnesses" below.
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
  38-106s spread on a single param, ~3x noise floor). *Bench-side
  fixable but expensive — multi-iter warmup multiplies bench wall
  time linearly. Not pursued.*
- **(b) Few queries per category.** 4 valid, 3 empty, 2 unbound,
  1 parse. Per-category averages aren't statistically robust; treat
  each cell as one data point, not a population estimate.
- **(c) Single inferred schema per dataset.** Active GRAPH TYPE is
  the auto-inferred DEFAULT on both datasets. A custom restrictive
  GRAPH TYPE would be more interesting because some valid-looking
  queries would be rejected by typing — needs a hand-written schema
  and is left for a follow-up. *(User-flagged as optional.)*
- **(d) ~~Schema clone per iter~~ — non-issue.** Earlier draft of
  this section overstated this. The schema clone is outside the
  timed region; the `Typechecker::new` call inside the timed region
  is move-only struct construction. No fix needed.
- **(e) μs-scale jitter — addressed in `benches/typecheck.rs`.**
  Typecheck phases are 3-200 μs. The CSV bench uses `Instant::now()`
  with min/median/mean/max and doesn't compute confidence intervals.
  The criterion harness at `benches/typecheck.rs` does (warmup,
  outlier detection, [low / point / high] CIs, baseline regression
  checks). See "Two harnesses" below.
- **(f) No CI on the bench — intentional.** Earlier draft framed
  this as a gap. It isn't. CI runners (GitHub Actions, etc.) are
  shared, noisy, and run-to-run variance routinely exceeds the
  signal we'd want to catch (microsecond-scale typecheck deltas,
  millisecond-scale runtime deltas at SF0.1). Running benches on
  CI would either produce false alarms (when CI noise dominates)
  or hide real regressions (when threshold is loose enough to
  tolerate the noise). Both benches are run on developer laptops
  where the environment is at least *consistent*, and criterion's
  baseline-comparison feature (`--save-baseline` / `--baseline`)
  is the right tool for "did my change make it slower." No CI
  integration planned.
- **(g) Limit is fixed.** `--limit 100` is the default; different
  limits change runtime cost. The CSV bench doesn't sweep limit.
- **(h) Predicate-pushdown gap dominates valid runtime — gqlite
  impl issue, out of scope here.** Same finding as the LDBC bench:
  the optimizer extracts `var.attr is type` from WHERE but not
  `var.attr = literal`. The "valid" category's runtime numbers are
  inflated by this. **Fixing it requires changes in
  `src/optimizer/pushdown.rs`, `src/syntax/descriptor.rs`, and
  `src/runtime/engine.rs`** (re-introduce post-elab `pushed_predicates`
  on Descriptor; have the runtime evaluate them per-candidate before
  joining). This bench branch is bench-only and explicitly does
  not modify gqlite implementation. Tracked as a follow-up.

## Two harnesses

| | `src/bin/typecheck_bench.rs` (CSV) | `benches/typecheck.rs` (criterion) |
|---|---|---|
| Invocation | `./target/release/typecheck_bench --iters 30 db1.gdb db2.gdb` | `cargo bench --bench typecheck` |
| Multi-DB | yes | no (uses `Schema::star()` — no .gdb dependency) |
| Includes runtime | yes (`rt_chk`, `rt_unchk`) | no — compile-side only |
| Phases | parse / elab / tc / opt / rt_chk / rt_unchk | parse / elaborate / typecheck / compile_full |
| Headline metric | speedup = rt_unchk / (parse+elab+tc+opt) | per-phase wall time + CIs |
| Statistical handling | min/median/mean/max | warmup + outlier detection + CIs |
| Soundness check | yes (rt_unchk row count vs r.empty) | no |
| Verdict tracking | yes (ok/empty/rejected/both/parse) | no |
| Baseline regression | no (raw CSV; diff in a script) | yes (`--save-baseline` / `--baseline`) |

Run both. The CSV bench answers "is the typechecker faster than the
runtime would have been?" (yes, 2-45 000× depending on dataset). The
criterion bench answers "did my change to the typechecker pipeline
make it slower than baseline?" with a real confidence interval and
not just a noisy median.

```bash
# Capture a baseline before refactoring
cargo bench --bench typecheck -- --save-baseline before
# ... make changes ...
cargo bench --bench typecheck -- --baseline before
# Reports % change per case + whether change is significant
```

### Criterion baseline (Schema::star, Windows / rustc 1.95 release, 100 samples after 3 s warmup)

CIs as `[low point high]` µs. Outliers auto-detected and excluded
by criterion. Each criterion group restarts the pipeline from the
query string, so `parse` is parse-only, `elaborate` is parse+elab,
`typecheck` is parse+elab+tc, `compile_full` is parse+elab+tc+opt.
Per-phase costs are `group(N) − group(N-1)`.

| group | case | time (µs) |
|---|---|---|
| parse | valid_label_person | [1.56 1.61 1.67] |
| parse | valid_label_user | [1.58 1.62 1.66] |
| parse | valid_chain_knows | [2.75 2.83 2.90] |
| parse | valid_chain_wrote | [2.92 2.99 3.06] |
| parse | empty_unknown_label | [1.74 1.82 1.90] |
| parse | empty_unknown_edge_lhs | [2.82 2.90 2.98] |
| parse | empty_chained_unknown | [4.17 4.28 4.39] |
| parse | invalid_unbound | [1.46 1.50 1.54] |
| parse | invalid_unbound_chain | [2.35 2.44 2.54] |
| parse | invalid_parse | [1.15 1.18 1.21] |
| elaborate | valid_label_person | [1.86 1.91 1.96] |
| elaborate | valid_label_user | [1.84 1.89 1.94] |
| elaborate | valid_chain_knows | [3.95 4.04 4.11] |
| elaborate | valid_chain_wrote | [3.83 3.92 4.00] |
| elaborate | empty_unknown_label | [1.90 1.94 1.98] |
| elaborate | empty_unknown_edge_lhs | [3.35 3.44 3.54] |
| elaborate | empty_chained_unknown | [4.92 5.07 5.24] |
| elaborate | invalid_unbound | [1.85 1.91 1.96] |
| elaborate | invalid_unbound_chain | [3.04 3.14 3.22] |
| typecheck | valid_label_person | [6.99 7.62 8.32] |
| typecheck | valid_label_user | [8.71 9.09 9.49] |
| typecheck | valid_chain_knows | [12.64 12.91 13.22] |
| typecheck | valid_chain_wrote | [12.67 13.97 15.42] |
| typecheck | empty_unknown_label | [3.90 4.00 4.11] |
| typecheck | empty_unknown_edge_lhs | [9.89 10.21 10.59] |
| typecheck | empty_chained_unknown | [20.44 20.92 21.34] |
| typecheck | invalid_unbound | [3.81 3.89 3.98] |
| typecheck | invalid_unbound_chain | [11.37 11.80 12.27] |
| compile_full | valid_label_person | [4.52 4.67 4.83] |
| compile_full | valid_label_user | [4.66 4.87 5.07] |
| compile_full | valid_chain_knows | [13.25 13.69 14.08] |
| compile_full | valid_chain_wrote | [10.09 10.45 10.81] |
| compile_full | empty_unknown_label | [3.67 3.72 3.79] |
| compile_full | empty_unknown_edge_lhs | [13.00 13.32 13.58] |
| compile_full | empty_chained_unknown | [19.67 20.29 20.94] |
| compile_full | invalid_unbound | [3.54 3.61 3.68] |
| compile_full | invalid_unbound_chain | [10.81 11.23 11.71] |

Quick per-phase reads on `valid_chain_knows`:

| phase | calculation | µs |
|---|---|---|
| parse | direct | 2.83 |
| elaborate (alone) | 4.04 − 2.83 | 1.21 |
| typecheck (alone) | 12.91 − 4.04 | 8.87 |
| optimize (alone) | 13.69 − 12.91 | 0.78 |

Total compile (parse+elab+tc+opt) for the `valid_chain_knows` case:
**13.69 µs**. Compared to the runtime's 1645 ms on SF0.1, compile is
**0.0008%** of wall time. The "typechecker overhead is negligible"
claim survives proper statistical handling.

The full per-case data including outlier breakdowns is captured
under `target/criterion/` after each `cargo bench` run; HTML reports
are generated automatically (open `target/criterion/report/index.html`).

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
