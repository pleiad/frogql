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

On the first invocation, `typecheck_bench` also auto-builds an
`ldbc-tiny.gdb` fixture by truncating each SF0.1 CSV to 50 lines
(~840× smaller, ~390 nodes). Same loader, strict subset of the
schema. Cached on disk afterward — subsequent runs reuse it.

No flags needed. `--iters N` (default 30) and `--warmup N` (default
1) are available if you're tuning iteration count.

The bench is paired with the LDBC dataset by design — it doesn't
accept arbitrary `.gdb` paths because the case set's whole point is
showing the speedup on a **realistic** workload, and that only works
if the schema is the one the cases were authored against.

## What it measures, per case

For each query, per database:

1. **Per-phase compile time.** parse / elaborate / typecheck /
   optimize timed separately. Typecheck includes the per-query
   `Schema::clone()` because production
   (`compile_query_with_diagnostics_with`) pays the same clone on
   every query — under-charging here would understate real cost.
2. **Both runtime paths in one run.**
   - `rt_chk` — runtime *with* the §10 short-circuit honored: when
     the typechecker says `empty` or rejects, runtime is skipped
     (records 0).
   - `rt_unchk` — runtime *without* the typechecker. Always runs
     to completion. This is the "what would have happened without
     the typechecker" baseline.
3. **Verdict** — `ok` / `empty` / `rejected` / `both` / `parse`.
4. **Speedup** = `rt_unchk_med / (parse_med + elab_med + tc_med + opt_med)`.
   Interpretation: how much wall time the typechecker saved by
   firing the short-circuit. For valid (`ok`) cases the column is
   informational (compile vs runtime ratio).

We also record `row_count()` on every unchecked iter as a soundness
cross-check: if the typechecker said `verdict=empty` but the
unchecked runtime returned a non-zero row count, that's a
typechecker bug that would otherwise be quietly logged as a great
speedup. The bench prints a `⚠ SOUNDNESS` warning if it sees one.

## Case set

19 cases across 4 categories. The set is deliberately weighted
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

### Invalid (unbound variable) on expensive shapes (5 cases)

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

### Parse error (1 case)

- `i_parse` — `MATCH (p RETURN p.name`. Caught before any later
  phase fires.

## Sample results (SF0.1, 30 iters / 1 warmup)

Captured from a real run; absolute numbers will vary by machine but
the orders of magnitude are stable.

```
cat              case                          parse_us   elab_us     tc_us    opt_us  rt_chk_ms rt_unchk_ms   verdict    speedup
------------------------------------------------------------------------------------------------------------------------------------
valid            v_label                          23.70      2.80     95.80      1.00      19.26       21.99        ok     178.4x
valid            v_chain_knows                    27.70      3.90    198.20      2.00    1516.91     1540.73        ok    6646.8x
valid            v_where                          26.30      2.70    101.40      4.10      19.10       17.51        ok     130.2x
empty            e_chain4_bad_leaf                72.90      8.00    354.00      3.30       0.00    66581.88     empty  151944.0x
empty            e_chain_mid_bad                  36.30      5.80    302.60      2.90       0.00     1735.36     empty    4992.4x
empty            e_bad_edge_deep                  40.80      4.80    191.40      3.10       0.00     1605.53     empty    6686.9x
empty            e_ic2_bad_msg                    39.80      6.60    293.30      2.20       0.00     6378.00     empty   18654.6x
empty            e_conflict_label_deep            49.00      5.30    278.70      2.80       0.00     1630.27     empty    4854.9x
empty            e_type_mismatch_chain          1473.50    804.10   3611.30     12.10       0.00    16786.70     empty    2844.7x
empty            e_union_all_bad                  36.90      4.00    194.10      2.90       0.00     2711.88     empty   11399.2x
empty            e_repeat_bad_leaf                51.20      6.40    264.20      2.90       0.00    16826.30     empty   51821.1x
empty            e_label_only                     22.10      2.40     60.20      0.70       0.00      993.10     empty   11628.8x
invalid_unbound  i_unbound_after_chain4           33.20      8.30    358.00      3.60       0.00     1337.75  rejected    3318.7x
invalid_unbound  i_unbound_in_where_chain         38.50      6.10    288.20      5.50       0.00     8204.10      both   24251.0x
invalid_unbound  i_unbound_in_union               33.20      5.00    267.40      2.80       0.00     3342.80  rejected   10839.2x
invalid_unbound  i_unbound_compound_where         35.70      3.40    121.10      5.50       0.00       22.54      both     136.0x
invalid_unbound  i_unbound_simple                 29.80      3.90    191.10      0.90       0.00     1494.21  rejected    6620.3x
invalid_parse    i_parse                           5.90         —         —         —          —           —     parse          —
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
- `verdict` — `ok` / `empty` / `rejected` / `both` / `parse` (see
  list at the top of "What it measures").
- `speedup` = `rt_unchk / (parse + elab + tc + opt)`. Meaningful
  whenever the short-circuit fires; informational for `ok` cases.

### Key takeaways from the captured run

- **Doomed queries dominate the speedup story.** The
  empty/rejected cases land at 2,800× to 152,000× — a 4-hop chain
  ending in an unknown label is 66 seconds the runtime would have
  spent vs 354 µs the typechecker spends to know it's hopeless.
- **Valid-case overhead is negligible.** Typecheck on
  `v_chain_knows` adds ~200 µs to a query whose runtime is 1.5
  seconds — 0.013% of wall time.
- **Small empty cases also win.** `e_label_only` (just `(x: Wagumi)`)
  still gets ~12,000× because the runtime takes ~1 second to
  confirm there are no Wagumi nodes in 327k.
- **Soundness held.** Across all 19 cases × 2 DBs, no
  `verdict=empty` row had a non-zero unchecked row count.

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
