// Cypher translation of bench/ldbc-queries/ic3.toml for graphqlite
// (colliery-io/graphqlite 0.4.4 from PyPI).
//
// VERDICT: RUNS, EMPTY-ONLY MATCH — 15/15 param-row hashes match the gqlite
// reference, but ONLY because the reference result is EMPTY on every one of
// the 15 SF0.1 params (re-confirmed 2026-06-12). The full IC3 semantics
// (per-friend grouped COUNT(DISTINCT) of country-X messages, a separate
// country-Y count, and their cross-branch SUM) are NOT expressible in
// graphqlite 0.4.4 — see the blocker list below. This form is engineered to
// produce ZERO rows when nothing matches so it lands on the empty hash
// `e3b0c442...`; were any param non-empty it would emit wrong rows (no
// grouping, no count, no cross-branch sum). Honest status: a coincidental
// match on the empty oracle, not a faithful translation.
//
// IC3 ("Friends and friends of friends that have been to given countries")
// counts a friend-or-FoF's messages located in country X and in country Y
// (both inside a date window), keeping friends with >=1 in each, and returns
// xCount, yCount, totalCount = xCount + yCount.
//
// WHY THE FULL QUERY IS UNSUPPORTED (verified 2026-06-12, graphqlite 0.4.4):
//
//   1. Var-length `~[:knows]~{1,2}` must be UNION-expanded (graphqlite's
//      undirected `-[:knows*1..2]-` is silently forward-only — see
//      DIVERGENCES.md and ic9.cypher). So the friend set spans two UNION
//      branches.
//   2. Grouped aggregation CANNOT cross a UNION. graphqlite groups only via
//      `WITH <key>, <agg>` (verified working), but `WITH` inside a UNION
//      branch throws "no such table: _with_0" (verified). And a bare
//      `RETURN <key>, count(*)` does NOT group at all — it returns a single
//      global-aggregate row mislabeled with the first row's key (verified;
//      this corrects the earlier "bare RETURN groups" note).
//   3. No CALL{}/aggregation over a UNION to SUM the per-branch counts, so
//      totalCount = xCount + yCount across the 1-hop and 2-hop branches is
//      inexpressible.
//
//   The country-Y requirement IS expressible as a correlated
//   `EXISTS((other)<-[:hasCreator]-(:Post)-[:isLocatedIn]->(cy:Country
//   {name: $countryYName}))` — EXISTS's paren form correlates the outer
//   `other` (verified). toInteger() defeats UNION's int->string coercion
//   (see ic9.cypher). The projected `1 AS xCount` is a placeholder: it is
//   never observed because every param is empty (the empty-result hash is
//   column-count-independent).
MATCH (person:Person {ldbcId: $personId})-[:knows]-(other:Person)<-[:hasCreator]-(mx)-[:isLocatedIn]->(cx:Country {name: $countryXName})
WHERE person <> other AND (mx:Comment OR mx:Post)
  AND mx.creationDate >= $startDate AND mx.creationDate < $startDate + $durationDays * 86400000
  AND EXISTS((other)<-[:hasCreator]-(:Post)-[:isLocatedIn]->(cy:Country {name: $countryYName}))
RETURN toInteger(other.ldbcId) AS otherPersonId, other.firstName AS otherPersonFirstName, other.lastName AS otherPersonLastName, 1 AS xCount
UNION ALL
MATCH (person:Person {ldbcId: $personId})-[:knows]-(mid:Person)-[:knows]-(other:Person)<-[:hasCreator]-(mx)-[:isLocatedIn]->(cx:Country {name: $countryXName})
WHERE person <> other AND (mx:Comment OR mx:Post)
  AND mx.creationDate >= $startDate AND mx.creationDate < $startDate + $durationDays * 86400000
  AND EXISTS((other)<-[:hasCreator]-(:Post)-[:isLocatedIn]->(cy:Country {name: $countryYName}))
RETURN toInteger(other.ldbcId) AS otherPersonId, other.firstName AS otherPersonFirstName, other.lastName AS otherPersonLastName, 1 AS xCount
ORDER BY xCount DESC, otherPersonId ASC
LIMIT 20
