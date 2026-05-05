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
| GQLite — auksys/gqlite, [gqlite.org](https://gqlite.org/) (OpenCypher, SQLite/Redb/Postgres backends; PyPI: `gqlitedb`) | [`auksys_gqlite/`](auksys_gqlite/) | ⚠ integration scaffolded; **fails to load LDBC SF0.1 in reasonable time** — see [`auksys_gqlite/DIVERGENCES.md`](auksys_gqlite/DIVERGENCES.md) |
| GraphLite — GraphLite-AI/GraphLite (ISO GQL, Sled-backed) | [`graphlite/`](graphlite/) | ⚠ integration scaffolded; **load hangs on the comments phase** — see [`graphlite/DIVERGENCES.md`](graphlite/DIVERGENCES.md) |
| GQLite — webbery/gqlite (custom DSL, dead since April 2023) | — | not integrated; `auksys/gqlite` above is the actively-maintained successor |

## Setup

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

Each external system additionally needs its own one-time setup
(install the system's CLI/library, load the LDBC CSVs into the
system's native format). See each subdir's README/script.

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

`comparison.txt` has four sections:

0. **Errored param rows** — for each system, the count of param rows
   where any iter returned `result_count = -1` (sentinel for runner-
   level failure: SDK error, panic, etc.). A high tally means the
   latency table below is over a partial sample. The integrated-but-
   blocked systems (graphlite, auksys_gqlite) document their failure
   modes in their per-system DIVERGENCES.md.
1. **Per-cell summary** — for each (params_row, system) pair, median
   latency, p95, iter count, the result_count, and the result_shape
   (per-row type signatures, deduped — e.g. `i,s,s,i,s,i|i,s,s,i,n,i`
   for IC2 where `c.content` is sometimes null). Errored rows are
   excluded.
2. **Count + shape consistency** — for each params_row, do all systems
   agree on row count AND per-row column types? Without ORDER BY the
   actual row contents legitimately differ (each system picks a
   different N rows from the full result), but the column count and
   types must match. `WARN` flags disagreement, which means a
   per-system query translation bug.
3. **Side-by-side latency** — one row per params_row, one column per
   system, median ms. Cells where a system errored out show as `--`
   (no successful samples to median).

## Out of scope

- Other ICs (IC1, IC3...IC14, BI*) — adding them is mechanical
  (new translation file per system) but defer until requested.
- Spec-faithful IC2 (ORDER BY, coalesce) — needs gqlite parser
  features first; revisit when those land.
- LDBC-driver-mediated audited compliance — that's a different
  deliverable (~3 weeks more work). This bench is research-paper-tier.
- CI integration — bench machines vary too much to threshold on.

