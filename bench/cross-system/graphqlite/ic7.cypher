// Cypher translation of bench/ldbc-queries/ic7.toml for graphqlite.
//
// Source-of-truth: bench/ldbc-queries/ic7.toml. The toml drops the
// spec's `minutesLatency` column because gqlite's parser doesn't
// have `/`, `FLOOR`, or `CAST AS FLOAT`. To keep the cross-system
// comparison apples-to-apples, every system runs the same 7-column
// shape (no minutesLatency).
//
// graphqlite-specific divergences (same pattern as ic2.cypher):
//   - `.ldbcId` instead of `.id` (graphqlite reserves `.id` for the
//     loader's prefixed external_id).
//   - `WHERE message:Comment OR message:Post` instead of label
//     disjunction in the pattern.
//   - Edge labels lowercase per loader convention.
MATCH (person:Person {ldbcId: $personId})<-[:hasCreator]-(message)<-[like:likes]-(liker:Person)
WHERE (message:Comment OR message:Post)
OPTIONAL MATCH (liker)-[:knows]-(person)
RETURN liker.ldbcId AS person_id,
       liker.firstName AS person_firstName,
       liker.lastName AS person_lastName,
       like.creationDate AS like_creationDate,
       message.ldbcId AS commentOrPost_id,
       COALESCE(message.content, message.imageFile) AS commentOrPost_content,
       NOT EXISTS { MATCH (liker)-[:knows]-(person) } AS isNew
ORDER BY like_creationDate DESC, person_id ASC
LIMIT 20
