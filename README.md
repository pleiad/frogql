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
gql> schema
Node labels:
  :Person (133 nodes)
  :Movie (38 nodes)
Edge labels:
  :ACTED_IN (172 edges)
  :DIRECTED (44 edges)
  ...

gql> MATCH (p: Person) -[:ACTED_IN]-> (m: Movie) WHERE m.released = 1999 RETURN p.name, m.title
p.name | m.title
-------+--------
"Keanu Reeves" | "The Matrix"
"Carrie-Anne Moss" | "The Matrix"
...

gql> MATCH (a: Person) -[:ACTED_IN]-> (m: Movie), (d: Person) -[:DIRECTED]-> (m) RETURN a.name, d.name
```

## Example Databases

The `examples/` directory contains ready-to-use `.gdb` files:

| Database | Nodes | Edges | Domain |
|----------|-------|-------|--------|
| movies.gdb | 171 | 253 | Movies (Person, Movie, ACTED_IN, DIRECTED, ...) |
| fraud_detection.gdb | 14,550 | 31,564 | Financial fraud (Transaction, Account, TRIGGERED_ALERT, ...) |
| bom.gdb | 1,500 | 2,248 | Bill of Materials (Product, Assembly, ConsistsOf, ...) |

Regenerate from CSV: `./target/release/gqlite examples/movies.gdb --import-csv <csv_dir>`

## Data Import

GQLite reads graph data from three formats:

**CSV with config** (Text2GQL / Cypher datasets):
```bash
gqlite mydb.gdb --import-csv path/to/Spanner_Instance/
# Reads spanner_import_config.json + CSV files
# Node CSVs: id column + property columns
# Edge CSVs: SRC_ID, DST_ID + property columns
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

After import, the `.gdb` file contains everything. No need for the source files.

## Query Language

### Full queries

```
MATCH <pattern> WHERE <condition> RETURN <expressions>
```

All clauses except the pattern are optional. `MATCH` keyword is also optional.

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
```

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

`:graph-types` (REPL meta-command) is a shorthand for `SHOW GRAPH TYPES`.

While a type is active the typechecker rejects (or empties out) queries
whose labels and properties don't fit. With no active type the checker
stays permissive (`Schema::star()`).

`DEFAULT` is reserved. It always represents the schema inferred from the
current graph data, refreshed on demand:

```
gql> USE GRAPH TYPE DEFAULT;
GRAPH TYPE 'DEFAULT' refreshed (4 node types, 6 edge types) and activated.
```

`gqlite db.gdb --import-csv ...` and `--import-json ...` populate and
activate `DEFAULT` automatically. `CREATE GRAPH TYPE DEFAULT` and
`DROP GRAPH TYPE DEFAULT` are rejected.

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
- Three storage backends: in-memory `Graph`, `LazyGraphStore` (topology in RAM, data on disk), `DiskGraphStore` (minimal RAM)
- The REPL uses `LazyGraphStore` for efficient memory usage on large graphs

See `docs/storage-architecture.md` for the full format specification.

## Building and Testing

```bash
cargo build --release                    # build all binaries
cargo test                               # run all 129 tests

# Specific test suites
cargo test --test parser_test            # 42 parser tests
cargo test --test runtime_test           # 38 runtime tests
cargo test --test store_runtime_test     # 31 store tests
cargo test --test text2gql_test          # 18 integration tests on Movies dataset
```

## Documentation

- `docs/storage-architecture.md` — File format, page layout, storage backends
- `docs/implemented-optimizations.md` — u32 IDs, hash-join, label index, early termination
- `docs/possible-optimizations.md` — Fixed-length cells, sorted adjacency, LTJ
- `bench/JOIN_STRATEGY_NOTES.md` — Join strategy analysis and LTJ comparison
