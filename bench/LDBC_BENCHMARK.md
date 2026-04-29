# LDBC SNB Interactive — How to Run

## Status

- **Implemented**: IC2.
- **Catalogued, not yet implemented**: IC1, IC3–IC14. Each has a
  `bench/ldbc-queries/ic<n>.toml` listing the gqlite features that
  would unblock it (`required_features`).

`./target/release/ldbc_bench placeholder --ic blocked` prints the
inventory.

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
./target/release/ldbc_bench placeholder --ic blocked
```

### Flags

| flag | default | meaning |
|---|---|---|
| `--ic <n>\|N,M\|all\|blocked` | `2` | Which IC(s) to run; `all` = every implemented; `blocked` = inventory only |
| `--backend memory\|lazy\|disk` | `lazy` | Which `GraphAccess` to use |
| `--iters N` | `3` | Measured iterations per param row |
| `--limit N` | `20` | Row cap (`Runtime::run_query`'s second arg; emulates spec `LIMIT 20` until gqlite has the keyword) |
| `--params-dir <dir>` | `bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1` | Where the LDBC param files live |
| `--queries-dir <dir>` | `bench/ldbc-queries` | Where the per-IC TOML files live |
| `--csv-dir <dir>` | — | Required only with `--backend memory`; the .gdb path is then ignored |

### Output

- **stdout**: CSV `query;backend;ic;param_idx;iter;result_count;elapsed_ns`.
  Suitable for piping into `awk`/`pandas` for aggregation.
- **stderr**: human-readable summary (per-row min / median / mean / max
  in ms when `--iters >= 2`; `wall=` only when N=1).

## Adding a new IC

When a gqlite feature lands that unblocks an IC:

1. Edit `bench/ldbc-queries/ic<n>.toml`:
   - Change `status` from `"blocked"` to `"implemented"`
   - Add `query = """..."""`, `params_file`, `param_columns`
   - Move `[divergences]` (if any) from `blocked_reason` prose into the
     structured table
2. Run `./target/release/ldbc_bench bench/data/ldbc-sf0.1.gdb --ic <n>`
   to verify the query parses and returns rows.

No bench code changes — the runner discovers the file at startup.

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
