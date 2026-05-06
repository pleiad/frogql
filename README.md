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

Everything else is parsed as either a GQL query or a catalog DDL statement (see [Graph Types](#graph-types) below).

## Example Databases

The `examples/` directory contains ready-to-use `.gdb` files:

| Database | Nodes | Edges | Domain |
|----------|-------|-------|--------|
| movies.gdb | 171 | 253 | Movies (Person, Movie, ACTED_IN, DIRECTED, ...) |
| fraud_detection.gdb | 14,550 | 31,564 | Financial fraud (Transaction, Account, TRIGGERED_ALERT, ...) |
| bom.gdb | 1,500 | 2,248 | Bill of Materials (Product, Assembly, ConsistsOf, ...) |

Regenerate from CSV: `./target/release/frogql examples/movies.gdb --import-csv <csv_dir>`

## Data Import

froGQL reads graph data from three formats. Every import path also runs
`infer_simple_schema` and persists the result as the `DEFAULT` GRAPH TYPE,
so the typechecker is useful immediately after the first open.

**CSV with config** (Text2GQL / Cypher datasets):
```bash
frogql mydb.gdb --import-csv path/to/Spanner_Instance/
# Reads spanner_import_config.json + CSV files.
# Node CSVs: detected as files without SRC_ID/DST_ID columns; ID column
#   resolved by trying `vid`, `<Label>_id`, any `*_id`, then column 0.
# Edge CSVs: SRC_ID, DST_ID + property columns; edge label inferred from
#   the config `label` field with known node-type prefixes stripped.
# All column lookups are case-insensitive.
```

**LDBC SNB CSV-Basic**:
```bash
frogql mydb.gdb --import-ldbc-csv path/to/social_network-sf0.1-CsvBasic-LongDateFormatter/
# Three-pass loader:
#   1. nodes — each LDBC entity owns its id space (Place id 0 ≠ Organisation id 0),
#      so the loader keys node ids by (entity_label, external_id).
#   2. multi-valued attributes — files like Person_email_emailaddress.csv
#      collapse onto the owning node as a Value::List.
#   3. edges — directed by default, with property columns preserved.
```

**JSON**:
```bash
frogql mydb.gdb --import-json graph.json
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

### EXISTS / NOT EXISTS

Boolean predicates over a subquery body. Use them to keep or drop outer
rows based on whether the body has any match.

```
MATCH (p: Person) WHERE EXISTS { (p)-[:ACTED_IN]->(:Movie) } RETURN p.name
MATCH (p: Person) WHERE NOT EXISTS { (p)-[:DIRECTED]->(:Movie) } RETURN p.name
```

The body accepts one or more `MATCH` / `OPTIONAL MATCH` clauses with
`WHERE` filters. `RETURN`, `GROUP BY`, and `LIMIT` are not allowed
inside the braces — the body's job is proving non-emptiness, not
projecting a result table.

Outer-bound variables are visible inside via correlation; inner-only
bindings stay local. References to inner variables from outside the
body are rejected at compile time:

```
MATCH (p) WHERE EXISTS { (p)-[:KNOWS]->(b) } RETURN p.name, b.name
                                             ^^^^^^^^^^^^^^^^^^^^
                                             error: b not bound
```

When the active graph type proves the body unsatisfiable (a label or
property the schema rejects), the optimiser folds `EXISTS` to `false`
and `NOT EXISTS` to `true` and the runtime never evaluates the body.

Two regimes at runtime:
- **Uncorrelated** body (no shared variable with the outer scope) runs
  once with `limit=1`; the bool result is reused across every outer
  row.
- **Correlated** body runs once with no limit; rows are projected onto
  the correlation variables and stored in a `HashSet`. Per outer row
  the predicate is one O(1) hash probe — semi-join for `EXISTS`, anti-
  join for `NOT EXISTS`.

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

## Data Modification (ISO §13)

froGQL implements the full MVP-0 + MVP-1 surface of ISO/IEC 39075:2024
§13: `INSERT`, `SET`, `REMOVE`, `[DETACH | NODETACH] DELETE`, optional
`RETURN` after the DM, explicit `.save`, and pg_dump-style snapshots in
both JSON and GQL.

```
gql> INSERT (alice:Person {name: 'Alice', age: 30})
OK (1 nodes inserted, 0 edges inserted, 0 nodes deleted, 0 edges deleted, 1 rows; 0.000s)

gql> INSERT (bob:Person {name: 'Bob', age: 25})
gql> MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'})
     INSERT (a)-[:KNOWS {since: 2020}]->(b)
OK (0 nodes inserted, 1 edges inserted, 0 nodes deleted, 0 edges deleted, 1 rows; 0.000s)

gql> MATCH (a:Person {name: 'Alice'}) SET a.tier = 'gold', a:VIP
gql> MATCH (a:Person {name: 'Alice'}) REMOVE a.age
gql> MATCH (a:Person {name: 'Alice'}) DETACH DELETE a

gql> .save
Saved /tmp/fresh.gdb (0.001s).
```

Surface accepted today:

| Construct | Example |
|---|---|
| Standalone `INSERT` | `INSERT (a:Person {name: 'Alice'})` |
| Multiple paths in one statement | `INSERT (a:A), (b:B), (a)-[:E]->(b)` |
| `MATCH` + `INSERT` with bound vars and computed properties | `MATCH (a:Person) INSERT (b:Tag {who: a.name})` |
| `SET x.prop = expr` | `MATCH (a:Person) SET a.tier = 'gold'` |
| `SET x = { ... }` (clear+set per §13.3 GR8 b.i) | `MATCH (a:Person) SET a = { name: 'X', age: 7 }` |
| `SET x:Label` / `SET x IS Label` | `MATCH (a:Person) SET a:VIP` |
| `REMOVE x.prop` | `MATCH (a:Person) REMOVE a.age` |
| `REMOVE x:Label` / `REMOVE x IS Label` | `MATCH (a:Person) REMOVE a:VIP` |
| `[DETACH \| NODETACH] DELETE <expr list>` | `MATCH (a:Person) DETACH DELETE a, COALESCE(a.parent, a)` |
| Optional `RETURN` after the DM | `INSERT (n:Tag {n: 'x'}) RETURN n` |

ISO compliance highlights:
- `MATCH (a:Person) INSERT (a)-[:K]->(b:Tag)` creates **one fresh `b` per matched `a`** (§13.2 GR4), not a single shared Tag.
- `SET x = { ... }` clears every existing property of `x` first, then applies the new map (§13.3 GR8 b.i).
- `REMOVE x:Label` and `REMOVE x.prop` are idempotent — removing something the element does not carry is a no-op (§13.4 GR4 a/b).
- `DELETE` accepts any `<value expression>` (Feature GD04). A target evaluating to `Null` is a no-op (§13.5 GR4 a); anything other than a node or edge reference raises an error.
- `NODETACH DELETE` raises `G1001 dependent object error — edges still exist` when the node has incident edges; statements roll back atomically (§13.5 GR5 + Note 196).
- When a non-DEFAULT GRAPH TYPE is active, every inserted or mutated element is validated against it; mismatches raise `G2000 graph type violation` and the statement aborts. DEFAULT skips validation (it is data-derived) and gets re-inferred lazily on the next `SHOW GRAPH TYPE DEFAULT` / `USE GRAPH TYPE DEFAULT`.

Persistence and dumps:
- Mutations live in an **in-RAM overlay** until `.save` (or `connection.save()` from Python). No auto-commit, mirroring SQLite.
- `.save` writes to `<path>.tmp` and atomically renames over the destination, so a crash mid-save cannot corrupt the existing file.
- `.dump-json <path>` writes a JSON snapshot in the shape `--import-json` consumes.
- `.dump-gql <path>` writes a GQL script that, when executed against an empty database, reproduces the graph: one `INSERT` per node (with a synthetic `_dump_id` property), one `MATCH ... INSERT (a)-[:E {...}]->(b)` per edge, then a final `MATCH (n) REMOVE n._dump_id` cleanup. If `_dump_id` is already in use the dumper falls back to `__dump_id_v1`, `__dump_id_v2`, ....

Carve-outs that do **not** ship today:
- Multi-DML chains in a single statement (`MATCH α INSERT β SET γ`); deferred to v2. Tests work around it by splitting into separate `execute` calls.
- String values containing a literal `'` cannot survive `.dump-gql` (the lexer has no escape syntax); the dumper raises an error rather than emit something unparseable.
- Statement-level atomicity. The whole DML statement either commits to the overlay or rolls back; on failure the **entire session overlay** is discarded (no transaction boundary smaller than the connection until WAL).
- Secondary indexes (hash + btree) become read-only while the overlay is non-empty: `lookup_node_eq` / `lookup_node_range` return `None` so the caller scans. Incremental maintenance lands in MVP-2.

### Python bindings

```python
import frogql
conn = frogql.open("/tmp/fresh.gdb")  # opens or creates
conn.execute("INSERT (a:Person {name: 'Alice'})")
# → {"nodes_inserted": 1, "edges_inserted": 0, "nodes_deleted": 0, "edges_deleted": 0, "rows": 1}

conn.execute("MATCH (p:Person) RETURN p.name")
# → [{"name": "Alice"}]

conn.save()  # persist the overlay to /tmp/fresh.gdb
```

## Graph Types

froGQL has a persistent catalog of named graph types. The catalog lives
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
import frogql
conn = frogql.open("movies.gdb")
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

froGQL uses a single `.gdb` file with 4KB pages:
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

## Secondary indexes

froGQL auto-builds hash indexes on `(label, prop)` pairs whose values are
unique within the label, in a single O(N) pass at `LazyGraphStore::open`.
On the LDBC SF0.1 dataset that captures `Person.id`, `Tag.name`,
`Country.name`, `TagClass.name`, every other `*_id` column the loader
produced — 26 indexes in total, no DDL required. The LTJ optimizer
constant-folds any `NodeAttrCmp { Eq, value }` predicate that hits an
index, substitutes the resolved NodeId in every triple position, and
excludes the variable from the VEO so leapfrog never enumerates it.

Measured impact on **LDBC IC2** (`MATCH (p:Person {id: $personId})~[:knows]~...`
over `bench/data/ldbc-sf0.1.gdb`, 15 params × 3 iters, lazy backend,
`--limit 20`):

| | Median | Range |
|---|---|---|
| Without secondary index (`GQLITE_DISABLE_INDEX_FOLD=1`) | 2417 ms | 2317–2582 ms |
| With secondary index (default) | **1377 ms** | 1363–1392 ms |
| **Speedup** | **1.76×** | |

IC2 itself uses a top-level `Comment | Post` union that falls back to
hash-join, but each branch independently decomposes into LTJ-eligible
triples and benefits from the start-node pin. Diagnostic env vars:
`GQLITE_DEBUG_INDEXES=1` prints the auto-built indexes and pinned
variables; `GQLITE_DISABLE_INDEX_FOLD=1` reverts to the pre-index plan
for A/B benchmarking.

### Declared indexes (`CREATE INDEX` DDL)

For `(label, prop)` pairs the auto-builder doesn't cover (because the
values aren't unique), declare the index explicitly:

```
gql> CREATE BTREE INDEX msg_date ON :Message(creationDate);
INDEX 'msg_date' created (BTREE on (:Message {creationDate}), 286592 entries) in 0.31s.

gql> CREATE HASH INDEX person_first ON :Person(firstName);
INDEX 'person_first' created (HASH on (:Person {firstName}), 587 entries) in 0.01s.

gql> SHOW INDEXES;     -- or .indexes meta-command
gql> DROP INDEX msg_date;
```

Both prefix (`CREATE BTREE INDEX foo ...`) and suffix (`CREATE INDEX foo
... USING BTREE`) syntaxes are accepted; HASH is the default kind.
HASH and BTREE coexist on the same `(label, prop)` pair — they serve
different query patterns and the LTJ optimizer picks the right one per
filter.

The optimizer wires both kinds into the LTJ pre-pass:
- `NodeAttrCmp { Eq, value }` → hash lookup, constant-fold or NodeInSet.
- `NodeAttrCmp { <, <=, >, >=, value }` → btree range lookup,
  precomputed sorted set, replace the per-row property comparison with
  an O(log n) binary-search membership test (`FilterKind::NodeInSet`).

All indexes are in-memory (rebuilt every open). Persistence in the .gdb
file header chain — so declared indexes survive close/reopen — is the
next step on the roadmap.

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

- `docs/storage-architecture.md` — File format, page layout, storage backends, catalog persistence
- `docs/JOIN_STRATEGY_NOTES.md` — LTJ algorithm, triple decomposition, benchmark numbers
- `docs/implemented-optimizations.md` — u32 IDs, hash-join, label index, early termination, predicate pushdown
- `docs/possible-optimizations.md` — Fixed-length cells, sorted adjacency, future LTJ work
- `docs/iso-gql-gaps.md` — Tracked deviations from ISO GQL
- `docs/graph-type-catalog-plan.md` — Catalog design rationale
- `CLAUDE.md` — Internal architecture notes (compiler pipeline, typechecker, LTJ details)
