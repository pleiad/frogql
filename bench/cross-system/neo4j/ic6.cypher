// Cypher translation of bench/ldbc-queries/ic6.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic6.toml. Implicit Cypher
// grouping by RETURN's non-aggregate column (otherTag.name).
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
       count(post) AS postCount
ORDER BY postCount DESC, tagName ASC
LIMIT 10
