# Data Modification (ISO §13)

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

## Surface accepted today

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

## ISO compliance highlights

- `MATCH (a:Person) INSERT (a)-[:K]->(b:Tag)` creates **one fresh `b` per matched `a`** (§13.2 GR4), not a single shared Tag.
- `SET x = { ... }` clears every existing property of `x` first, then applies the new map (§13.3 GR8 b.i).
- `REMOVE x:Label` and `REMOVE x.prop` are idempotent — removing something the element does not carry is a no-op (§13.4 GR4 a/b).
- `DELETE` accepts any `<value expression>` (Feature GD04). A target evaluating to `Null` is a no-op (§13.5 GR4 a); anything other than a node or edge reference raises an error.
- `NODETACH DELETE` raises `G1001 dependent object error — edges still exist` when the node has incident edges; statements roll back atomically (§13.5 GR5 + Note 196).
- When a non-DEFAULT GRAPH TYPE is active, every inserted or mutated element is validated against it; mismatches raise `G2000 graph type violation` and the statement aborts. DEFAULT skips validation (it is data-derived) and gets re-inferred lazily on the next `SHOW GRAPH TYPE DEFAULT` / `USE GRAPH TYPE DEFAULT`.

## Persistence and dumps

- Mutations live in an **in-RAM overlay** until `.save` (or `connection.save()` from Python). No auto-commit, mirroring SQLite.
- `.save` writes to `<path>.tmp` and atomically renames over the destination, so a crash mid-save cannot corrupt the existing file.
- `.dump-json <path>` writes a JSON snapshot in the shape `--import-json` consumes.
- `.dump-gql <path>` writes a GQL script that, when executed against an empty database, reproduces the graph: one `INSERT` per node (with a synthetic `_dump_id` property), one `MATCH ... INSERT (a)-[:E {...}]->(b)` per edge, then a final `MATCH (n) REMOVE n._dump_id` cleanup. If `_dump_id` is already in use the dumper falls back to `__dump_id_v1`, `__dump_id_v2`, ....

## Carve-outs

These do **not** ship today:

- Multi-DML chains in a single statement (`MATCH α INSERT β SET γ`); deferred to v2. Tests work around it by splitting into separate `execute` calls.
- String values containing a literal `'` cannot survive `.dump-gql` (the lexer has no escape syntax); the dumper raises an error rather than emit something unparseable.
- Statement-level atomicity. The whole DML statement either commits to the overlay or rolls back; on failure the **entire session overlay** is discarded (no transaction boundary smaller than the connection until WAL).
- Secondary indexes (hash + btree) become read-only while the overlay is non-empty: `lookup_node_eq` / `lookup_node_range` return `None` so the caller scans. Incremental maintenance lands in MVP-2.

## Python bindings

```python
import frogql
conn = frogql.open("/tmp/fresh.gdb")  # opens or creates
conn.execute("INSERT (a:Person {name: 'Alice'})")
# → {"nodes_inserted": 1, "edges_inserted": 0, "nodes_deleted": 0, "edges_deleted": 0, "rows": 1}

conn.execute("MATCH (p:Person) RETURN p.name")
# → [{"name": "Alice"}]

conn.save()  # persist the overlay to /tmp/fresh.gdb
```
