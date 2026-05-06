// openCypher translation of bench/ldbc-queries/ic2.toml for Kuzu
// (kuzudb 0.11.3 from PyPI).
//
// Source-of-truth: bench/ldbc-queries/ic2.toml. Mirrors the toml's
// shape (anonymous start, COALESCE, ORDER BY, <=, canonical column
// names). NOT a verbatim copy of the LDBC reference Cypher; the
// toml is the bench's source-of-truth.
//
// Kuzu-specific divergences from the toml:
//   - Kuzu has no label-disjunction mechanism at all. Tested 2026-05:
//     `(message:Comment|Post)` pattern → parser error; `WHERE
//     message:Comment OR message:Post` predicate → parser error;
//     `UNION ALL ... LIMIT N` → silently broken (LIMIT applied per
//     branch only, not globally); `CALL { ... UNION ALL ... }` →
//     no CALL{} grammar; `... UNION ALL ... WITH ... LIMIT` →
//     parser error after UNION. So we use `(message)` bound but
//     unlabeled — Kuzu's multi-typed REL TABLE on `hasCreator`
//     (FROM Comment, FROM Post) constrains `message` to Comment+Post
//     at the SCHEMA level. Same semantic constraint, applied via
//     edge schema rather than node label predicate.
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
