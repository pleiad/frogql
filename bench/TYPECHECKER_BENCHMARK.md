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

1. **Per-phase compile time.** parse / elaborate / typecheck /
   optimize timed separately. Typecheck includes the per-query
   `Schema::clone()` because production
   (`compile_query_with_diagnostics_with`) pays the same clone on
   every query — under-charging here would understate real cost.
2. **Both runtime paths in one run.**
   - `rt_chk` — runtime *with* the §10 short-circuit honored: when
     the typechecker says `Empty` or rejects, runtime is skipped
     (records 0).
   - `rt_unchk` — runtime *without* the typechecker. Always runs
     to completion. The "what would have happened without the
     typechecker" baseline.
3. **Outcome** — three buckets derived from the typechecker's
   booleans: `ok` (valid, runs), `empty` (statically empty,
   short-circuited), `rejected` (typecheck error or parse error;
   the compiler refused this query). The "rejected AND empty"
   intersection collapses into `rejected` because a rejected
   query is invalid regardless of whether the residual pattern
   was unsatisfiable.
4. **tc_impact** — split format depending on outcome:
   - `ok` cases: signed percentage `±X%` of total wall time vs
     the no-typechecker path. `+X%` is overhead (expected); `-X%`
     is only reachable from runtime variance.
   - `empty` / `rejected`: speedup multiplier
     `rt_unchk / (parse + elab + tc + opt)`. Numbers are 10³ to
     10⁵× — a percentage saturates near 100% and loses magnitude.
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

### Empty by typing on expensive shapes (9 cases)

Each is a multi-hop or otherwise non-trivial pattern with one
element that makes the static analysis bottom out. The runtime
without the typechecker doesn't necessarily detect the same fact
until it has done substantial enumeration.

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

The 5th invalid case is `i_parse` — `MATCH (p RETURN p.name`,
caught at parse time. It collapses into the same `Outcome::Rejected`
bucket as the unbound-var cases since both are "the compile pipeline
refused this query".

## Sample results (SF0.1, 30 iters / 1 warmup)

Captured from a real run; absolute numbers will vary by machine but
the orders of magnitude are stable. tc_impact column reformatted
post-capture: short-circuit cases keep multiplier, valid cases
recomputed as signed % from the same raw timings.

```
cat        case                          parse_us   elab_us     tc_us    opt_us  rt_chk_ms rt_unchk_ms   outcome   tc_impact
-----------------------------------------------------------------------------------------------------------------------------
valid      v_label                          23.70      2.80     95.80      1.00      19.26       21.99        ok     -11.96%
valid      v_chain_knows                    27.70      3.90    198.20      2.00    1516.91     1540.73        ok      -1.53%
valid      v_where                          26.30      2.70    101.40      4.10      19.10       17.51        ok      +9.64%
empty      e_chain4_bad_leaf                72.90      8.00    354.00      3.30       0.00    66581.88     empty   151944.0x
empty      e_chain_mid_bad                  36.30      5.80    302.60      2.90       0.00     1735.36     empty     4992.4x
empty      e_bad_edge_deep                  40.80      4.80    191.40      3.10       0.00     1605.53     empty     6686.9x
empty      e_ic2_bad_msg                    39.80      6.60    293.30      2.20       0.00     6378.00     empty    18654.6x
empty      e_conflict_label_deep            49.00      5.30    278.70      2.80       0.00     1630.27     empty     4854.9x
empty      e_type_mismatch_chain          1473.50    804.10   3611.30     12.10       0.00    16786.70     empty     2844.7x
empty      e_union_all_bad                  36.90      4.00    194.10      2.90       0.00     2711.88     empty    11399.2x
empty      e_repeat_bad_leaf                51.20      6.40    264.20      2.90       0.00    16826.30     empty    51821.1x
empty      e_label_only                     22.10      2.40     60.20      0.70       0.00      993.10     empty    11628.8x
invalid    i_unbound_after_chain4           33.20      8.30    358.00      3.60       0.00     1337.75  rejected     3318.7x
invalid    i_unbound_in_where_chain         38.50      6.10    288.20      5.50       0.00     8204.10  rejected    24251.0x
invalid    i_unbound_in_union               33.20      5.00    267.40      2.80       0.00     3342.80  rejected    10839.2x
invalid    i_unbound_compound_where         35.70      3.40    121.10      5.50       0.00       22.54  rejected      136.0x
invalid    i_unbound_simple                 29.80      3.90    191.10      0.90       0.00     1494.21  rejected     6620.3x
invalid    i_parse                           5.90         —         —         —          —           —  rejected           —
```

### Reading the table

- `parse_us` / `elab_us` / `tc_us` / `opt_us` — compile phases in
  **microseconds**.
- `rt_chk_ms` — runtime *with* the §10 short-circuit honored.
  Zero where the typechecker fired the short-circuit.
- `rt_unchk_ms` — runtime *without* the typechecker, in
  **milliseconds**. This is the "would-be" cost the speedup
  compares against. Each iter also captures `row_count()` for the
  soundness cross-check.
- `outcome` — `ok` / `empty` / `rejected`. Three buckets derived
  structurally from the typechecker's booleans (see "What it
  measures" §3).
- `tc_impact` — split format:
  - `ok` rows: signed % delta of total wall time vs the path
    production would take without the typechecker (`+X%` overhead,
    `-X%` only reachable from runtime variance).
  - `empty` / `rejected` rows: speedup multiplier
    `rt_unchk / (parse + elab + tc + opt)`.
  - parse failure: `—`.

### Key takeaways from the captured run

- **Doomed queries dominate the speedup story.** The
  empty/rejected cases land at 136× to 152,000× — a 4-hop chain
  ending in an unknown label is 66 seconds the runtime would have
  spent vs 354 µs the typechecker spends to know it's hopeless.
- **Valid-case impact is in the noise.** v_label and
  v_chain_knows show *negative* tc_impact (–11.96% and –1.53%) —
  not a real speedup, just runtime variance making `rt_chk` happen
  to come in lower than `rt_unchk` on that iter. v_where shows
  +9.64%, which is real overhead. Either way the magnitudes are
  small and consistent with "typecheck cost is noise vs runtime".
- **Small empty cases also win.** `e_label_only` (just `(x: Wagumi)`)
  still gets ~11,000× because the runtime takes ~1 second to
  confirm there are no Wagumi nodes in 327k.
- **Soundness held.** Across all 18 cases, no `empty` outcome had a
  non-zero unchecked row count, and no case's outcome diverged from
  its category-expected outcome.

## Output

- **stdout**: per-iteration CSV `db;category;case;phase;iter;ns;flags`
  for offline analysis (compute min/p95/CI, plot distributions, etc.).
- **stderr**: human-readable per-DB tables + soundness warnings.

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
- **No predicate-pushdown investigation.** The "valid" runtime
  numbers are inflated by a known gqlite optimizer gap (extracts
  `var.attr is type` from WHERE but not `var.attr = literal`). Out
  of scope here; tracked as a follow-up in the optimizer.
- **No CI integration.** Bench machines are noisy; criterion's
  per-developer-baseline workflow is the right shape if we ever
  want regression detection, not CI thresholds.
