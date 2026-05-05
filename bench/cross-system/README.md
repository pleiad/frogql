# Cross-system benchmark

Side-by-side latency for LDBC SNB Interactive Complex queries on
gqlite + a set of external graph systems, against the same SF0.1
dataset and the same substitution-parameter rows. Currently only IC2
is wired up (it's the only IC whose `bench/ldbc-queries/ic<n>.toml`
has `status = "implemented"`); more come online as the parser gains
features. The runner accepts `--ic <n>`; each invocation runs one IC
across all (selected) systems.

## What gets compared

| System | Subdir | Status |
|---|---|---|
| gqlite (lazy backend) | [`gqlite/`](gqlite/) | ✅ implemented |
| GraphQLite — colliery-io/graphqlite (Cypher, SQLite-backed) | [`graphqlite/`](graphqlite/) | ✅ implemented |
| Kuzu — kuzudb (vectorized columnar engine, CIDR 2023; pinned to v0.11.3 since [upstream archived 2025-10-10](https://github.com/kuzudb/kuzu)) | [`kuzu/`](kuzu/) | ✅ implemented; see [`kuzu/DIVERGENCES.md`](kuzu/DIVERGENCES.md) for the archival-status framing and the `UNION ALL` query-shape divergence |
| GraphLite — GraphLite-AI/GraphLite (ISO GQL, Sled-backed) | — | not yet integrated |
| GQLite — webbery/gqlite (custom DSL, dead since April 2023) | — | not yet integrated |

## Setup

### 1. Shared dataset

The bench depends on the same LDBC SF0.1 dataset our regular
`ldbc_bench` uses. From the repo root:

```bash
cargo build --release
./target/release/bench_setup   # downloads + extracts SF0.1 (~17 MiB)
```

That produces:
- `bench/data/ldbc-sf0.1.gdb` — gqlite's binary (used by `gqlite/run.sh`)
- `bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter/`
  — raw LDBC CSVs (used by external systems' loaders)
- `bench/data/substitution_parameters-sf0.1/.../interactive_2_param.txt`
  — 15 IC2 param rows (`personId|maxDate`)

### 2. Per-system prerequisites

Each external system has its own install line. To install all of
them in one go:

```bash
bash bench/cross-system/install_python_deps.sh
```

That script just runs `pip install -r requirements.txt` in each
implemented per-system subdir. If you'd rather install them
piecemeal, they're:

| System | Install command | Other prerequisites |
|---|---|---|
| `gqlite` | `cargo build --release` (covered by step 1) | none |
| `graphqlite` | `pip install -r bench/cross-system/graphqlite/requirements.txt` | none |
| `kuzu` | `pip install -r bench/cross-system/kuzu/requirements.txt` | none |

Each per-system subdir has its own `README.md` documenting prereqs,
CLI, and any per-system gotchas. Failing-but-scaffolded systems
(e.g. on the `bench/cross-system-failed-attempts` branch) ship a
`DIVERGENCES.md` that explains why their integration didn't pan out.

### 3. Per-system data load

The first time `run_all.sh` runs, each implemented system's runner
will auto-invoke its own `setup.py` (or equivalent) to load the LDBC
CSVs into that system's native format. This is a one-time cost
amortized over every subsequent bench run. To pre-load explicitly:

```bash
python bench/cross-system/graphqlite/setup.py --ic 2
python bench/cross-system/kuzu/setup.py --ic 2
```

(`gqlite` doesn't need a separate per-system setup — `bench_setup`
in step 1 already produced its `.gdb`.)

## Running

```bash
# Default: IC2 against every implemented system.
bench/cross-system/run_all.sh

# Pick a different IC (must be 'implemented' in its toml):
bench/cross-system/run_all.sh --ic 2

# Just one system (useful while iterating on a per-system runner):
bench/cross-system/run_all.sh --only gqlite
bench/cross-system/run_all.sh --only gqlite,graphqlite

# Tune iteration count:
bench/cross-system/run_all.sh --iters 30 --warmup 3
```

For a multi-IC sweep, run the script once per IC — each invocation
lands in its own timestamped results dir.

Output lands in `bench/cross-system/results/<timestamp>/`:
- `<system>.csv` per system — raw per-iter rows (same schema as
  `ldbc_bench`:
  `query;backend;params;row;iter;result_count;elapsed_ns`)
- `cross_system.csv` — concatenation of all the above
- `comparison.txt` — `compare_results.py` output (latency table +
  count/shape consistency check + side-by-side comparison)
- `skipped.log` — any systems that couldn't run, with reasons

## The query

The canonical IC2 lives in [`bench/ldbc-queries/ic2.toml`](../ldbc-queries/ic2.toml)
— same TOML our regular `ldbc_bench` consumes. Per-system harnesses
translate it into their native query syntax (`graphqlite/ic2.cypher`,
`graphlite/ic2.gql`, etc.); each translation file's first comment
points back to the toml.

The toml documents divergences from the LDBC spec (no ORDER BY, no
`coalesce`, lowercase edge labels) — these are gqlite parser
limitations. Per the plan: every system runs **our** divergent IC2,
not the spec version. That keeps the comparison apples-to-apples
even though the other systems could technically execute spec IC2.
The doc-pointer convention makes this honest: if you read
`graphqlite/ic2.cypher` and wonder why it doesn't have `ORDER BY`,
the comment-link explains.

## Reading the results

`comparison.txt` has three sections:

1. **Per-cell summary** — for each (params_row, system) pair, median
   latency, p95, iter count, the result_count, and the result_shape
   (per-row type signatures, deduped — e.g. `i,s,s,i,s,i|i,s,s,i,n,i`
   for IC2 where `c.content` is sometimes null).
2. **Count + shape consistency** — for each params_row, do all systems
   agree on row count AND per-row column types? Without ORDER BY the
   actual row contents legitimately differ (each system picks a
   different N rows from the full result), but the column count and
   types must match. `WARN` flags disagreement, which means a
   per-system query translation bug.
3. **Side-by-side latency** — one row per params_row, one column per
   system, median ms.

## Out of scope

- Other ICs (IC1, IC3...IC14, BI*) — adding them is mechanical
  (new translation file per system) but defer until requested.
- Spec-faithful IC2 (ORDER BY, coalesce) — needs gqlite parser
  features first; revisit when those land.
- LDBC-driver-mediated audited compliance — that's a different
  deliverable (~3 weeks more work). This bench is research-paper-tier.
- CI integration — bench machines vary too much to threshold on.

