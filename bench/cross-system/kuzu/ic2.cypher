// openCypher translation of bench/ldbc-queries/ic2.toml for Kuzu
// (kuzudb 0.11.3 from PyPI).
//
// Source-of-truth: bench/ldbc-queries/ic2.toml. Mirrors the toml's
// shape (anonymous start, COALESCE, ORDER BY, <=, canonical column
// names). NOT a verbatim copy of the LDBC reference Cypher; the
// toml is the bench's source-of-truth.
//
// Kuzu-specific divergences from the toml:
//   - Kuzu's openCypher subset doesn't accept the pipe-disjunction
//     pattern `(message:Comment|Post)` (parser error) nor the
//     runtime predicate `WHERE message:Comment OR message:Post`
//     (also parser error — Kuzu has no `node:Label` predicate
//     because every node lives in exactly one NODE TABLE, so the
//     usual openCypher predicate isn't part of their grammar).
//     Instead, Kuzu provides a `label(node)` builtin that returns
//     the node's NODE TABLE name as a string. We use
//     `WHERE label(message) IN ["Comment", "Post"]` — this is a
//     real query-level label predicate, evaluated per row, not a
//     schema-level data-shape constraint.
//   - Edge labels lowercase per loader convention.
MATCH (:Person {id: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(message)
WHERE label(message) IN ["Comment", "Post"]
  AND message.creationDate <= $maxDate
RETURN friend.id AS personId,
       friend.firstName AS personFirstName,
       friend.lastName AS personLastName,
       message.id AS postOrCommentId,
       COALESCE(message.content, message.imageFile) AS postOrCommentContent,
       message.creationDate AS postOrCommentCreationDate
ORDER BY postOrCommentCreationDate DESC, postOrCommentId ASC
LIMIT 20
