// Cypher translation of bench/ldbc-queries/ic5.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic5.toml. Uses Cypher's
// canonical `WITH ... ORDER BY ... LIMIT ... RETURN` idiom; gqlite's
// toml uses explicit GROUP BY instead. Both produce the same 20 rows
// in the same order.
//
// Mirrors the toml's flat-chain shape (no `WITH DISTINCT friend`
// dedup) — see the toml's `no-with-distinct` divergence; all systems
// in this bench run the same variant so the row hashes are
// comparable.
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
MATCH (otherPerson)<-[hm:hasMember]-(forum:Forum)
WHERE hm.joinDate > $minDate
OPTIONAL MATCH (otherPerson)<-[:hasCreator]-(post:Post)<-[:containerOf]-(forum)
WITH forum, count(post) AS postCount
ORDER BY postCount DESC, forum.id ASC
LIMIT 20
RETURN forum.title AS forumName, forum.id AS forumId, postCount
