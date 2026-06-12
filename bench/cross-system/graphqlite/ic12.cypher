// Cypher translation of bench/ldbc-queries/ic12.toml for graphqlite
// (colliery-io/graphqlite 0.4.4 from PyPI).
//
// VERDICT: RUNS-WRONG — the query runs and produces the correct friends,
// reply counts, and ordering, but the `tagNames` list column DIVERGES from
// the gqlite reference because graphqlite 0.4.4's `collect(DISTINCT x)` does
// NOT dedupe. gqlite emits `List([Str("Michael_Jordan")])` (1 element);
// graphqlite emits `["Michael_Jordan","Michael_Jordan","Michael_Jordan",
// "Michael_Jordan"]` (one copy per contributing comment). Every non-empty
// param therefore hash-diverges on the list cell. 0/15 row-hash match.
//
// IC12 ("Expert search") counts a friend's Comment replies to Posts tagged
// under a given TagClass (or any subclass), grouped per friend, with the set
// of matched tag names.
//
// WHAT WORKS HERE (verified 2026-06-12 against the loaded SF0.1 DB):
//   - Grouped aggregation via `WITH friend, count(comment), collect(...)`
//     groups correctly per friend (graphqlite groups on a `WITH <key>, <agg>`
//     projection — verified). The 0-hop friend/reply structure matches the
//     reference exactly (row0: friend 8796093023000, replyCount 4).
//   - `~[:knows]~` is a SINGLE-hop bidirectional edge (no quantifier), so the
//     forward-only variable-length bug does NOT apply — `-[:knows]-` is
//     correctly bidirectional. No UNION needed, so WITH-grouping is available.
//
// TRANSLATION SIMPLIFICATION (verified safe on all 15 params):
//   The toml's `-[:isSubclassOf]->{0,}(target)` walks UP the TagClass DAG.
//   On SF0.1, EVERY one of the 15 IC12 target TagClasses (BasketballPlayer,
//   Chancellor, MilitaryUnit, GolfPlayer) is a LEAF with ZERO isSubclassOf
//   descendants (verified: `(tc)-[:isSubclassOf*1..N]->(target)` returns 0
//   for all 15). So only the 0-hop case (tag's tagClass IS the target) ever
//   contributes, and the var-length is a no-op. We therefore match the
//   target tagClass directly: `(tag)-[:hasType]->(:TagClass {name})`. This
//   sidesteps a SECOND graphqlite bug — `-[:isSubclassOf*0..N]->` OMITS the
//   zero-length path (verified: `(BasketballPlayer)-[:isSubclassOf*0..3]->`
//   returns Athlete/Person/Agent but NOT BasketballPlayer itself), so the
//   spec form would return 0 friends on every param.
//
// THE BLOCKERS (two, both list/collect related):
//   1. `collect(DISTINCT tag.name)` returns duplicates in 0.4.4. Verified on a
//      minimal case: `collect(DISTINCT p.browserUsed)` -> `["Chrome","Chrome"]`.
//      A pre-`WITH DISTINCT friend, tag, comment` does not help (each comment
//      is a distinct row; the same tag repeats per comment). There is no
//      in-engine way to dedupe the list, so the list cell can't match
//      gqlite's deduped COLLECT_LIST(DISTINCT ...).
//   2. A grouped RETURN that carries a `collect()` column STRINGIFIES EVERY
//      column of that result: friendId, replyCount come back as Python `str`
//      ('8796093023000', '4'), and tagNames as a JSON STRING
//      ('["Michael_Jordan",...]') — verified. So the int columns (gqlite
//      types them `i`) also hash-diverge, independent of the dedup bug.
//      `toInteger()` does NOT override this (the value is already a string
//      by the time the accessor sees it). Masking either bug in run.py
//      (json.loads + dedupe + int-cast) would put graphqlite on a code path
//      its users never write — the rejected make-it-look-correct anti-pattern.
//
// Shipped form is the best attainable: correct friends/counts/order, wrong
// (duplicated, stringified) list + int columns. The `.ldbcId` accessor,
// lowercase edge labels, and `:TagClass {name}` are the other divergences.
MATCH (person:Person {ldbcId: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(comment:Comment)-[:replyOf]->(post:Post)-[:hasTag]->(tag:Tag)-[:hasType]->(tagClass:TagClass {name: $tagClassName})
WITH friend, count(comment) AS replyCount, collect(DISTINCT tag.name) AS tagNames
RETURN friend.ldbcId AS friendId,
       friend.firstName AS friendFirstName,
       friend.lastName AS friendLastName,
       tagNames,
       replyCount
ORDER BY replyCount DESC, friendId ASC
LIMIT 20
