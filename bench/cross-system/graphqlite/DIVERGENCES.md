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

## graphqlite GROUP BY / aggregation reality (corrected 2026-06-12)

An earlier pass recorded that graphqlite "groups implicitly on a bare
`RETURN x, count(*)`" and that "`WITH` + aggregation is broken." Direct
re-verification against the loaded SF0.1 DB inverts both claims. The
accurate behaviour of graphqlite 0.4.4:

- **There is NO `GROUP BY` clause.** `RETURN ... GROUP BY x` is a syntax
  error (`unexpected IDENTIFIER, expecting end of file`).
- **A bare `RETURN <key>, <agg>` does NOT group.** It returns a SINGLE
  global-aggregate row, mislabeled with the FIRST input row's key value.
  Verified: `MATCH (p:Person) RETURN p.gender, count(*)` → one row
  `{male, 1528}` (1528 = the total person count, not the male count).
  The IC6 2-hop chain returns 71 distinct non-aggregate rows, but adding
  `count(*)` collapses to one row `{Rainer_Schüttler, 71}`.
- **Grouping WORKS through the first `WITH <key>, <agg>`.** `MATCH (p:Person)
  WITH p.gender AS g, count(*) AS c RETURN g, c` → `[{female,778},{male,750}]`.
  An `OPTIONAL MATCH` before the grouping `WITH` is fine. This is the
  canonical Cypher idiom and it is correct in graphqlite.
- **A SECOND aggregating `WITH` (after `ORDER BY`) does NOT re-group.** The
  arg-max idiom `WITH liker, l ORDER BY ... WITH liker, head(collect(...))`
  collapses to one global row — blocks IC7's per-liker latest-like.
- **`WITH` inside a UNION branch fails** with `no such table: _with_0`. So
  grouped aggregation CANNOT cross a UNION.
- **`collect(DISTINCT x)` does NOT dedupe.** `collect(DISTINCT p.browserUsed)`
  → `["Chrome","Chrome"]`. Blocks IC12's `COLLECT_LIST(DISTINCT tag.name)`.
- **Aggregating a 64-bit EDGE property returns NULL.** `max(l.creationDate)`
  on the `:likes` edge → NULL inside a grouped `WITH` (node-property `max`
  works). Compounds IC7's arg-max blocker.
- **A grouped RETURN carrying a `collect()` column STRINGIFIES every column**
  (int keys come back as `str`, the list as a JSON string). Blocks IC12 even
  apart from the dedup bug.

The net consequence: any IC that needs BOTH a bidirectional variable-length
friend set (which must be UNION-expanded, see below) AND a grouped aggregate
is unsatisfiable in graphqlite 0.4.4 — the friend set forces a UNION and the
grouping can't cross it. That is IC1, IC5, IC6. IC7 and IC12 are blocked by
the arg-max / collect-DISTINCT bugs independently.

## Verified row-equivalence status (2026-06-12, graphqlite 0.4.4)

Validated against gqlite's ROW hashes (1 iter, full SF0.1). All 14 ICs with
an `implemented` toml were run; counts are exact param-row hash matches.

| IC | n/15 | verdict | evidence / root cause |
|---|---|---|---|
| IC1 | 1/15 | UNSUPPORTED | needs bidirectional `knows*1..3` (→ UNION) AND grouped `COLLECT_LIST(RECORD{...})`; UNION+WITH-grouping unsatisfiable; `collect(DISTINCT)` also broken. The 1 match is row 5 (reference empty). |
| IC2 | 15/15 ✅ | VERIFIED | named-node + `:Comment OR :Post` workaround; non-aggregating projection. |
| IC3 | 15/15 ✅* | RUNS (empty-only) | reference EMPTY on all 15 params; non-aggregate UNION form lands on the empty hash. Full grouped cross-branch SUM is inexpressible — match is coincidental on the empty oracle. |
| IC4 | — | UNSUPPORTED | `EXISTS` has no `WHERE` clause; date-restricted anti-join inexpressible (best-effort un-dated `NOT EXISTS` → 0 rows on every param). |
| IC5 | 2/15 | UNSUPPORTED | bidirectional `knows*1..2` (→ UNION) AND `WITH forum, count(post)` grouping — unsatisfiable together. Best-effort forward-only `*1..2` returns wrong friend set. The 2 matches are the 2 empty-result params. |
| IC6 | 0/15 | UNSUPPORTED | same root cause as IC5 (UNION friend set vs WITH-grouping per `otherTag`). |
| IC7 | 0/15 | UNSUPPORTED | per-liker arg-max: second-stage `WITH`+`head(collect())` doesn't re-group; edge-property `max` → NULL; no VALUE subquery / RECORD. |
| IC8 | 15/15 ✅ | VERIFIED | non-aggregating reply projection. |
| IC9 | 15/15 ✅ | VERIFIED | var-length `~{1,2}` → 1-hop+2-hop UNION ALL of bidirectional single hops; `toInteger()` casts. Non-aggregating. |
| IC11 | 14/15 | VERIFIED (1 engine outlier) | `type`-property sub-label encoding + var-length. Row 1 (`24189255811707`) is a forward-only `*1..2` runtime-bug outlier. |
| IC12 | 1/15 | RUNS-WRONG | correct friends/counts/order via `WITH friend, count(comment)`; var-length elided (all 15 targets are isSubclassOf-leaves, 0-hop suffices); but `collect(DISTINCT tag.name)` duplicates AND grouped-collect rows stringify all columns → list+int cells diverge. The 1 match is row 6 (reference empty). |
| IC13 | VERIFIED | VERIFIED | bounded fixed-length probe (shortestPath hangs). See ic13.cypher. |

\* IC3's 15/15 is an empty-only match; documented honestly in ic3.cypher.

**Why IC5/IC6 were NOT fixed by the var-length UNION workaround.** That
workaround (proven on IC9/IC11) only works for NON-aggregating projections:
IC9 and IC11 just project rows, so a UNION ALL of hop-branches reproduces the
multiset directly. IC5/IC6 additionally GROUP-and-COUNT over that multiset,
and graphqlite cannot aggregate across a UNION (`_with_0` error for WITH;
global-collapse for bare RETURN). So the workaround that lifted IC9 to 15/15
cannot lift IC5/IC6 — they move from RUNS-WRONG to UNSUPPORTED, with the
deeper "no grouped aggregation over a UNION" root cause now documented above.

The non-VERIFIED mismatches are graphqlite **engine** limitations (broken
grouping/collect/edge-aggregation, forward-only undirected var-length, no
EXISTS-WHERE) — the class of defect the row-equivalence oracle exists to
catch. Latency numbers for those (system, IC) cells must NOT be quoted as
comparable: the engine is doing different (and usually less, or wrong) work.
