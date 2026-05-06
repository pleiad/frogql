// Cypher translation of bench/ldbc-queries/ic6.toml for graphqlite.
//
// Source-of-truth: bench/ldbc-queries/ic6.toml. Implicit Cypher
// grouping by RETURN's non-aggregate columns (otherTag.name) makes
// explicit GROUP BY unnecessary in Cypher.
//
// graphqlite-specific divergences:
//   - `.ldbcId` instead of `.id`.
//   - `~[:knows]~{1, 2}` → `-[:knows*1..2]-`.
//   - Edge labels lowercase per loader convention.
MATCH (person:Person {ldbcId: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
MATCH (otherPerson)<-[:hasCreator]-(post:Post)-[:hasTag]->(tag:Tag {name: $tagName})
MATCH (post)-[:hasTag]->(otherTag:Tag)
WHERE otherTag <> tag
RETURN otherTag.name AS tagName,
       COUNT(post) AS postCount
ORDER BY postCount DESC, tagName ASC
LIMIT 10
