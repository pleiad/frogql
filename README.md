<p align="center">
  <img src="assets/frogql-logo.png" alt="froGQL logo" width="240">
</p>

<h1 align="center">froGQL</h1>

<p align="center">
  A Rust graph database implementing ISO GQL, with single-file storage inspired by SQLite.
</p>

<p align="center">
  <a href="https://pypi.org/project/frogql/"><img alt="PyPI" src="https://img.shields.io/pypi/v/frogql?logo=pypi&logoColor=white&label=PyPI&color=2c8a3e"></a>
  <a href="https://www.npmjs.com/package/frogql"><img alt="npm" src="https://img.shields.io/npm/v/frogql?logo=npm&label=npm&color=2c8a3e"></a>
  <a href="https://www.npmjs.com/package/frogql-wasm"><img alt="npm (wasm)" src="https://img.shields.io/npm/v/frogql-wasm?logo=webassembly&logoColor=white&label=wasm&color=2c8a3e"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-2c8a3e"></a>
  <a href="https://pleiad.github.io/frogql/"><img alt="Website" src="https://img.shields.io/badge/website-pleiad.github.io%2Ffrogql-2c8a3e"></a>
</p>

<p align="center">
  <b><a href="https://pleiad.github.io/frogql/#playground">Live playground</a></b> ·
  <a href="https://pleiad.github.io/frogql/">Website</a> ·
  <a href="docs/query-language.md">Query language</a> ·
  <a href="docs/data-modification.md">Data modification</a> ·
  <a href="docs/internals/storage-architecture.md">Storage format</a>
</p>

<p align="center">
  <i>Run GQL in your browser &mdash; the website loads the real <code>frogql-wasm</code> engine and a sample movies graph, no install required.</i>
</p>

---

## Project history and our use of AI

froGQL is a research prototype that grew into a working database. Its differentiator is a typechecker grounded in a gradual type system for GQL: it detects queries that cannot return results because of type mismatches, before they run. We combine hand-written research code with AI-assisted engineering, and we are explicit about the boundary between the two.

The path so far:

1. **Theory first.** The project grew out of [our paper on a gradual type system for GQL](https://dl.acm.org/doi/10.1145/3763061), which contributes the formalization and the soundness theorems (we have a background in gradual typing and databases). The central result is **Emptiness**: statically detecting queries that are guaranteed to return nothing because of type mismatches.

2. **A hand-written prototype.** We implemented the typechecker and the runtime semantics of the formal model in Python, by hand, without AI. This is the reference everything else is checked against.

3. **The question worth testing.** Does the typechecker earn its cost? That is, is *typecheck + run* worthwhile against blindly *running* a query only to discover it returns nothing? Answering this needed performance the Python prototype could not provide.

4. **Port to Rust, with AI.** We rewrote the engine in Rust with AI assistance, taking inspiration from SQLite's single-file storage architecture.

5. **Worst-case-optimal joins.** Joins were slow. Colleagues who work on databases pointed us to the [CompactLTJ paper](https://users.dcc.uchile.cl/~gnavarro/ps/vldbj25.pdf) (Arroyuelo et al., VLDB Journal 2025) and its C++ reference implementation. We implemented the Leapfrog Triejoin algorithm with generative AI, keeping the test suite green throughout.

6. **LDBC and language growth.** To run the LDBC Social Network Benchmark we extended the language with more features, including our centerpiece, the typechecker. We did this carefully to avoid breaking the theorems we had proved formally.

7. **Where AI stopped being enough.** The benchmarks were strong on some fronts and weak on others. Closing the gaps took design decisions and research that generative AI could not deliver on its own: LTJ interacts poorly with GQL features such as `UNION`, and even with plain `ORDER BY`. We grow the system slowly, debating design strategies and testing several before each new feature lands.

Benchmark charts from the LDBC Social Network Benchmark are coming here as the numbers stabilize.

**Papers**
- [*Flexible and Expressive Typed Path Patterns for GQL*](https://dl.acm.org/doi/10.1145/3763061) — the gradual type system, formalization, and the Emptiness theorem behind froGQL's typechecker.
- [CompactLTJ](https://users.dcc.uchile.cl/~gnavarro/ps/vldbj25.pdf) (Arroyuelo et al., VLDB Journal 2025) — the worst-case-optimal join algorithm froGQL uses for comma-joins and chains.

---

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
| Indexes | Auto-built hash + btree on every uniquely-keyed `(label, prop)` (rebuilt on open); explicit `CREATE [HASH \| BTREE] INDEX` for the rest, persisted in the `.gdb` header chain so they survive close/reopen. | [`docs/secondary-indexes.md`](docs/secondary-indexes.md) |

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
triples; non-decomposable shapes (any-direction edges, repetitions with
named variables, unbounded `{n,}`) fall back to pairwise hash-join.
Speedups on `soc-LiveJournal1-100k` (limit 1000) range from
**14× (3-clique) to 4097× (4-path)**, with a 4-clique going from "hung"
to 43 ms. See [`docs/internals/JOIN_STRATEGY_NOTES.md`](docs/internals/JOIN_STRATEGY_NOTES.md)
and `CLAUDE.md` for the algorithm in detail.

## Optimizer

The compiler pipeline is `parse → elaborate → typecheck → optimize → run`.
The optimizer is reserved for performance-preserving rewrites; ISO
syntactic sugar lives in `elaborate`. Major passes:

- **WHERE pushdown** into descriptor `value_preds` and `value_filters`
  (type ascriptions like `is T`, value comparisons like `<`, `<=`, `=`).
- **Index-driven constant folding**: `Eq` predicates that hit a hash
  index resolve to a single NodeId, get pre-bound in the result tuple,
  and excluded from the VEO. `<` / `<=` / `>` / `>=` that hit a btree
  precompute the matching sorted set and become an O(log n) membership
  test.
- **Bounded `Repeat` unrolling**: `(P){lb, ub}` with single-edge inner
  and `ub - lb + 1 ≤ 4` rewrites to `Union(P^lb, …, P^ub)` and
  distributes the Union out of any surrounding Concat — each arm
  becomes a flat chain that decomposes natively into triples.
- **ORDER BY alias resolution**: `ORDER BY <RETURN-alias>` lowers to
  the underlying `AttrLookup` so the runtime routes the sort through
  the pre-projection top-k heap (and the btree-driven path) instead of
  fully projecting every row before truncating.
- **OPTIONAL MATCH bind-pushdown** (runtime-side, in `run_match_chain`):
  for each outer row, the inner pattern runs as a small LTJ with the
  shared variables pre-pinned — SQLite's correlated nested-loop with
  index lookup, mapped onto the LTJ. Cuts a representative LDBC IS5
  query from 52 s → 0.56 s on SF0.1 (~93×).
- **BTree-driven top-k**: when the sort key has a btree (auto-built or
  declared) the runtime walks the btree in primary-attr order, runs an
  LTJ pin per id, and stops at `LIMIT k` (or at the first cohort
  boundary past `k` for multi-spec ORDER BY). Multi-label `Or`
  descriptors (`Comment | Post`) drive the search via a k-way merge
  over per-label btrees. Drops LDBC IC2 on SF0.1 from 3.0 s → 0.086 s
  with the right indexes declared.

See [`docs/internals/implemented-optimizations.md`](docs/internals/implemented-optimizations.md)
for benchmark numbers and code-level detail.

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
