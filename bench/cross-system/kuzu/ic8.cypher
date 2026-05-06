// openCypher translation of bench/ldbc-queries/ic8.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic8.toml. Mirrors the toml's
// shape — `start` is the queried Person, `person` is the comment
// author, canonical column names.
//
// Kuzu-specific divergences from the toml:
//   - Kuzu has no `(node:Comment|Post)` pattern disjunction nor
//     `WHERE node:Label` predicate — see ic2.cypher header for the
//     fuller enumeration. Instead we use `label(node)` builtin in
//     WHERE, which IS a real query-level label predicate (not
//     schema-level data-shape constraint). Same idiom as ic2.cypher.
//   - Edge labels lowercase per loader convention.
MATCH (start:Person {id: $personId})<-[:hasCreator]-(message)<-[:replyOf]-(comment:Comment)-[:hasCreator]->(person:Person)
WHERE label(message) IN ["Comment", "Post"]
RETURN person.id AS personId,
       person.firstName AS personFirstName,
       person.lastName AS personLastName,
       comment.creationDate AS commentCreationDate,
       comment.id AS commentId,
       comment.content AS commentContent
ORDER BY commentCreationDate DESC, commentId ASC
LIMIT 20
