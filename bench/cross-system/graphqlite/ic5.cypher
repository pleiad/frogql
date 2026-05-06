// Cypher translation of bench/ldbc-queries/ic5.toml for graphqlite.
//
// Source-of-truth: bench/ldbc-queries/ic5.toml. Mirrors the toml's
// shape, but uses Cypher's canonical `WITH ... ORDER BY ... LIMIT
// ... RETURN` arg-max idiom (the spec form). gqlite's parser
// doesn't have WITH, so its toml uses explicit GROUP BY instead;
// both produce the same 20 rows in the same order.
//
// graphqlite-specific divergences:
//   - `.ldbcId` instead of `.id` (graphqlite reserves `.id`).
//   - `~[:knows]~{1, 2}` → `-[:knows*1..2]-`.
//   - Edge labels lowercase per loader convention.
MATCH (person:Person {ldbcId: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
MATCH (otherPerson)<-[hm:hasMember]-(forum:Forum)
WHERE hm.joinDate > $minDate
OPTIONAL MATCH (otherPerson)<-[:hasCreator]-(post:Post)<-[:containerOf]-(forum)
WITH forum, COUNT(post) AS postCount
ORDER BY postCount DESC, forum.ldbcId ASC
LIMIT 20
RETURN forum.title AS forumName, forum.ldbcId AS forumId, postCount
