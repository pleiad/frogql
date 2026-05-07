# froGQL

A Rust graph database implementing ISO GQL path pattern matching, with single-file storage inspired by SQLite.

## Install

**Python (PyPI)**:
```bash
pip install frogql
```
Wheels ship for CPython 3.8+ on Linux (x86_64, aarch64), macOS (x86_64, arm64), and Windows (x86_64).

**CLI / library (build from source)**:
```bash
cargo build --release
```

## Quick Start (REPL)

```bash
# Import a CSV dataset and open the REPL
./target/release/frogql movies.gdb --import-csv path/to/Spanner_Instance/

# Open an existing database
./target/release/frogql movies.gdb

# Skip the typechecker for the session
./target/release/frogql movies.gdb --no-typecheck
```

```
gql> .schema
GRAPH TYPE DEFAULT (active)
Node types:
    (:Movie  {released INT, title STRING, votes INT, *})
    (:Person {name STRING, *})
Edge types:
    (:Person)-[:ACTED_IN {roles LIST<STRING>}]->(:Movie)
    (:Person)-[:DIRECTED]->(:Movie)
    (:Person)-[:REVIEWED {rating INT, summary STRING}]->(:Movie)
    ...

gql> MATCH (p: Person) -[:ACTED_IN]-> (m: Movie) WHERE m.released = 1999 RETURN p.name, m.title
p.name | m.title
-------+--------
"Keanu Reeves" | "The Matrix"
"Carrie-Anne Moss" | "The Matrix"
...

gql> MATCH (a: Person) -[:ACTED_IN]-> (m: Movie), (d: Person) -[:DIRECTED]-> (m) RETURN a.name, d.name
```

### REPL meta-commands

The REPL follows the SQLite convention: every meta-command starts with `.`.

| Command | Effect |
|---------|--------|
| `.schema` | alias for `SHOW GRAPH TYPE DEFAULT` (the auto-inferred schema) |
| `.schema simple` | grouped by-label renderer of the inferred schema |
| `.graph-types` | alias for `SHOW GRAPH TYPES` |
| `.indexes` | alias for `SHOW INDEXES` |
| `.save` | atomically persist the in-RAM mutations to the open `.gdb` (tmp+rename) |
| `.dump-json <path>` | pg_dump-style JSON snapshot of the merged graph |
| `.dump-gql <path>` | pg_dump-style GQL script that re-creates the graph |
| `.help` | list meta-commands and quick query syntax |
| `.quit` / `.exit` | exit (bare `quit` / `exit` also work) |

Opening a path that does not exist creates an empty database (sqlite3 convention):

```bash
./target/release/frogql /tmp/fresh.gdb
creating new database: /tmp/fresh.gdb
gql> INSERT (a:Person {name: 'Alice'})
gql> .save
```

## Example Databases

The `examples/` directory contains ready-to-use `.gdb` files:

| Database | Nodes | Edges | Domain |
|----------|-------|-------|--------|
| movies.gdb | 171 | 253 | Movies (Person, Movie, ACTED_IN, DIRECTED, ...) |
| fraud_detection.gdb | 14,550 | 31,564 | Financial fraud (Transaction, Account, TRIGGERED_ALERT, ...) |
| bom.gdb | 1,500 | 2,248 | Bill of Materials (Product, Assembly, ConsistsOf, ...) |

Regenerate from CSV: `./target/release/frogql examples/movies.gdb --import-csv <csv_dir>`

## What you can do

| Topic | One-liner | Details |
|---|---|---|
| Load data | CSV (Spanner / Text2GQL / Cypher), LDBC SNB, or JSON; `DEFAULT` graph type is inferred at import. | [`docs/data-import.md`](docs/data-import.md) |
| Query | `MATCH`, `OPTIONAL MATCH`, comma-joins, `WHERE`, `RETURN`, `EXISTS`, repetition `{n,m}`, label algebra. | [`docs/query-language.md`](docs/query-language.md) |
| Mutate | `INSERT`, `SET`, `REMOVE`, `[DETACH] DELETE` (ISO §13 MVP-1). In-RAM overlay until `.save`. | [`docs/data-modification.md`](docs/data-modification.md) |
| Schemas | Persistent named graph types with `CREATE / USE / DROP / SHOW / VALIDATE GRAPH TYPE`; reserved `DEFAULT` is auto-inferred. | [`docs/graph-types.md`](docs/graph-types.md) |
| Indexes | Auto-built hash index on every uniquely-keyed `(label, prop)`; explicit `CREATE [HASH \| BTREE] INDEX` for the rest. | [`docs/secondary-indexes.md`](docs/secondary-indexes.md) |

## Storage

