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
