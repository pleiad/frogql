// Cypher translation of bench/ldbc-queries/ic6.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic6.toml. Implicit Cypher
// grouping by RETURN's non-aggregate column (otherTag.name).
// Flat-chain (no WITH DISTINCT) per the toml's no-with-distinct
// divergence — same variant as every other system in the bench.
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
MATCH (otherPerson)<-[:hasCreator]-(post:Post)-[:hasTag]->(tag:Tag {name: $tagName})
MATCH (post)-[:hasTag]->(otherTag:Tag)
WHERE otherTag <> tag
RETURN otherTag.name AS tagName,
       count(post) AS postCount
ORDER BY postCount DESC, tagName ASC
LIMIT 10