froGQL uses a single `.gdb` file with 4KB pages:
- All node/edge IDs are `u32` internally (no string overhead at query time)
- Three storage backends: in-memory `Graph`, `LazyGraphStore` (topology in RAM, labels/props from disk via LRU page cache), `DiskGraphStore` (minimal RAM)
- The REPL uses `LazyGraphStore` for efficient memory usage on large graphs
- The GRAPH TYPE catalog persists in its own page chain, so `CREATE / USE / DROP GRAPH TYPE` survive close/reopen

See [`docs/internals/storage-architecture.md`](docs/internals/storage-architecture.md) for the full format specification.

## Join strategy

Comma-joins and chains of directed/undirected edges are executed with
**Leapfrog Triejoin** (LTJ), a worst-case-optimal multi-way join that
binds variables one at a time across all participating patterns
simultaneously, with no intermediate materialisation. Each directed edge
is modelled as a triple `(src, label, tgt)` indexed in six sorted
orderings. LTJ activates automatically when the pattern decomposes into
triples; non-decomposable shapes (unions, repetitions, any-direction
edges) fall back to pairwise hash-join. Speedups on
`soc-LiveJournal1-100k` (limit 1000) range from **14× (3-clique) to
4097× (4-path)**, with a 4-clique going from "hung" to 43 ms. See
[`docs/internals/JOIN_STRATEGY_NOTES.md`](docs/internals/JOIN_STRATEGY_NOTES.md) and `gqlrust/CLAUDE.md` for the algorithm
in detail.

## Building and Testing

```bash
cargo build --release                    # build all binaries

# Strict clippy (run before every commit)
cargo clippy --workspace --all-targets -- -D clippy::all

# Lib + integration sweep (bench_test is excluded — pre-existing failures)
cargo test --lib
cargo test --test parser_test --test runtime_test --test store_runtime_test \
           --test text2gql_test --test parse_and_run_test --test count_test \
           --test null_test --test record_test --test list_test \
           --test compile_diagnostics --test elaborate_test --test float_test \
           --test graph_type_test --test typecheck_smoke --test typecheck_test \
           --test optional_match_test --test multi_match_test \
           --test aggregates_proptest --test lattice_proptest --test multi_match_proptest \
           --test exists_fold_test --test exists_runtime_test \
           --test parser_dm_test --test lazy_mut_test --test dm_runtime_test \
           --test dm_persistence_test --test dm_schema_test --test dm_default_test \
           --test dump_test --test dm_set_test --test dm_remove_test \
           --test dm_label_test --test dm_delete_expr_test

# Single test
cargo test --test runtime_test test_join_star_any_label -- --exact
```

Other binaries in `src/bin/`:
- `bench_queries` — generic benchmark runner
- `bench_setup` — downloads + extracts LDBC datasets (`bench/data/` is gitignored)
- `ldbc_bench` — LDBC interactive-complete driver, queries in `bench/ldbc-queries/*.toml`
- `internal_bench` — gqlite-only diagnostic bench (typechecker on/off, lazy/disk backend)
- `convert_edgelist` — edge-list format converter

## Documentation

User-facing:
- [`docs/data-import.md`](docs/data-import.md) — CSV / LDBC / JSON loaders
- [`docs/query-language.md`](docs/query-language.md) — `MATCH`, `OPTIONAL MATCH`, `EXISTS`, repetition, label algebra
- [`docs/data-modification.md`](docs/data-modification.md) — ISO §13 surface (`INSERT`, `SET`, `REMOVE`, `DELETE`), persistence, dumps
- [`docs/graph-types.md`](docs/graph-types.md) — Catalog DDL, `DEFAULT` lifecycle, validation
- [`docs/secondary-indexes.md`](docs/secondary-indexes.md) — Auto-build, declared indexes, LTJ wiring

Internals:
- [`docs/internals/storage-architecture.md`](docs/internals/storage-architecture.md) — File format, page layout, storage backends, catalog persistence
- [`docs/internals/JOIN_STRATEGY_NOTES.md`](docs/internals/JOIN_STRATEGY_NOTES.md) — LTJ algorithm, triple decomposition, benchmark numbers
- [`docs/internals/implemented-optimizations.md`](docs/internals/implemented-optimizations.md) — u32 IDs, hash-join, label index, early termination, predicate pushdown
- [`docs/internals/possible-optimizations.md`](docs/internals/possible-optimizations.md) — Fixed-length cells, sorted adjacency, future LTJ work
- [`docs/internals/iso-gql-gaps.md`](docs/internals/iso-gql-gaps.md) — Tracked deviations from ISO GQL
- [`docs/internals/graph-type-catalog-plan.md`](docs/internals/graph-type-catalog-plan.md) — Catalog design rationale
- `CLAUDE.md` — Internal architecture notes (compiler pipeline, typechecker, LTJ details)
