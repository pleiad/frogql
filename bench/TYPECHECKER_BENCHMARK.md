# Typechecker Benchmark — How to Run

Compares wall time on an LDBC SF0.1 case set with vs without the
typechecker. The typechecker can short-circuit doomed queries
(empty by typing, unbound variable, type mismatch) before the
runtime ever runs; this bench measures both paths and reports
the difference.

## Setup

```bash
cargo build --release --bin gqlite --bin bench_setup --bin typecheck_bench
./target/release/bench_setup
```

Same `bench_setup` as the LDBC bench — fetches
`social_network-sf0.1-CsvBasic-LongDateFormatter.tar.zst` (17 MiB),
unpacks, and runs `gqlite --import-ldbc-csv` to produce
`bench/data/ldbc-sf0.1.gdb` (~1.4 GiB). Idempotent; re-running with
the .gdb already present is a no-op.

The bench is paired with that exact dataset. It opens
`bench/data/ldbc-sf0.1.gdb` from a hardcoded path and the case set
references LDBC labels (`Person`, `:knows`, `:hasCreator`, …); it
does not accept arbitrary `.gdb` paths.

## Run

```bash
# Default: 3 iters per case, 1 warmup iter dropped (~10–15 min wall):
./target/release/typecheck_bench

# Capture both streams separately:
./target/release/typecheck_bench > tc_data.csv 2> tc_summary.txt

# More statistical confidence (slow — slowest doomed case is ~50s/iter
# on the unchecked path, so 30 iters is a ~1.5h run):
./target/release/typecheck_bench --iters 30 --warmup 3
```

### Flags

| flag | default | meaning |
|---|---|---|
| `--iters N` | `3` | Measured iterations per case |
| `--warmup N` | `1` | Extra iters per case before measurement; their times are discarded |

No other flags. No dataset path argument — see Setup.

## Output

Two streams, same Unix convention as the LDBC bench (stdout = data,
stderr = chatter):

### stdout — one row per phase per iter

```csv
db;category;case;phase;iter;ns;flags
bench/data/ldbc-sf0.1.gdb;valid;v_label;compile_chk;0;145600;
bench/data/ldbc-sf0.1.gdb;valid;v_label;compile_unchk;0;19800;
bench/data/ldbc-sf0.1.gdb;valid;v_label;rt_unchk;0;25310400;rows=1528
...
```

| column | meaning |
|---|---|
| `db` | path to the .gdb |
| `category` | case author's static category: `valid` / `empty` / `invalid` |
| `case` | case id (e.g. `e_chain4_bad_leaf`) |
| `phase` | which of the three phases this row times — see below |
| `iter` | 0-indexed iteration of this case (0 to `--iters - 1` after warmup is dropped) |
| `elapsed_ns` (column 6) | wall time of the call, in **nanoseconds** |
| `flags` | empty / `skipped` / `rows=N` — see below |

`phase` ∈ three values, three rows per case per iter:

| phase | call | meaning |
|---|---|---|
| `compile_chk` | `gqlrust::compile_query_with_diagnostics_with(active, query)` | parse + elab + tc, plus opt iff typechecker accepted |
| `compile_unchk` | `gqlrust::compile_query_unchecked(query)` | parse + elab + opt; no tc |
| `rt_unchk` | `rt.run_query(unchecked_query, --limit)` | runtime on the unchecked path; `0; skipped` only when parse failed |

There's no `rt_chk` phase: on `ok` queries the runtime would
execute identical work to `rt_unchk` whether the typechecker fired
or not, so measuring it twice would just add cache-warming variance
without signal. On doomed queries (`empty` / `rejected`) the §10
short-circuit means production never calls runtime — that's
validated structurally via the `outcome` column.

`flags` vocabulary:

- *empty* — phase ran successfully
- `skipped` — phase didn't run. Only appears on `rt_unchk`, only when parse failed.
- `rows=N` — `rt_unchk` row count. `N=0` means "ran, returned zero rows" (meaningful, distinct from `skipped`); used by the soundness check.

For default `--iters 3` you get `3 × 18 cases × 3 phases = 162` CSV
rows. Use this stream when you want to compute your own statistics —
it's the raw data.

### stderr — human-readable summary

