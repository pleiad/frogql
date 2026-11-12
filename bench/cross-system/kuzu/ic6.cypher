// openCypher translation of bench/ldbc-queries/ic6.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic6.toml. Implicit Cypher
// grouping by RETURN's non-aggregate columns (otherTag.name).
//
// Edge labels lowercase per loader convention.
//
// Friends-of-friends set: `WITH DISTINCT otherPerson` right after the
// knows hop, as in the LDBC reference; the toml gets the same set from
// `ANY SHORTEST` (its `friends-set` divergence).
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
WITH DISTINCT otherPerson
MATCH (otherPerson)<-[:hasCreator]-(post:Post)-[:hasTag]->(tag:Tag {name: $tagName})
MATCH (post)-[:hasTag]->(otherTag:Tag)
WHERE otherTag <> tag
RETURN otherTag.name AS tagName,
       COUNT(post) AS postCount
ORDER BY postCount DESC, tagName ASC
LIMIT 10
