// openCypher translation of bench/ldbc-queries/ic2.toml for Kuzu
// (kuzudb 0.11.3 from PyPI).
//
// Source-of-truth: bench/ldbc-queries/ic2.toml. The toml runs the
// spec-faithful query (COALESCE, ORDER BY) since gqlite supports
// both as of 2026-05.
//
// Kuzu-specific divergences:
//   - Each Kuzu node has exactly one label (its NODE TABLE), so
//     `(message:Comment|Post)` label disjunction in patterns isn't
//     directly available. We leave `(message)` unlabeled — Kuzu
//     resolves it automatically because `hasCreator` is declared as
//     a multi-typed REL TABLE (`FROM Comment TO Person, FROM Post
//     TO Person`), so the engine constrains `message` to Comment-or-
//     Post implicitly via the relationship type. This matches the
//     canonical IC2 shape (one MATCH + implicit label disjunction)
//     and avoids the `UNION ALL` rewrite which has awkward LIMIT
//     semantics. See DIVERGENCES.md for the LIMIT-over-UNION-ALL story.
//   - Edge labels lowercase per loader convention.
MATCH (p:Person {id: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(message)
WHERE message.creationDate < $maxDate
RETURN friend.id AS friend_id,
       friend.firstName AS friend_firstName,
       friend.lastName AS friend_lastName,
       message.id AS message_id,
       COALESCE(message.content, message.imageFile) AS message_content,
       message.creationDate AS message_creationDate
ORDER BY message_creationDate DESC, message_id ASC
LIMIT 20
