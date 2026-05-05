# graphqlite — divergences from spec / gqlite

## graphqlite reserves `.id` for the loader's external_id

**Symptom**: `MATCH (p:Person) RETURN p.id` returns `"Person:933"`
(string, the prefixed external_id we passed to
`Graph.insert_nodes_bulk`), not `933` (the int LDBC id we tried to
store under `props["id"]`).

**Why**: graphqlite's API binds the first tuple element of
`insert_nodes_bulk(nodes)` to the node's identity. The Cypher
accessor `node.id` reads from that, not from `props["id"]`. Setting
`props["id"]` in the dict is silently overwritten by the loader.
Verified by:

```python
g.insert_nodes_bulk([("Person:933", {"id": 933, "ldbcId": 933}, "Person")])
g.query("MATCH (p:Person) RETURN p.id, p.ldbcId")
# → {'p.id': 'Person:933', 'p.ldbcId': 933}
```

**Why we prefix at all**: LDBC IDs are unique within a label only.
Tag/TagClass/Place IDs all start at 0 and overlap across labels —
~10K cross-label collisions in SF0.1. `insert_edges_bulk` takes ONE
flat external_id → rowid map, so without prefixing the map silently
corrupts. Per-label prefixing is required. Removing the prefix is
not an option.

**The fix in this bench**: store the int LDBC id under a non-
conflicting prop name `ldbcId`. graphqlite preserves it. Cypher
queries reference `.ldbcId` instead of `.id`. gqlite and Kuzu both
use `.id` directly because they don't have this naming conflict.

This means every per-IC translation in this directory has a small
graphqlite-specific divergence from the gqlite/Kuzu translations:

| System | Start predicate | Returned id columns |
|---|---|---|
| gqlite | `(p:Person {id: $personId})` | `friend.id`, `c.id` |
| Kuzu | `(p:Person {id: $personId})` | `friend.id`, `c.id` |
| graphqlite | `(p:Person {ldbcId: $personId})` | `friend.ldbcId AS friend_id`, `c.ldbcId AS c_id` |

The AS-aliases in graphqlite's RETURN keep the per-system CSV column
headers identical (`friend_id`, `c_id`) so the cross-system comparison
machinery (compare_results.py) doesn't need per-system column maps.

**How we caught the bug**: while investigating an unrelated 2.7×
speedup of graphqlite between phase-0 and the canonical bench
(eventually traced to SQLite query-plan stats), we noticed the
canonical bench's `friend.id` was returning `"Person:933"` not `933`.
The bench's per-iter shape verification (`shape=s,s,s,s,s,i status=fail`
vs expected `i,s,s,i,s,i`) had been logging this on every IC2 row
across every run, but `comparison.txt` only surfaced count-based
consistency, so the type mismatch went unnoticed for several runs.
After the fix, both `setup.py` (storing `ldbcId`) and
`compare_results.py` (now surfacing shape failures into
comparison.txt) close the loophole.

**Doesn't affect latency**: the engine work graphqlite does to
produce these strings is identical to the work it would do for ints
— `friend.ldbcId` is a property lookup of the same shape. The
canonical 11.04 ms cross-row median measured before the fix is
unchanged after the fix (modulo run-to-run variance).
