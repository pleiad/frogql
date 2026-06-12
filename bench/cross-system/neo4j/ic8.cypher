// Cypher translation of bench/ldbc-queries/ic8.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic8.toml. `start` is the
// queried Person, `person` is the comment author.
//
// `(:Comment|Post)` — native Neo4j 5 label-expression disjunction
// (cf. Kuzu's label() workaround). Edge labels lowercase per loader
// convention.
MATCH (start:Person {id: $personId})<-[:hasCreator]-(message:Comment|Post)<-[:replyOf]-(comment:Comment)-[:hasCreator]->(person:Person)
RETURN person.id AS personId,
       person.firstName AS personFirstName,
       person.lastName AS personLastName,
       comment.creationDate AS commentCreationDate,
       comment.id AS commentId,
       comment.content AS commentContent
ORDER BY commentCreationDate DESC, commentId ASC
LIMIT 20
