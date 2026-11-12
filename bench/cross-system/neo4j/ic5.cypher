// Cypher translation of bench/ldbc-queries/ic5.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic5.toml. Uses Cypher's
// canonical `WITH ... ORDER BY ... LIMIT ... RETURN` idiom; gqlite's
// toml uses explicit GROUP BY instead. Both produce the same 20 rows
// in the same order.
//
// Friends-of-friends set: `WITH DISTINCT otherPerson` right after the
// knows hop, as in the LDBC reference; the toml gets the same set from
// `ANY SHORTEST` (its `friends-set` divergence).
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
WITH DISTINCT otherPerson
MATCH (otherPerson)<-[hm:hasMember]-(forum:Forum)
WHERE hm.joinDate > $minDate
OPTIONAL MATCH (otherPerson)<-[:hasCreator]-(post:Post)<-[:containerOf]-(forum)
WITH forum, count(post) AS postCount
ORDER BY postCount DESC, forum.id ASC
LIMIT 20
RETURN forum.title AS forumName, forum.id AS forumId, postCount
