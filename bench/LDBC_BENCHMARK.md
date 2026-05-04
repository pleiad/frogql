# LDBC SNB Interactive — How to Run

## Status

- **Implemented**: IC2 — `query` + in-query `LIMIT 20`; bench uses `run_query(..., 0)` (caller cap off, LIMIT applies).
- **Blocked**: IC1, IC3–IC14 — `blocked_reason`, `required_features`, optional `params_file` / `param_columns` / `param_types` / `query` (not run). `--ic blocked` = inventory only.

`ic<n>.toml`: title, spec URL, keys, `query`; IC2 adds `return_columns`, `expected_shape`, `[divergences]`. Import / type letters (`i`/`s`/…, `/` = union): below.

### Labels (CsvBasic → `.gdb`)

**Edges:** rel type = middle stem of the edge CSV filename (e.g. `knows`, `replyOf`, `hasMember`). **Nodes:** label from the node CSV stem with initial cap (`Person`, `Post`, `TagClass` — see `ldbc_node_label` in `src/model/csv_loader.rs`). Different rule than edges; use these strings in patterns against an imported `.gdb`.

## Setup

One command does everything — downloads the dataset + substitution
parameters and builds the `.gdb`:

```bash
cargo build --release --bin gqlite --bin bench_setup
./target/release/bench_setup
```

That fetches `social_network-sf0.1-CsvBasic-LongDateFormatter.tar.zst`
(17 MiB) and `substitution_parameters-sf0.1.tar.zst` (200 KiB), unpacks
both, and runs `gqlite --import-ldbc-csv` to produce the 1.4 GiB
`.gdb`. Each step is idempotent and skips if its output is present;
re-running is a no-op.

After it completes:

```
bench/data/
├── ldbc-sf0.1.gdb                                            ← what the bench opens
├── ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter/  ← raw CSVs
└── substitution_parameters-sf0.1/substitution_parameters-sf0.1/ ← per-IC param files
```

Useful flags:

| flag | default | meaning |
|---|---|---|
| `--data-dir <dir>` | `bench/data` | Where archives + extracted dirs live |
| `--rebuild` | off | Force-rebuild the `.gdb` even if present |
| `--skip-download` | off | Use already-downloaded archives only; fail if missing |

The bench's `--params-dir` defaults match the layout above, so no
extra flags needed if you used the default `--data-dir`. If you set
`--data-dir <X>`, `bench_setup` prints the exact `ldbc_bench`
invocation with the right `--params-dir`.

## Run

```bash
cargo build --release --bin ldbc_bench

# IC2, default backend (lazy), 3 iterations per param:
./target/release/ldbc_bench bench/data/ldbc-sf0.1.gdb --ic 2 --iters 3

# Same, against the disk backend:
./target/release/ldbc_bench bench/data/ldbc-sf0.1.gdb --ic 2 --backend disk --iters 3

# All currently-implemented ICs:
./target/release/ldbc_bench bench/data/ldbc-sf0.1.gdb --ic all

# Inventory of blocked ICs (no run):
./target/release/ldbc_bench bench/data/ldbc-sf0.1.gdb --ic blocked
```

### Flags

| flag | default | meaning |
|---|---|---|
| `--ic <n>\|N,M\|all\|blocked` | `2` | Which IC(s) to run; `all` = every implemented; `blocked` = inventory only |
| `--backend memory\|lazy\|disk` | `lazy` | Which `GraphAccess` to use |
| `--iters N` | `3` | Measured iterations per param row |
| `--warmup N` | `0` | Extra iters per row before measurement; their times are discarded. Whether it helps depends on OS / RAM / storage — on a warm machine with free RAM ≥ dataset size it usually doesn't (OS page cache populated by row#0); on cold or cache-constrained machines it can absorb a cold-iter spike. Raise it if the per-row summary shows iter 0 consistently dominating min/median. |
| `--params-dir <dir>` | `bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1` | Where the LDBC param files live |
| `--queries-dir <dir>` | `bench/ldbc-queries` | Where the per-IC TOML files live |
| `--csv-dir <dir>` | — | Required only with `--backend memory`; the .gdb path is then ignored |

