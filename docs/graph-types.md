# Graph Types

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
`SHOW GRAPH TYPE DEFAULT` and `SHOW GRAPH TYPES` respectively.

While a type is active the typechecker rejects (or empties out) queries
whose labels and properties don't fit. With no active type the checker
stays permissive (`Schema::star()`). To open the REPL with the
typechecker disabled, pass `--no-typecheck`.

## DEFAULT (auto-inferred)

`DEFAULT` is reserved. It always represents the schema inferred from the
current graph data, refreshed on demand:

```
gql> USE GRAPH TYPE DEFAULT;
GRAPH TYPE 'DEFAULT' refreshed (4 node types, 6 edge types) and activated.
```

All three import modes (`--import-csv`, `--import-ldbc-csv`,
`--import-json`) populate and activate `DEFAULT` automatically.
`CREATE GRAPH TYPE DEFAULT` and `DROP GRAPH TYPE DEFAULT` are rejected.

## Validating data against a type

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

## Type expressions

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

## Python bindings

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
