// openCypher translation of bench/ldbc-queries/ic8.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic8.toml. Mirrors the toml's
// shape — `start` is the queried Person, `person` is the comment
// author, canonical column names. Multi-typed REL TABLE on
// hasCreator and replyOf lets Kuzu resolve the implicit
// Comment-or-Post label of the message node.
//
// Kuzu-specific divergences from the toml:
//   - `(message)` (bound, unlabeled) instead of `(:Comment|Post)`;
//     Kuzu's multi-typed REL TABLE handles label resolution. We
//     explicitly bind the variable rather than using the truly-
//     anonymous `()` form — measured Kuzu's optimizer to pick a
//     much worse plan with `()` (~15 s/iter vs ~80 ms with bound),
//     for reasons not investigated. Bound form matches what we
//     also do in ic2.cypher.
//   - Edge labels lowercase per loader convention.
MATCH (start:Person {id: $personId})<-[:hasCreator]-(message)<-[:replyOf]-(comment:Comment)-[:hasCreator]->(person:Person)
RETURN person.id AS personId,
       person.firstName AS personFirstName,
       person.lastName AS personLastName,
       comment.creationDate AS commentCreationDate,
       comment.id AS commentId,
       comment.content AS commentContent
ORDER BY commentCreationDate DESC, commentId ASC
LIMIT 20
