// openCypher translation of bench/ldbc-queries/ic2.toml for Kuzu
// (kuzudb 0.11.3 from PyPI).
//
// Source-of-truth: bench/ldbc-queries/ic2.toml. Mirrors the toml's
// shape (anonymous start, COALESCE, ORDER BY, <=, canonical column
// names). NOT a verbatim copy of the LDBC reference Cypher; the
// toml is the bench's source-of-truth.
//
// Kuzu-specific divergences from the toml:
//   - Each Kuzu node has exactly one label (its NODE TABLE), so
//     `(message:Comment|Post)` label disjunction in patterns isn't
//     directly available. We leave `(message)` unlabeled — Kuzu's
//     multi-typed REL TABLE on `hasCreator` constrains the label
//     implicitly. Same logical query, different syntactic form.
//   - Edge labels lowercase per loader convention.
MATCH (:Person {id: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(message)
WHERE message.creationDate <= $maxDate
RETURN friend.id AS personId,
       friend.firstName AS personFirstName,
       friend.lastName AS personLastName,
       message.id AS postOrCommentId,
       COALESCE(message.content, message.imageFile) AS postOrCommentContent,
       message.creationDate AS postOrCommentCreationDate
ORDER BY postOrCommentCreationDate DESC, postOrCommentId ASC
LIMIT 20
