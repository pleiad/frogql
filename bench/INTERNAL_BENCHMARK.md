# Internal Benchmark — How to Run

Engine-internal performance characterization for gqlite. Measures
typechecker overhead vs short-circuit benefit, runtime cost across
backends (lazy vs disk), open-time, peak RSS, and on-disk size.
Companion to the **external bench** (`bench/cross-system/`) which
compares gqlite to other graph databases. The internal bench
diagnoses gqlite *to itself* — no external systems involved, no
cross-engine coordination, runs whatever queries make sense for the
question.

## Setup

```bash
cargo build --release --bin gqlite --bin bench_setup --bin internal_bench
./target/release/bench_setup
```

Same `bench_setup` as the cross-system bench — fetches
`social_network-sf0.1-CsvBasic-LongDateFormatter.tar.zst` (17 MiB),
unpacks, and runs `gqlite --import-ldbc-csv` to produce
`bench/data/ldbc-sf0.1.gdb` (~1.4 GiB). Idempotent.

For the optional `--sf 0.3` mode, you must build the SF0.3 .gdb
yourself; `bench_setup` only handles SF0.1 today. The bench errors
cleanly if `bench/data/ldbc-sf0.3.gdb` is missing.

## Run

```bash
# Default: 3 iters per case, 1 warmup, SF0.1, both backends (~30 min wall).
./target/release/internal_bench

# Capture the streams separately:
./target/release/internal_bench > internal.csv 2> internal.txt

# Higher iter count for stable medians (slow — see notes):
./target/release/internal_bench --iters 30 --warmup 3

# SF0.3 instead of SF0.1:
./target/release/internal_bench --sf 0.3
```

### Flags

| flag | default | meaning |
|---|---|---|
| `--iters N` | `3` | Measured iterations per case |
| `--warmup N` | `1` | Extra iters per case before measurement; their times are discarded |
| `--sf 0.1\|0.3` | `0.1` | Scale factor — picks `bench/data/ldbc-sf{N}.gdb` |

