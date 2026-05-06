// openCypher translation of bench/ldbc-queries/ic5.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic5.toml. Uses Cypher's
// canonical `WITH ... ORDER BY ... LIMIT ... RETURN` idiom, the
// spec form. gqlite's toml uses explicit GROUP BY instead (no WITH
// in our parser); both produce the same 20 rows in the same order.
//
// Edge labels lowercase per loader convention.
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
MATCH (otherPerson)<-[hm:hasMember]-(forum:Forum)
WHERE hm.joinDate > $minDate
OPTIONAL MATCH (otherPerson)<-[:hasCreator]-(post:Post)<-[:containerOf]-(forum)
WITH forum, COUNT(post) AS postCount
ORDER BY postCount DESC, forum.id ASC
LIMIT 20
RETURN forum.title AS forumName, forum.id AS forumId, postCount