### Output

The bench writes to two streams on purpose, following the Unix
convention that **stdout = data, stderr = chatter**:

```bash
./target/release/ldbc_bench db.gdb --ic 2 \
    > runs.csv          # raw measurements only
    2> summary.txt      # human-readable progress + per-row summaries
```

If you don't redirect, both go to your terminal interleaved.

#### stdout — one row per `(param, iter)` measurement

```csv
query;backend;params;row;iter;result_count;elapsed_ns
IC2;lazy;19791209300143|1354060800000;0;0;20;7800623500
IC2;lazy;19791209300143|1354060800000;0;1;20;9511623200
IC2;lazy;19791209300143|1354060800000;0;2;20;7637990200
IC2;lazy;10995116278647|1346112000000;1;0;20;8418309000
...
```

| column | meaning |
|---|---|
| `query` | which IC ran (e.g. `IC2`) |
| `backend` | `memory` / `lazy` / `disk` |
| `params` | substitution-param values for this row, joined by `\|` (same separator LDBC's param files use). For IC2, that's `<personId>\|<maxDate>` |
| `row` | 0-indexed line of the LDBC param file (`interactive_<n>_param.txt`); same number as `row#N` in the stderr summary |
| `iter` | 0-indexed iteration of this same row (0 to `--iters - 1`) |
| `result_count` | how many rows the query returned (capped by the in-query `LIMIT` and `Runtime::run_query` limit combining) |
| `elapsed_ns` | wall time of one `Runtime::run_query` call, in **nanoseconds** |

For `--iters 3` with 15 IC2 params, you get 45 rows of CSV. Use this
stream when you want to compute your own statistics (medians,
percentiles, geomean across params, etc.) — it's the raw data.

#### stderr — human-readable progress, summaries, and memory tracking

A complete stderr from `--ic 2 --iters 3` looks like:

```
Loaded 14 IC definitions from bench/ldbc-queries (1 implemented, 13 blocked)
RSS baseline: 9.9 MiB
Loading bench/data/ldbc-sf0.1.gdb (LazyGraphStore)...
  loaded 327588 nodes / 1477965 edges in 10.23s
  RSS after open: 401.2 MiB (+391.3 MiB)

=== IC2: Recent messages by your friends (backend=lazy) ===
Params: interactive_2_param.txt (15 rows, columns: personId, maxDate);  0+3 iters/param (warmup+measured)
  IC2 row#0   (19791209300143|1354060800000) count=20  min= 7637.99ms  med= 7800.62ms  mean= 8316.74ms  max= 9511.62ms  (n=3)
  IC2 row#1   (10995116278647|1346112000000) count=20  min= 7858.60ms  med= 8418.31ms  mean= 8448.11ms  max= 9067.41ms  (n=3)
  ...

✓ done — 1 IC(s) ran to completion
  IC2: 15 rows × 3 iter(s) = 45 runs; across-row median 8420.34ms (range 7240.12-10380.55ms)
Peak RSS during query loop: 407.4 MiB (+398.6 MiB over baseline)
```

Three sections:

**Setup** (everything before the `===` line). What was loaded and how
much RAM the backend's open call cost.

**Per-row line** (`IC2 row#0  (...)  ...  (n=3)`):

- `row#N` — same as the `row` column in the CSV (0-indexed line of the LDBC param file)
- `(personId|maxDate)` in parens — substitution-param values for this row
- `count=20` — `result_count` (rows the query returned; IC2 uses `LIMIT 20` in the GQL template)
- `min / med / mean / max` — the four stats computed from `--iters` runs of this row, in milliseconds
- `(n=3)` — the sample size. When `--iters 1` (so n=1), the four stats collapse to the same number; the bench prints `wall=Xms` instead with a hint to raise `--iters`

**Final summary** (after the last row):

- `✓ done` — confirms the run reached the end (vs got killed mid-row)
- The per-IC `× iter(s) = N runs` + `across-row median` line is the headline number; no grepping needed
- `Peak RSS` — see below

#### What RSS and "baseline" mean

**RSS** ("resident set size") = how much physical RAM the bench
process is using right now, as reported by the OS. Higher = more
RAM in use. The bench reads it via `sysinfo` at three points:
process start, just after the backend opens, and at the peak during
the query loop.

**Baseline** is just the RSS at process start — an almost-empty
process before the backend has loaded anything. Reporting
`+X MiB over baseline` makes the per-step cost readable:
`+391 MiB` reads as "the backend's open used 391 MiB of RAM,"
without you having to subtract the housekeeping the OS attributes
to the process.

Concretely on the example above:

- `RSS baseline: 9.9 MiB` — process started, almost nothing loaded
- `RSS after open: 401.2 MiB (+391.3 MiB)` — `LazyGraphStore::open` brought topology + label index + initial page cache into RAM
- `Peak RSS during query loop: 407.4 MiB (+398.6 MiB)` — peak observed *during* the timed loop, ~7 MiB above the post-open figure (Lazy's in-process LRU page cache filling as queries fault new pages)

For comparing backends (Lazy vs Disk), the **deltas** are what
matters — how many extra MiB each backend's open and query loop
add over the same baseline. Absolute numbers vary by OS / mmap
behavior; deltas are honest.

#### Cross-checking the two streams

The summary line above for `row#0` was computed from these three CSV rows:
```
IC2;lazy;19791209300143|1354060800000;0;0;20;7800623500   ← iter 0: 7.80 s
IC2;lazy;19791209300143|1354060800000;0;1;20;9511623200   ← iter 1: 9.51 s
IC2;lazy;19791209300143|1354060800000;0;2;20;7637990200   ← iter 2: 7.64 s
```
min = 7.64 s, median = 7.80 s, mean ≈ 8.32 s, max = 9.51 s — matches the stderr line.

If you only need the headline numbers, skip the CSV entirely and read
the `.txt`. The CSV is there for when you need raw data.

## Adding a new IC

When a gqlite feature lands that unblocks an IC:

1. Edit `bench/ldbc-queries/ic<n>.toml`:
   - Change `status` from `"blocked"` to `"implemented"`
   - Add `query = """..."""`, `params_file`, `param_columns`
   - **If any param is a string** (e.g. `firstName` in IC1, country
     names in IC3/IC11, `tagName` in IC6), add a `param_types` array
     with `"string"` for those columns and `"int"` for the rest. The
     runner wraps string values in single quotes automatically; bare
     `{<col>}` placeholders in the query template work for both types.
     **Caveat:** gqlite's lexer has no escape syntax for `'` inside
     string literals, so any param value containing `'` is rejected
     at substitute time with a clear error.
   - Move `[divergences]` (if any) from `blocked_reason` prose into the
     structured table
2. Run `./target/release/ldbc_bench bench/data/ldbc-sf0.1.gdb --ic <n>`
   to verify the query parses and returns rows.

No bench code changes — the runner discovers the file at startup,
validates the schema, and dispatches by id.

## Backends, in one paragraph

`lazy` is what gqlite's REPL/Python bindings use; it's the default
and the canonical "embedded DB" configuration. `disk` is similar but
without the in-process LRU cache. `memory` loads the entire LDBC CSV
into RAM at startup — useful for smoke tests with no `.gdb` dependency,
not realistic as a database configuration.

## See also

- `bench/ldbc-queries/ic<n>.toml` — per-IC query, params, divergences,
  and required-features metadata.
- `src/bin/ldbc_bench.rs` — the runner (rustdoc header has the same
  flag reference).
- The paper — for results, findings, and analysis.
