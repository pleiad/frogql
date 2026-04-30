# Typechecker Benchmark

Demonstrates that the typechecker rejects doomed queries in
microseconds while the runtime — given the same query without the
typechecker — burns milliseconds to seconds enumerating doomed work
before discovering the same fact.

## Setup

Two commands. The first downloads the LDBC SNB SF0.1 dataset (~17
MiB, ~327k nodes / ~1.48M edges) and builds it into a `.gdb` once.
The second runs the bench:

```bash
cargo build --release
./target/release/bench_setup        # downloads + builds ldbc-sf0.1.gdb
./target/release/typecheck_bench    # runs the bench
```

No flags needed. `--iters N` (default 30) and `--warmup N` (default
1) are available if you're tuning iteration count.

(An earlier version also auto-truncated SF0.1 to a `ldbc-tiny.gdb`
fixture and ran every case against both. Dropped: the dataset-size
axis it demonstrated — typecheck cost is schema-bound, runtime cost
data-bound — is theoretically obvious, and the truncation strategy
was fragile because too aggressive a `head -N` dropped sparse edge
types from the inferred schema. SF0.1 alone makes the headline.)

The bench is paired with the LDBC dataset by design — it doesn't
accept arbitrary `.gdb` paths because the case set's whole point is
showing the speedup on a **realistic** workload, and that only works
if the schema is the one the cases were authored against.

## What it measures, per case

For each query:

1. **Two compile-pipeline times.** The bench invokes production's
   entry points wholesale rather than reimplementing the pipeline
   phase-by-phase, so it can't drift from production semantics:
   - `compile_chk_us` — `compile_query_with_diagnostics_with(active, query)`,
     the production checked path. Bails before optimize when the
     typechecker rejects (no Query produced); runs the full pipeline
     otherwise.
   - `compile_unchk_us` — `compile_query_unchecked(query)`, the
     "what production would do without the typechecker" baseline.
     Runs parse + elab + opt; no tc.
2. **Both runtime paths in one run.**
   - `rt_chk` — runtime on the checked-path Query. Skipped if the
     compile rejected the query, or if `guaranteed_empty` fired the
     §10 short-circuit (records 0).
   - `rt_unchk` — runtime on the unchecked-path Query. Always runs
     unless parse itself failed.
3. **Outcome** — three buckets derived structurally from the
   compile result: `ok` (CompileResult with !guaranteed_empty),
   `empty` (CompileResult with guaranteed_empty), `rejected`
   (CompileError of either Type or Parse variant). The
   "rejected AND empty" intersection collapses into `rejected`
   because a rejected query is invalid regardless of whether the
   residual pattern was unsatisfiable.
4. **tc_impact** — split format depending on outcome:
   - `empty` / `rejected`: speedup multiplier
     `rt_unchk / compile_chk`. Numbers are 10³ to 10⁵× — a
     percentage saturates near 100% and loses magnitude.
   - `ok`: signed percentage of total wall time delta:
     `(compile_chk + rt_chk - compile_unchk - rt_unchk) / (compile_unchk + rt_unchk)`.
     `+X%` is overhead (expected); `-X%` is only reachable from
     runtime variance.
   - parse failure: `—` (no compile pipeline to compare).

