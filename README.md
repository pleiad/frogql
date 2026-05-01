# GQLite

A Rust graph database implementing ISO GQL path pattern matching, with single-file storage inspired by SQLite.

## Quick Start

```bash
cargo build --release

# Import a CSV dataset and open the REPL
./target/release/gqlite movies.gdb --import-csv path/to/Spanner_Instance/

# Open an existing database
./target/release/gqlite movies.gdb
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
| `.help` | list meta-commands and quick query syntax |
| `.quit` / `.exit` | exit (bare `quit` / `exit` also work) |

Everything else is parsed as either a GQL query or a catalog DDL statement (see [Graph Types](#graph-types) below).

## Example Databases

The `examples/` directory contains ready-to-use `.gdb` files:

| Database | Nodes | Edges | Domain |
|----------|-------|-------|--------|
| movies.gdb | 171 | 253 | Movies (Person, Movie, ACTED_IN, DIRECTED, ...) |
| fraud_detection.gdb | 14,550 | 31,564 | Financial fraud (Transaction, Account, TRIGGERED_ALERT, ...) |
| bom.gdb | 1,500 | 2,248 | Bill of Materials (Product, Assembly, ConsistsOf, ...) |

Regenerate from CSV: `./target/release/gqlite examples/movies.gdb --import-csv <csv_dir>`

## Data Import

GQLite reads graph data from three formats. Every import path also runs
`infer_simple_schema` and persists the result as the `DEFAULT` GRAPH TYPE,
so the typechecker is useful immediately after the first open.

**CSV with config** (Text2GQL / Cypher datasets):
```bash
gqlite mydb.gdb --import-csv path/to/Spanner_Instance/
# Reads spanner_import_config.json + CSV files.
# Node CSVs: detected as files without SRC_ID/DST_ID columns; ID column
#   resolved by trying `vid`, `<Label>_id`, any `*_id`, then column 0.
# Edge CSVs: SRC_ID, DST_ID + property columns; edge label inferred from
#   the config `label` field with known node-type prefixes stripped.
# All column lookups are case-insensitive.
```

**LDBC SNB CSV-Basic**:
```bash
gqlite mydb.gdb --import-ldbc-csv path/to/social_network-sf0.1-CsvBasic-LongDateFormatter/
# Three-pass loader:
#   1. nodes — each LDBC entity owns its id space (Place id 0 ≠ Organisation id 0),
#      so the loader keys node ids by (entity_label, external_id).
#   2. multi-valued attributes — files like Person_email_emailaddress.csv
#      collapse onto the owning node as a Value::List.
#   3. edges — directed by default, with property columns preserved.
```

**JSON**:
```bash
gqlite mydb.gdb --import-json graph.json
```

```json
{
  "nodes": [
    { "id": "n1", "labels": ["Person"], "props": { "name": "Alice", "age": 30 } }
  ],
  "edges": [
    { "id": "e1", "labels": ["Knows"], "endpoints": ["n1", "n2"], "directionality": "->" }
  ]
}
```

After import, the `.gdb` file contains everything — graph data, string
table, label index, adjacency, and the `DEFAULT` GRAPH TYPE. The original
source files are no longer needed.

## Query Language

### Full queries

```
MATCH <pattern> [WHERE <condition>] [RETURN <expressions>]
OPTIONAL MATCH <pattern> [WHERE <condition>]
```

`MATCH` is optional on the first clause (bare patterns parse). `OPTIONAL MATCH`
is supported as a top-level clause and follows ISO left-join semantics: rows
from the previous match are preserved even when the optional pattern fails to
bind, and the unbound variables become `null`.

```
gql> MATCH (p: Person)
     OPTIONAL MATCH (p)-[:DIRECTED]->(m: Movie)
     RETURN p.name, m.title