The bench runs **both backends in one invocation**: a lazy pass
(`LazyGraphStore`, with secondary indexes + LRU page cache) and a
disk pass (`DiskGraphStore`, no caching layer). Schema is grabbed
once from the lazy store's catalog and reused for both passes
(typechecker doesn't depend on backend).

## Output

Two streams, same Unix convention as the cross-system bench
(stdout = data, stderr = chatter):

### stdout — one row per phase per iter per backend

```csv
backend;db;category;case;phase;iter;ns;flags
lazy;bench/data/ldbc-sf0.1.gdb;valid;v_ic2_spec;compile_chk;0;165500;
lazy;bench/data/ldbc-sf0.1.gdb;valid;v_ic2_spec;compile_unchk;0;21300;
lazy;bench/data/ldbc-sf0.1.gdb;valid;v_ic2_spec;rt_unchk;0;120844400;rows=20
...
disk;bench/data/ldbc-sf0.1.gdb;valid;v_ic2_spec;compile_chk;0;...
```

| column | meaning |
|---|---|
| `backend` | `lazy` or `disk` — which `GraphAccess` backend the runtime ran against |
| `db` | path to the .gdb |
| `category` | case author's static category: `valid` / `empty` / `invalid` |
| `case` | case id (e.g. `v_ic2_spec`) |
| `phase` | `compile_chk` / `compile_unchk` / `rt_unchk` |
| `iter` | 0-indexed iteration after warmup is dropped |
| `ns` (column 7) | wall time of the call, in **nanoseconds** |
| `flags` | empty / `skipped` / `rows=N` |

`phase` ∈ three values per case per iter (same as before; just
reused across backends). The typechecker is backend-independent, so
`compile_chk`/`compile_unchk` rows on the disk pass measure the
same compile work as on the lazy pass — the variance there is just
process / cache state. The interesting cross-backend column is
`rt_unchk`.

For default `--iters 3` you get `3 iters × 25 cases × 3 phases × 2
backends = 450` CSV rows.

### stderr — labeled setup info + per-case table

```
backend;db;category;case;phase;iter;ns;flags
Setup: db_path = bench/data/ldbc-sf0.1.gdb
       db_size = 1423.1 MiB (1492238336 bytes)
       rss_baseline = 8.2 MiB

=== lazy: bench/data/ldbc-sf0.1.gdb ===
  open_time     = 2.14s
  graph         = 327588 nodes / 1477965 edges
  rss_after_open = 295.6 MiB (+287.4 over baseline)
cat        case                         compile_chk_us compile_unchk_us rt_unchk_ms   outcome   tc_impact
... 25 case rows ...
  peak_rss_loop = 432.1 MiB (+423.9 over baseline)

=== disk: bench/data/ldbc-sf0.1.gdb ===
  open_time     = 1.20s
  rss_after_open = 60.0 MiB (+51.8 over baseline)
... 25 case rows ...
  peak_rss_loop = 295.0 MiB (+286.8 over baseline)
```

The `Setup` block runs once. Each backend gets:
- `open_time` — wall time of `<Store>::open()`
- `rss_after_open` — process RSS just after open, before any queries
- `peak_rss_loop` — highest RSS observed across the per-case loop
- `db_size` — on-disk .gdb size (same value for both backends; printed once at top)

These are 4 numbers per backend; reading them off stderr is fine.
They're **not** in the CSV — keeping the CSV purely per-case.

The per-case table format is the same as before, with `tc_impact`
formulas listed inline. See "tc_impact" below.

### tc_impact

| outcome | formula | meaning |
|---|---|---|
| `empty` / `rejected` | `(compile_unchk + rt_unchk) / compile_chk` | speedup ratio: how much wall-time the typechecker's short-circuit saved by skipping the runtime |
| `ok` | `(compile_chk - compile_unchk) / (compile_unchk + rt_unchk)` | typechecker overhead as a fraction of total runtime work |
| (parse fail) | `—` | nothing meaningful to compute |

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
  unchecked runtime returned a non-zero row count. The
  guaranteed-empty short-circuit would have discarded real results.

Clean run reads `Soundness: clean. 25/25 ...`. Anything else is a
regression — investigate before trusting the rest of the table.

## Case set

25 cases across 3 categories. Source of truth:
`src/bin/internal_bench.rs::CASES`.

**Valid (9)** — both paths run; controls for typecheck overhead.

| id | shape | what it tests |
|---|---|---|
| `v_label` | single label scan | baseline |
| `v_chain_knows` | undirected 1-hop | basic traversal |
| `v_where` | indexed lookup by id | secondary index hit |
| `v_ic2_spec` | full LDBC IC2 (with COALESCE + ORDER BY) | spec-faithful complex query |
| `v_ic2_noorder` | IC2 without ORDER BY | direct comparison vs `v_ic2_spec` — what does ORDER BY cost? |
| `v_ic8_spec` | LDBC IC8 (multi-hop chain) | different shape than IC2 |
| `v_empty_by_data` | indexed lookup that misses | type-checks fine; runtime returns 0. Pairs vs `e_*` to show the typechecker's *limit* |
| `v_count_friends` | aggregate `COUNT(f.id)` | aggregation runtime path |
| `v_repeat` | variable-length `~[:knows]~{1,2}` | repetition runtime |

**Empty by typing (10)** — typechecker says guaranteed-empty.

| id | what it tests |
|---|---|
| `e_chain4_bad_leaf` | 4-hop knows chain to unknown leaf label |
| `e_chain_mid_bad` | unknown label mid-chain |
| `e_bad_edge_deep` | unknown edge label deep in chain |
| `e_ic2_bad_msg` | IC2 shape (friend-of-friend → message) with bad message label |
| `e_conflict_label_deep` | same var bound to incompatible labels (`Person ∩ Comment = ⊥`) |
| `e_type_mismatch_chain` | str/int mismatch in WHERE on a 3-hop pattern |
| `e_union_all_bad` | path-pattern union with every arm empty |
| `e_repeat_bad_leaf` | bounded repetition `{1,2}` to unknown label |
| `e_label_only` | single label not in schema (small-pattern control) |
| `e_type_clash_arith` | arithmetic between mismatched types (`p.firstName + p.id`) — different surface than `e_type_mismatch_chain` (which uses equality) |

**Invalid (6)** — compile pipeline rejects.

| id | what it tests |
|---|---|
| `i_unbound_after_chain4` | free var in RETURN after 4-hop chain |
| `i_unbound_in_where_chain` | free var in WHERE on multi-hop pattern |
| `i_unbound_in_union` | free var in projection on union pattern |
| `i_unbound_compound_where` | free var alongside valid filter via AND |
| `i_unbound_simple` | free var in RETURN on bare match |
| `i_parse` | parse error (`MATCH (p RETURN p.name`) |

## Why this exists separate from the cross-system bench

The cross-system bench answers "how does gqlite compare to other
graph databases on user-facing query latency". Its case set is
gated on what gqlite, Kuzu, and graphqlite can ALL run; its
ablation has one knob (LTJ index-fold on/off). It produces the
headline numbers.

The internal bench answers "how do gqlite's own components and
configurations behave". It's not gated on cross-engine
compatibility, runs anything we author, and surfaces things the
external bench can't (typechecker short-circuit value, lazy-vs-disk
backend tradeoff, scale-factor scaling, RSS / setup-time
diagnostics). It's slower per run (~30 min vs the external's ~10
min) and runs on a different cadence — not every dev iteration.

Together they're the project's two-bench split: external for
competitive claims, internal for engineering diagnostics.

## See also

- `src/bin/internal_bench.rs` — the binary; rustdoc header has a
  brief usage reference.
- `bench/cross-system/README.md` — companion external bench.
- `bench/cross-system/SURVEY.md` — narrative covering every external
  system evaluated (working AND rejected).