The bench cross-checks soundness on every iter: if the typechecker
asserted `empty` but the unchecked runtime returned a non-zero row
count, the §10 short-circuit would have discarded real results.
The bench prints a `⚠ SOUNDNESS` warning per offending case and a
tail count of how many cases tripped warnings (either an outcome
mismatch vs the case's expected category, or empty-but-nonempty).
On a clean run the tail reads `Soundness: clean. N/N cases
produced their expected outcome ...`.

## Case set

18 cases across 3 categories. The set is deliberately weighted
toward "doomed query, expensive would-be runtime" — that's where
the typechecker most clearly justifies itself.

### Valid controls (3 cases)

Single-hop, multi-hop, and WHERE-pinned. These exist purely to
show typecheck cost is negligible vs the runtime even when both
succeed.

- `v_label` — `MATCH (p: Person) RETURN p.firstName`
- `v_chain_knows` — `MATCH (p: Person)~[:knows]~(f: Person) RETURN f.firstName`
- `v_where` — `MATCH (p: Person) WHERE p.id = 933 RETURN p.firstName`

### Empty by typing (9 cases)

Mostly multi-hop / realistic-shape patterns where one element
makes the static analysis bottom out — the runtime, without
typecheck, has to do the enumeration to find out. One small-
pattern control (`e_label_only`) at the end of the bucket gives
a same-bucket reference for cases where the runtime can rule
out emptiness cheaply.

- `e_chain4_bad_leaf` — 4-hop knows chain ending in unknown label
  (`Wagumi`). Runtime walks friend-of-friend³ before discovering
  the label has no nodes.
- `e_chain_mid_bad` — unknown label in the middle of a chain.
- `e_bad_edge_deep` — unknown edge label deep in a chain.
- `e_ic2_bad_msg` — LDBC IC2 shape (friend-of-friend → message),
  message label set to garbage.
- `e_conflict_label_deep` — same variable bound twice with
  incompatible labels (`Person ∩ Comment = ⊥`). LabelType-level
  emptiness, distinct from unknown-label.
- `e_type_mismatch_chain` — type mismatch in WHERE on a 3-hop
  pattern (str field compared to int literal → filter type ⊥).
- `e_union_all_bad` — path-pattern union where every arm is empty.
- `e_repeat_bad_leaf` — bounded repetition `{1,2}` ending in
  unknown label. The bound is `{1,2}` rather than the spec-faithful
  `{1,3}` of LDBC IC1 because `{1,3}` OOMs the unchecked runtime
  on SF0.1 today (tracked separately as a runtime perf issue);
  typecheck cost is identical between bounds.
- `e_label_only` — single label not in schema. The simplest
  empty-by-typing case, included as a small-pattern control inside
  this bucket so we can see how the speedup varies with pattern size.

### Invalid — rejected by the compile pipeline (6 cases)

The unchecked runtime runs the whole pattern, projects the free
variable as NULL — wrong-but-not-empty. Typechecker rejects in
microseconds.

- `i_unbound_after_chain4` — free var in RETURN after a 4-hop chain.
- `i_unbound_in_where_chain` — free var in WHERE on a multi-hop
  pattern (different code path than RETURN-clause resolution).
- `i_unbound_in_union` — free var in projection on a union pattern.
- `i_unbound_compound_where` — free var alongside a valid filter
  via AND. Tests free-var detection survives compound predicates.
- `i_unbound_simple` — short reproduction of the original
  wrong-NULL bug, kept as a small-pattern baseline within this bucket.

The 6th case in this category is `i_parse` — `MATCH (p RETURN p.name`,
caught at parse time. It collapses into the same `Outcome::Rejected`
bucket as the unbound-var cases since both are "the compile pipeline
refused this query".

## Sample shape (run the bench for fresh numbers)

Below is a shape-of-output illustration from a smoke run (n=2, no
warmup — *not* statistically robust; the per-row numbers are noisy).
It shows the column layout under the current refactor (where the
bench calls production's `compile_query_with_diagnostics_with` and
`compile_query_unchecked` wholesale rather than timing per-phase).
Run `./target/release/typecheck_bench` yourself for solid medians.

```
cat        case                         compile_chk_us compile_unchk_us  rt_chk_ms rt_unchk_ms   outcome   tc_impact
--------------------------------------------------------------------------------------------------------------------
valid      v_label                             991.50           21.45      28.52       20.11        ok     +46.59%
valid      v_chain_knows                       286.50           26.70    2089.89     1943.33        ok      +7.55%
valid      v_where                             173.45           41.80      14.53       14.51        ok      +1.06%
empty      e_chain4_bad_leaf                   452.00           17.15       0.00    74256.23     empty   164283.7x
empty      e_chain_mid_bad                    4238.90           24.15       0.00     2168.03     empty      511.5x
empty      e_bad_edge_deep                     411.40           21.75       0.00     1769.79     empty     4301.9x
empty      e_ic2_bad_msg                      1608.05           17.80       0.00    12296.76     empty     7647.0x
empty      e_conflict_label_deep               451.05           16.20       0.00     1832.54     empty     4062.8x
empty      e_type_mismatch_chain              1449.30           24.55       0.00     3710.01     empty     2559.9x
empty      e_union_all_bad                     394.60           21.95       0.00     3703.33     empty     9385.0x
empty      e_repeat_bad_leaf                  1712.40           19.05       0.00    20100.50     empty    11738.2x
empty      e_label_only                        150.55            6.80       0.00     2075.61     empty    13786.9x
invalid    i_unbound_after_chain4             1774.95           24.50       0.00     1878.39  rejected     1058.3x
invalid    i_unbound_in_where_chain           1105.50           23.95       0.00    10891.39  rejected     9852.0x
invalid    i_unbound_in_union                  292.15           11.55       0.00     3978.76  rejected    13618.9x
invalid    i_unbound_compound_where             97.70          336.40       0.00        8.64  rejected       88.4x
invalid    i_unbound_simple                    363.70            8.85       0.00     1380.38  rejected     3795.4x
invalid    i_parse                                  —               —          —           —  rejected           —
```

Notes on this snapshot:
- `n=2` so per-row noise is large (`v_label`'s `compile_chk_us=991.50`
  is a cold-cache outlier; `compile_chk` for most cases on real n=30
  runs settles in the 100-400 µs range).
- `i_unbound_compound_where` is the smallest gap (`88.4×`) and
  measures correctly: pushdown lets the runtime defend itself well
  on this case, so the typechecker's relative win shrinks (but is
  still two orders of magnitude).
