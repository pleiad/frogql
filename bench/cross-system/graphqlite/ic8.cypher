// Cypher translation of bench/ldbc-queries/ic8.toml for graphqlite.
//
// Source-of-truth: bench/ldbc-queries/ic8.toml. The query is a
// direct translation of the spec — `(message:Message)` → `WHERE
// message:Comment OR message:Post`, plain int-column ORDER BY (no
// CAST needed since `.ldbcId` is already int-typed).
//
// graphqlite-specific divergences:
//   - `.ldbcId` instead of `.id` (graphqlite reserves `.id`).
//   - WHERE for label disjunction.
//   - Edge labels lowercase.
MATCH (person:Person {ldbcId: $personId})<-[:hasCreator]-(message)<-[:replyOf]-(comment:Comment)-[:hasCreator]->(commentAuthor:Person)
WHERE (message:Comment OR message:Post)
RETURN commentAuthor.ldbcId AS commentAuthor_id,
       commentAuthor.firstName AS commentAuthor_firstName,
       commentAuthor.lastName AS commentAuthor_lastName,
       comment.creationDate AS comment_creationDate,
       comment.ldbcId AS comment_id,
       comment.content AS comment_content
ORDER BY comment_creationDate DESC, comment_id ASC
LIMIT 20
