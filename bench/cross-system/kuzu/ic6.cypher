// openCypher translation of bench/ldbc-queries/ic6.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic6.toml. Implicit Cypher
// grouping by RETURN's non-aggregate columns (otherTag.name).
//
// Edge labels lowercase per loader convention.
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
MATCH (otherPerson)<-[:hasCreator]-(post:Post)-[:hasTag]->(:Tag {name: $tagName})
MATCH (post)-[:hasTag]->(otherTag:Tag)
WHERE otherTag.name <> $tagName
RETURN otherTag.name AS otherTagName,
       COUNT(post) AS postCount
ORDER BY postCount DESC, otherTagName ASC
LIMIT 10