- All other doomed cases land at 10³ to 10⁵× as expected.
- `i_unbound_compound_where` shows compile_chk (97.70) < compile_unchk
  (336.40) — that's the early-bail effect: compile_chk rejects mid-
  pipeline (no opt), compile_unchk runs the whole no-tc pipeline
  including opt. So the checked path is *cheaper* in compile, and
  the runtime saving is bonus.

### Reading the table

- `compile_chk_us` — production checked-pipeline cost (parse + elab
  + tc, plus opt iff the typechecker accepted), microseconds.
- `compile_unchk_us` — same pipeline minus the typechecker, the
  no-typechecker baseline cost, microseconds.
- `rt_chk_ms` — runtime *with* the §10 short-circuit honored.
  Zero where the typechecker fired the short-circuit.
- `rt_unchk_ms` — runtime *without* the typechecker, milliseconds.
  This is the "would-be" cost the speedup compares against.
- `outcome` — `ok` / `empty` / `rejected`. Three buckets derived
  structurally from the compile result (see §3).
- `tc_impact` — split format:
  - `empty` / `rejected` rows: speedup multiplier
    `rt_unchk / compile_chk`.
  - `ok` rows: signed % of total-wall-time delta with vs without
    the typechecker (`+X%` overhead, `-X%` only reachable from
    runtime variance).
  - parse failure: `—`.

### What you should expect to see

- **Doomed queries dominate the speedup story.** Empty/rejected cases
  land in the 10³ to 10⁵× range. The biggest win is `e_chain4_bad_leaf`
  (4-hop chain to an unknown label) where the runtime spends ~50–70
  seconds to discover what the typechecker rejects in a few hundred µs.
- **Valid-case impact lives in the noise.** `compile_chk + rt_chk` is
  comparable to `compile_unchk + rt_unchk`; the `tc_impact` percentage
  on `ok` rows tends to come in within a few percent of zero, with
  sign determined by per-iter variance. The typechecker can't actually
  make a query it doesn't reject run faster — large negative values
  are runtime-cache asymmetry, not real speedup.
- **Small empty cases also win.** `e_label_only` (just
  `(x: Wagumi)`) still gets ~10⁴× because confirming "no Wagumi
  exists" still costs the runtime ~1 second of label-index work.
- **Smallest invalid speedup is the canary.** `i_unbound_compound_where`
  is where the runtime defends itself best (pushdown narrows the
  predicate to one person quickly). Expect ~10² × there. If that
  number ever drops near 1×, the case has lost signal value; replace it.
- **Soundness should be `clean. 18/18`.** Anything else is a
  regression to investigate before trusting the rest of the table.

## Output

- **stdout**: per-iteration CSV `db;category;case;phase;iter;ns;flags`
  for offline analysis (compute min/p95/CI, plot distributions, etc.).
  `phase` ∈ {`compile_chk`, `rt_chk`, `compile_unchk`, `rt_unchk`} —
  one row per phase per iter. Notes:
  - `category` is the case author's static category (`valid`,
    `empty`, `invalid`); the runtime `outcome` (in the stderr table)
    is computed structurally and may diverge if there's a regression.
  - `flags` ∈ {`""`, `"skipped"`, `"rows=N"`}: empty for normal
    rows; `skipped` on `rt_chk` when the §10 short-circuit fired or
    the compile rejected, and on `rt_unchk` only when parse itself
    failed; `rows=N` on every `rt_unchk` row that actually ran
    (N=0 is meaningful — "ran, got 0 rows" — and distinct from
    `skipped`). Used for the empty-but-nonempty soundness check.
- **stderr**: human-readable summary table + soundness warnings.

Redirect them separately:

```bash
./target/release/typecheck_bench > tc_data.csv 2> tc_summary.txt
```

## What this bench does NOT do

- **No isolated typechecker microbench.** An earlier version had a
  criterion harness (`benches/typecheck.rs`) for compile-only
  numbers with statistical CIs. Removed: it couldn't make the
  bench's actual claim (faster than would-be runtime) without a
  runtime to compare against. If we ever need regression detection
  on typecheck-only cost, computing min/median/p95 from the
  per-iter CSV is ~30 lines of post-processing.
- **No `.gdb` flexibility.** Cases reference LDBC labels by name
  (`Person`, `:knows`, `:hasCreator`, `Comment`, …); pointing the
  bench at a different schema would silently turn most cases into
  the wrong measurement. Schema-flexible typecheck testing is the
  job of unit tests, not this bench.
- **No CI integration.** Bench machines are noisy; per-developer
  baseline-comparison is the right shape if we ever want regression
  detection, not CI thresholds.
