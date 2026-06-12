// Cypher translation of bench/ldbc-queries/ic6.toml for graphqlite
// (colliery-io/graphqlite 0.4.4 from PyPI).
//
// VERDICT: UNSUPPORTED — same root cause as IC5. IC6 needs a bidirectional
// 1..2-hop friend set AND a per-tag grouped COUNT. In graphqlite 0.4.4 these
// are mutually exclusive: the correct friend set forces a UNION, and grouped
// aggregation cannot cross a UNION. 0/15 row-hash match.
//
// IC6 ("Tag co-occurrence") ranks tags that co-occur (on the same posts) with
// a given tag, across the posts authored by a person's friends/FoF, by post
// count.
//
// ROOT-CAUSE (verified 2026-06-12 against the loaded SF0.1 DB):
//
//   1. `~[:knows]~{1,2}` is a bidirectional variable-length walk; graphqlite's
//      undirected `-[:knows*1..2]-` is silently FORWARD-ONLY (DIVERGENCES.md,
//      ic9.cypher). Correct expansion = UNION ALL of a 1-hop + 2-hop branch of
//      bidirectional single-hop `-[:knows]-`.
//
//   2. The RETURN groups per co-occurring tag: `WITH otherTag, count(post)`.
//      graphqlite groups ONLY through `WITH <key>, <agg>` (verified). But
//      grouping cannot cross a UNION:
//        - `WITH` inside a UNION branch -> "no such table: _with_0" (verified).
//        - bare `RETURN otherTag.name, count(post)` does NOT group — it
//          collapses ALL rows to a SINGLE global-aggregate row mislabeled with
//          the first row's tag name (verified directly: the IC6 2-hop chain
//          returns 71 distinct non-aggregate rows, but adding `count(*)`
//          yields 1 row {Rainer_Schüttler: 71}). This is THE finding that
//          overturns the prior pass's "bare RETURN groups" assumption.
//        - no CALL{}/aggregation over a UNION to merge per-tag counts across
//          the 1-hop and 2-hop branches.
//
//   So WITH-grouping needs a non-UNION pattern, but the correct friend set
//   needs UNION — unsatisfiable. The forward-only `*1..2` + WITH form runs but
//   returns the wrong friend set (0 rows for the row-0 person whose knows are
//   all reverse).
//
// Best-effort shipped below: WITH-grouped over the forward-only `*1..2`
// (correct grouping machinery, WRONG friend set). The comma-join of the two
// hasTag patterns is kept (gqlite's typechecker reason; graphqlite accepts
// either two MATCH clauses or a comma-join — both group the same). It runs and
// is grouped, but the friend membership is wrong, so the result diverges from
// the gqlite reference on every param. `.ldbcId`, lowercase edge labels are
// the usual graphqlite divergences.
MATCH (person:Person {ldbcId: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
MATCH (otherPerson)<-[:hasCreator]-(post:Post)-[:hasTag]->(tag:Tag {name: $tagName})
MATCH (post)-[:hasTag]->(otherTag:Tag)
WHERE otherTag <> tag
WITH otherTag, count(post) AS postCount
RETURN otherTag.name AS tagName,
       postCount
ORDER BY postCount DESC, tagName ASC
LIMIT 10