"Alice"   | null            -- Alice never directed
"Lana W." | "The Matrix"
```

### Path patterns

```
()                              any node
(x: Account)                    labeled node bound to x
(:Person & Teacher)             multiple labels (conjunction)
-[:Transfer]->                  directed labeled edge
<-[:Knows]-                     reverse direction
~[:Friends]~                    undirected edge
(x)-[:Transfer]->(y)            traversal
```

### Comma-join (multi-pattern queries)

```
(a)-[]->(b), (a)-[]->(c)                    star pattern
(a)-[]->(b), (b)-[]->(c), (c)-[]->(a)       triangle
```

Semantics: cross-product of results where shared variables unify.

### WHERE filters

```
MATCH (x: Account) WHERE x.amount > 1000 and x.active = true
MATCH (x) WHERE x.name = 'Alice'
MATCH (x) WHERE x.age is int              -- type test
MATCH (x) WHERE not x.blocked
MATCH (x) WHERE x.deleted_at IS NULL      -- absent properties read as null
MATCH (x) WHERE x.email IS NOT NULL
```

`null` is a first-class value with three-valued logic in comparisons (a
predicate involving `null` returns false and the row is dropped).
Aggregates skip `null` and empty aggregates emit `null`. Equality and
range pushdowns (`= != < <= > >=`) on node properties are evaluated
inside the LTJ loop when present.

### RETURN projection

```
MATCH (p: Person) -[:ACTED_IN]-> (m: Movie) RETURN p.name, m.title AS movie
MATCH (p: Person) -[:DIRECTED]-> (m: Movie) RETURN DISTINCT p.name
```

### Repetition

```
(-[:Transfer]->){1,3}           1 to 3 hops (variables bound to lists)
(-[x]->){2,2}                   x ↦ [e1, e2]
((-[x]->){1,2}){1,2}            x ↦ [[e1], [e2, e3]]
```

### Labels

```
(:Person & Teacher)             conjunction (both)
(:Person | Company)             disjunction (either)
(:!Admin)                       negation (not)
```

## Graph Types

GQLite has a persistent catalog of named graph types. The catalog lives
inside the `.gdb` and survives close/reopen. The DDL is ISO-style:

```
gql> CREATE GRAPH TYPE strict AS { (:Person {name STRING, age INT}),
                                   (:Person)-[:KNOWS]->(:Person) };
GRAPH TYPE 'strict' created (1 node types, 1 edge types).

gql> USE GRAPH TYPE strict;
Active GRAPH TYPE: strict.

gql> SHOW GRAPH TYPES;
  DEFAULT
* strict

gql> SHOW CURRENT GRAPH TYPE;
CURRENT GRAPH TYPE: strict
Node types:
    (:Person {age INT, name STRING})
Edge types:
    (:Person)-[:KNOWS]->(:Person)

gql> SHOW GRAPH TYPE strict;
GRAPH TYPE strict (active)
Node types:
    (:Person {age INT, name STRING})
Edge types:
    (:Person)-[:KNOWS]->(:Person)

gql> DROP GRAPH TYPE strict;
GRAPH TYPE 'strict' dropped.
```

The REPL meta-commands `.schema` and `.graph-types` are shorthands for
`SHOW GRAPH TYPE DEFAULT` and `SHOW GRAPH TYPES` respectively (see
[REPL meta-commands](#repl-meta-commands)).

While a type is active the typechecker rejects (or empties out) queries
whose labels and properties don't fit. With no active type the checker
stays permissive (`Schema::star()`). To open the REPL with the
typechecker disabled, pass `--no-typecheck`.

`DEFAULT` is reserved. It always represents the schema inferred from the
current graph data, refreshed on demand:

```
gql> USE GRAPH TYPE DEFAULT;
GRAPH TYPE 'DEFAULT' refreshed (4 node types, 6 edge types) and activated.
```

All three import modes (`--import-csv`, `--import-ldbc-csv`,
`--import-json`) populate and activate `DEFAULT` automatically.
`CREATE GRAPH TYPE DEFAULT` and `DROP GRAPH TYPE DEFAULT` are rejected.

### Validating data against a type

`USE GRAPH TYPE` does not walk the graph; it just flips the active
pointer. To check whether the actual data fits the schema, run
`VALIDATE GRAPH TYPE <name>`. The walk is opt-in because it is
O(N + E) and you may not need it on every USE.

```
gql> CREATE GRAPH TYPE strict AS { (:Person {name STRING, age INT}) };
GRAPH TYPE 'strict' created (1 node types, 0 edge types).

gql> VALIDATE GRAPH TYPE strict;
Validated 170 nodes, 253 edges against GRAPH TYPE 'strict' in 0.012s
  Violations: 38 node(s), 0 edge(s)
  Samples:
    node 12 (:Person {name: str})
    node 19 (:Movie {title: str, released: int})
    ...
