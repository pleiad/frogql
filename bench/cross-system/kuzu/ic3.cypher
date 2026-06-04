// openCypher translation of bench/ldbc-queries/ic3.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic3.toml. Friends + FoF who
// posted in both country X and country Y within a date window.
//
// Kuzu-specific divergences (see DIVERGENCES.md for the full audit):
//   - `(messageX:Comment|Post)` pattern alternation is unsupported;
//     the message node is bound unlabeled and filtered with
//     `label(m) IN ['Comment','Post']` (Kuzu's per-row builtin).
//   - No `:City` / `:Country` sub-labels: the loader keeps `Place`
//     flat with a `type` column. `(:City)` → `(p:Place) WHERE
//     p.type='city'`, `(:Country)` → `type='country'`.
//   - `~[:knows]~{1,2}` → `-[:knows*1..2]-` (any-direction VLR).
//   - `DURATION({days: N})` desugars to `N*86400000` ms; creationDate
//     is stored as int epoch-millis, so the window bound is plain int
//     arithmetic.
//   - GROUP BY is implicit on the non-aggregated RETURN terms
//     (otherPerson.id/firstName/lastName), the Cypher idiom.
//   - otherPerson.id is INT64, so `ORDER BY otherPersonId` already
//     sorts numerically (= the toml's CAST(... AS INTEGER)).
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
MATCH (otherPerson)-[:isLocatedIn]->(city:Place)-[:isPartOf]->(country:Place)
WHERE city.type = 'city' AND country.type = 'country'
  AND country.name <> $countryXName AND country.name <> $countryYName
MATCH (otherPerson)<-[:hasCreator]-(messageX)-[:isLocatedIn]->(cx:Place)
WHERE label(messageX) IN ['Comment', 'Post']
  AND cx.type = 'country' AND cx.name = $countryXName
  AND messageX.creationDate >= $startDate
  AND messageX.creationDate < $startDate + $durationDays * 86400000
MATCH (otherPerson)<-[:hasCreator]-(messageY)-[:isLocatedIn]->(cy:Place)
WHERE label(messageY) IN ['Comment', 'Post']
  AND cy.type = 'country' AND cy.name = $countryYName
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
