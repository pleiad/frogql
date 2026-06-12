// Cypher translation of bench/ldbc-queries/ic2.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic2.toml. Mirrors the toml's
// shape (anonymous start, COALESCE, ORDER BY, <=, canonical column
// names).
//
// Neo4j-specific notes (see DIVERGENCES.md):
//   - `(message:Comment|Post)` — Neo4j 5's label-expression
//     disjunction maps the ISO GQL alternation 1:1, evaluated at
//     query level. No `label()`-builtin workaround needed (cf. Kuzu).
//   - Edge labels lowercase per loader convention.
MATCH (:Person {id: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(message:Comment|Post)
WHERE message.creationDate <= $maxDate
RETURN friend.id AS personId,
       friend.firstName AS personFirstName,
       friend.lastName AS personLastName,
       message.id AS postOrCommentId,
       coalesce(message.content, message.imageFile) AS postOrCommentContent,
       message.creationDate AS postOrCommentCreationDate
ORDER BY postOrCommentCreationDate DESC, postOrCommentId ASC
LIMIT 20
