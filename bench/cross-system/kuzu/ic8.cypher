// openCypher translation of bench/ldbc-queries/ic8.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic8.toml. Direct translation
// — multi-typed REL TABLE on hasCreator and replyOf lets Kuzu
// resolve the implicit Comment-or-Post label of (message) without
// us writing the disjunction explicitly.
//
// Kuzu-specific divergences:
//   - `(message)` unlabeled.
//   - Edge labels lowercase.
MATCH (person:Person {id: $personId})<-[:hasCreator]-(message)<-[:replyOf]-(comment:Comment)-[:hasCreator]->(commentAuthor:Person)
RETURN commentAuthor.id AS commentAuthor_id,
       commentAuthor.firstName AS commentAuthor_firstName,
       commentAuthor.lastName AS commentAuthor_lastName,
       comment.creationDate AS comment_creationDate,
       comment.id AS comment_id,
       comment.content AS comment_content
ORDER BY comment_creationDate DESC, comment_id ASC
LIMIT 20
