// Cypher translation of bench/ldbc-queries/ic3.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic3.toml. Friends + FoF who
// posted in both country X and country Y within a date window.
//
// Neo4j-specific divergences (see DIVERGENCES.md):
//   - `(messageX:Comment|Post)` — native label-expression disjunction.
//   - No `:City` / `:Country` sub-labels: `Place` is flat with a
//     `type` property ('city' / 'country' / 'continent').
//   - `~[:knows]~{1,2}` → `-[:knows*1..2]-` (any-direction VLR over
//     single-direction storage — finds each stored edge both ways).
//   - `DURATION({days: N})` desugars to `N*86400000` ms; creationDate
//     is stored as int epoch-millis, so the window bound is plain int
//     arithmetic.
//   - GROUP BY is implicit on the non-aggregated RETURN terms.
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
MATCH (otherPerson)-[:isLocatedIn]->(city:Place)-[:isPartOf]->(country:Place)
WHERE city.type = 'city' AND country.type = 'country'
  AND country.name <> $countryXName AND country.name <> $countryYName
MATCH (otherPerson)<-[:hasCreator]-(messageX:Comment|Post)-[:isLocatedIn]->(cx:Place)
WHERE cx.type = 'country' AND cx.name = $countryXName
  AND messageX.creationDate >= $startDate
  AND messageX.creationDate < $startDate + $durationDays * 86400000
MATCH (otherPerson)<-[:hasCreator]-(messageY:Comment|Post)-[:isLocatedIn]->(cy:Place)
WHERE cy.type = 'country' AND cy.name = $countryYName
  AND messageY.creationDate >= $startDate
  AND messageY.creationDate < $startDate + $durationDays * 86400000
RETURN otherPerson.id AS otherPersonId,
       otherPerson.firstName AS otherPersonFirstName,
       otherPerson.lastName AS otherPersonLastName,
       count(DISTINCT messageX) AS xCount,
       count(DISTINCT messageY) AS yCount,
       count(DISTINCT messageX) + count(DISTINCT messageY) AS totalCount
ORDER BY totalCount DESC, otherPersonId ASC
LIMIT 20