```

The verdict is cached in the catalog and survives close/reopen, so
re-running the command on unchanged data is essentially free. The cache
is invalidated when the type is replaced (re-CREATE), dropped, or when
DEFAULT is refreshed.

### Type expressions

Property types accept the ISO uppercase forms `STRING`, `INT`/`INTEGER`,
`FLOAT`, `BOOL`/`BOOLEAN`, plus composites:

```
LIST<STRING>           or  [STRING]            list
LIST<LIST<INT>>                                 nested list
{ city STRING, zip INT }                         record
LIST<{ ts INT, msg STRING }>                     list of records
STRING | INT                                     union
ANY                    or  *                    wildcard
```

Label expressions support conjunction (`A&B`), disjunction (`A|B`), and
negation (`!A`), the same forms accepted in queries.

The body of a node/edge descriptor is **closed when present** — only the
listed properties are valid. An empty body `()` (no record) means the
type allows any properties.

### Python bindings

```python
import gqlite
conn = gqlite.open("movies.gdb")
conn.execute("CREATE GRAPH TYPE strict AS { (:Movie {title STRING}) }")
# → {"ok": True, "kind": "ddl", "message": "GRAPH TYPE 'strict' created..."}

conn.execute("USE GRAPH TYPE strict")
conn.execute("MATCH (m: Movie) RETURN m.title")  # typechecks against strict

conn.execute("SHOW CURRENT GRAPH TYPE")
# → {"active": "strict", "nodes": 1, "edges": 0,
#    "formatted": "Node types:\n    (:Movie {title STRING})\n"}

conn.execute("VALIDATE GRAPH TYPE strict")
# → {"ok": False, "name": "strict",
#    "nodes_checked": 171, "edges_checked": 253,
#    "node_violations": 38, "edge_violations": 0,
#    "samples": [{"kind": "node", "id": 12, "labels": ["Person"], ...}, ...]}

conn.graph_types()
# → [{"name": "DEFAULT", "active": False, "nodes": 4, "edges": 6},
#    {"name": "strict",  "active": True,  "nodes": 1, "edges": 0}]
```

## Storage

GQLite uses a single `.gdb` file with 4KB pages:
- All node/edge IDs are `u32` internally (no string overhead at query time)
- Three storage backends: in-memory `Graph`, `LazyGraphStore` (topology in RAM, labels/props from disk via LRU page cache), `DiskGraphStore` (minimal RAM)
- The REPL uses `LazyGraphStore` for efficient memory usage on large graphs
- The GRAPH TYPE catalog persists in its own page chain, so `CREATE / USE / DROP GRAPH TYPE` survive close/reopen

See `docs/storage-architecture.md` for the full format specification.

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
`docs/JOIN_STRATEGY_NOTES.md` and `gqlrust/CLAUDE.md` for the algorithm
in detail.

## Building and Testing

```bash
cargo build --release                    # build all binaries

# Strict clippy (run before every commit)
cargo clippy --workspace --all-targets -- -D clippy::all

# Lib + integration sweep (~320 tests; bench_test is excluded — pre-existing failures)
cargo test --lib                         # 152 unit tests
cargo test --test parser_test --test runtime_test --test store_runtime_test \
           --test text2gql_test --test parse_and_run_test --test count_test \
           --test null_test --test record_test --test list_test \
           --test compile_diagnostics --test elaborate_test --test float_test \
           --test graph_type_test --test typecheck_smoke --test typecheck_test \
           --test optional_match_test --test multi_match_test \
           --test aggregates_proptest --test lattice_proptest --test multi_match_proptest

# Single test
cargo test --test runtime_test test_join_star_any_label -- --exact
```

Other binaries in `src/bin/`:
- `bench_queries` — generic benchmark runner
- `bench_setup` — downloads + extracts LDBC datasets (`bench/data/` is gitignored)
- `ldbc_bench` — LDBC interactive-complete driver, queries in `bench/ldbc-queries/*.toml`
- `typecheck_bench` — typechecker microbench
- `convert_edgelist` — edge-list format converter

## Documentation

- `docs/storage-architecture.md` — File format, page layout, storage backends, catalog persistence
- `docs/JOIN_STRATEGY_NOTES.md` — LTJ algorithm, triple decomposition, benchmark numbers
- `docs/implemented-optimizations.md` — u32 IDs, hash-join, label index, early termination, predicate pushdown
- `docs/possible-optimizations.md` — Fixed-length cells, sorted adjacency, future LTJ work
- `docs/iso-gql-gaps.md` — Tracked deviations from ISO GQL
- `docs/graph-type-catalog-plan.md` — Catalog design rationale
- `CLAUDE.md` — Internal architecture notes (compiler pipeline, typechecker, LTJ details)
