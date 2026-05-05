// Cypher translation of bench/ldbc-queries/ic8.toml for graphqlite.
//
// Source-of-truth: bench/ldbc-queries/ic8.toml. Mirrors the toml's
// shape — same MATCH chain (`start` is the queried Person, `person`
// is the comment author), same column names matching the LDBC
// reference Cypher. Plain int ORDER BY (no toInteger needed since
// `.ldbcId` is int-typed).
//
// graphqlite-specific divergences from the toml:
//   - `.ldbcId` instead of `.id` (graphqlite reserves `.id`).
//   - `WHERE message:Comment OR message:Post` instead of label
//     disjunction in the pattern.
//   - Edge labels lowercase.
MATCH (start:Person {ldbcId: $personId})<-[:hasCreator]-(message)<-[:replyOf]-(comment:Comment)-[:hasCreator]->(person:Person)
WHERE (message:Comment OR message:Post)
RETURN person.ldbcId AS personId,
       person.firstName AS personFirstName,
       person.lastName AS personLastName,
       comment.creationDate AS commentCreationDate,
       comment.ldbcId AS commentId,
       comment.content AS commentContent
ORDER BY commentCreationDate DESC, commentId ASC
LIMIT 20
