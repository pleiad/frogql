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

## graphqlite RETURN-clause property accessor breaks int64 values

**Symptom**: `RETURN p.ldbcId` and `RETURN p.creationDate` return
WRONG int values for properties that are 64-bit integers in the
LDBC dataset (Person.id, Comment.id, Post.id, all `creationDate` /
`joinDate` / `birthday` timestamps in milliseconds).

Verified directly against the loaded graphqlite SF0.1 DB:

```python
import graphqlite
g = graphqlite.Graph('bench/data/cross-system/graphqlite/ldbc-sf01.db')

# RETURN <node> serializes the WHOLE node correctly, all int64 props intact:
g.query('MATCH (p:Person {ldbcId: 24189255811566}) RETURN p')
# → [{'p': {'id': 26514, 'labels': ['Person'],
#          'properties': {'ldbcId': 24189255811566,
#                         'creationDate': 1322656837118, ...}}}]

# RETURN <node>.<int64-prop> returns garbage for the same property:
g.query('MATCH (p:Person {ldbcId: 24189255811566}) RETURN p.ldbcId AS lid')
# → [{'lid': 494}]    ← WRONG. 494 is the rowid of an unrelated
#                       Organisation node, not the LDBC id.

g.query('MATCH (p:Person {ldbcId: 24189255811566}) RETURN p.creationDate AS cd')
# → [{'cd': -193090050}]    ← WRONG. int32-truncated form of 1322656837118
#                              (cast to signed 32-bit overflows negative).
```

So graphqlite **stores** int64 correctly (the SQLite `node_props_int`
column is INTEGER, which is 64-bit) — the WHERE predicate in the
MATCH clause finds the right node, and `RETURN p` (whole-node
serialization path) preserves the int64 properties. The bug lives
specifically in the RETURN-clause `<var>.<prop>` projection path.

**Why not in graphqlite/DIVERGENCES.md as a "we work around it"?**
We don't. Two reasons:

1. **The bench's job is to measure what the engine does on the
   spec-faithful query.** Every system runs the spec form of IC2 etc.
   on its native dialect; the cross-system row-content hash oracle
   either confirms agreement or surfaces a real finding. graphqlite's
   IC2 result rows disagreeing with gqlite's and Kuzu's IS the real
   finding here — engineering teams shipping graphqlite-on-LDBC
   would hit this every time they project `.id` or `.creationDate`.
   Hiding it behind a bench-side workaround (e.g. switching graphqlite
   alone to `RETURN p` + post-processing in Python) would put
   graphqlite on a code path none of its users would write, and the
   resulting numbers would describe a system nobody actually runs.

2. **It's the same anti-pattern as the rejected schema-constrained
   Kuzu form** (`kuzu/DIVERGENCES.md` documents that whole story).
   Make-it-look-correct is not the same as correct.

**What this means for the bench output**: the cross-system
row-content equivalence section ("Row-content equivalence" in
`comparison.txt`) will report graphqlite hash mismatches against
gqlite + Kuzu on every IC. The mismatches are real per-system
behaviour, not bench bugs. gqlite ↔ Kuzu hashes should still agree
(both have correct int64 RETURN). The MISS / WARN lines for
graphqlite point at this divergence file.

**Tested versions**: graphqlite 0.4.4 (PyPI, `pip install graphqlite`).
If a future version fixes int64 RETURN, this divergence becomes
moot and the cross-system comparison rises to 3/3 systems.

## IC11 sub-types encoded as `type` property, not as labels

The LDBC SNB IC11 spec references `:Company` and `:Country` as
schema-level sub-labels of `:Organisation` and `:Place`. Our
graphqlite loader (`setup.py`) loads each Organisation node with
label `"Organisation"` and each Place node with label `"Place"`,
storing the sub-type as a regular property column (`type`) on the
node. graphqlite's `insert_nodes_bulk` API takes a single label per
node and there's no schema-level multi-label or sub-label primitive.
The cross-system IC11 cypher filters by the `type` property instead
of matching the sub-label directly:

```cypher
MATCH ... -[:workAt]-> (company:Organisation) -[:isLocatedIn]-> (country:Place)
WHERE company.type = 'company' AND country.type = 'country' AND ...
```

Pre-fix the cypher used the spec form `(:Company) ... (:Country)`
directly; graphqlite didn't error on the unknown labels but every
match failed silently → 0 rows on every parameter row. Post-fix the
query is structurally correct, but graphqlite still returns 0/15
because of a separate runtime bug in undirected variable-length
expansion (see next section). The encoding-of-sub-types fix is
required regardless of that runtime bug — without it the cypher
could never match anything.

The semantic effect is identical to gqlite's `:Company` / `:Country`
match: gqlite's LDBC loader synthesizes the sub-label via
`LabelType::And` on the same `type` column at load time; ours keeps
it flat as a property. Documented inline in `ic11.cypher`.

## graphqlite undirected variable-length expansion `[:rel*N..M]-` follows only outgoing edges

**Symptom**: `MATCH (p:Person {ldbcId: $pid})-[:knows*1..2]-(o:Person) RETURN count(o)`
returns 0 for some Persons whose 1-hop count is non-zero. Concretely,
for `pid=24189255811707` (LDBC SF0.1):

```
1-hop knows (no quantifier): 11 friends
1-hop knows forward only:    0
1-hop knows reverse only:    11
[:knows*1..1]- (undirected):  0  ← BUG. should be 11.
[:knows*1..2]- (undirected):  0  ← BUG.
[:knows*1..2]-> (forward):    0
<-[:knows*1..2]- (reverse):   373
```

So `-[:rel]-` (no quantifier) is correctly bidirectional, but
`-[:rel*N..M]-` (variable-length, no arrow heads) is silently
forward-only. For Persons whose `knows` edges all live in the
reverse direction in the underlying SQLite table (an artifact of
LDBC storing each pair once in one direction), the variable-length
expansion finds zero friends.

**Effect on the bench**: IC11 has been re-translated to use the
`type` column on `:Organisation` / `:Place` (the loader doesn't
synthesize `:Company` / `:Country` sub-labels — see "encoding of
sub-types" in each ic-cypher comment block). After that fix, Kuzu
and gqlite produce byte-identical IC11 rows, but graphqlite still
returns 0 on the one IC11 parameter row where the expected result
is non-empty (LDBC interactive_11_param.txt row 1, person
`24189255811707`). The graphqlite outlier on this row is a
graphqlite runtime bug, not a translation bug.

**Tested versions**: graphqlite 0.4.4. Reproduces with the literal
queries above; not yet reported upstream.