```
=== bench/data/ldbc-sf0.1.gdb ===
  327588 nodes / 1477965 edges in 11.12s
cat        case                         compile_chk_us compile_unchk_us rt_unchk_ms   outcome   tc_impact
  (tc_impact: Yx on empty/rejected = rt_unchk / compile_chk; ±X% on ok = (compile_chk - compile_unchk) / (compile_unchk + rt_unchk); — for parse fail)
---------------------------------------------------------------------------------------------------------
valid      v_label                             182.60           23.40       22.50        ok      +0.71%
...
---------------------------------------------------------------------------------------------------------
Soundness: clean. 18/18 cases produced their expected outcome and no empty case had a non-zero rt_unchk row count.
```

Columns:

| column | meaning |
|---|---|
| `cat` | case author's category: `valid` / `empty` / `invalid` |
| `case` | case id |
| `compile_chk_us` / `compile_unchk_us` | medians of the two compile times, microseconds |
| `rt_unchk_ms` | median of the runtime time on the unchecked path, milliseconds |
| `outcome` | what the typechecker actually decided this run: `ok` / `empty` / `rejected` |
| `tc_impact` | dual format: speedup multiplier `Yx` for empty/rejected, signed percentage `±X%` for ok, `—` for parse fail |

`tc_impact` formulas (also shown in the inline legend):

- empty/rejected: `rt_unchk / compile_chk` (the §10 short-circuit means the user's checked-path cost is just `compile_chk`; without the typechecker they'd pay `compile_unchk + rt_unchk`)
- ok: `(compile_chk − compile_unchk) / (compile_unchk + rt_unchk)` (typecheck overhead as fraction of total wall time)

Sign convention on `ok` rows: `+X%` is overhead added by the
typechecker; `-X%` only happens if `compile_chk < compile_unchk`,
which is rare and reflects µs-scale variance on the compile
measurement.

### Soundness summary

Last line of stderr counts cases that tripped a warning. Two flavors:

- **outcome mismatch** — the typechecker's actual outcome doesn't
  match the case author's expected category. A regression in the
  typechecker, parser, or schema inference.
- **empty-but-nonempty** — the typechecker said `empty` but the
  unchecked runtime returned a non-zero row count. The §10
  short-circuit would have discarded real results.

Clean run reads `Soundness: clean. 18/18 ...`. Anything else is a
regression — investigate before trusting the rest of the table.

### Cross-checking the streams

The summary `compile_chk_us` for a case is the median of the
`phase=compile_chk` ns values divided by 1000. For `valid v_label`:

```
compile_chk;0;145600;
compile_chk;1;182600;
compile_chk;2;195800;
...
```

median of those (in ns) ÷ 1000 = the `compile_chk_us` column.

If you only need the headline, read the `.txt`. The CSV is there
for when you need raw data.

## Case set

18 cases across 3 categories (`bench/TYPECHECKER_BENCHMARK.md`'s only
source of truth is `src/bin/typecheck_bench.rs::CASES`).

**Valid (3)** — both paths run; controls for typecheck overhead.
- `v_label`, `v_chain_knows`, `v_where`

**Empty by typing (9)** — typechecker says guaranteed-empty.
- `e_chain4_bad_leaf` — 4-hop knows chain to unknown leaf label
- `e_chain_mid_bad` — unknown label mid-chain
- `e_bad_edge_deep` — unknown edge label deep in chain
- `e_ic2_bad_msg` — IC2 shape (friend-of-friend → message) with bad message label
- `e_conflict_label_deep` — same var bound to incompatible labels (`Person ∩ Comment = ⊥`)
- `e_type_mismatch_chain` — str/int mismatch in WHERE on a 3-hop pattern
- `e_union_all_bad` — path-pattern union with every arm empty
- `e_repeat_bad_leaf` — bounded repetition `{1,2}` to unknown label
- `e_label_only` — single label not in schema (small-pattern control)

**Invalid (6)** — compile pipeline rejects.
- `i_unbound_after_chain4` — free var in RETURN after 4-hop chain
- `i_unbound_in_where_chain` — free var in WHERE on multi-hop pattern
- `i_unbound_in_union` — free var in projection on union pattern
- `i_unbound_compound_where` — free var alongside valid filter via AND
- `i_unbound_simple` — free var in RETURN on bare match
- `i_parse` — parse error (`MATCH (p RETURN p.name`)

## See also

- `src/bin/typecheck_bench.rs` — the runner; the rustdoc header
  has a brief usage reference.
- `bench/LDBC_BENCHMARK.md` — companion bench (LDBC SNB Interactive)
  using the same dataset and `bench_setup`.
- The paper — for results, findings, and analysis.
