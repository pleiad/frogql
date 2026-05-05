// Cypher translation of bench/ldbc-queries/ic2.toml for graphqlite
// (colliery-io/graphqlite 0.4.4 from PyPI).
//
// Source-of-truth: bench/ldbc-queries/ic2.toml. The toml runs the
// spec-faithful query (COALESCE, ORDER BY) since gqlite supports
// both as of 2026-05. This file is the direct translation for
// graphqlite's Cypher dialect.
//
// graphqlite-specific divergences:
//   - graphqlite reserves `.id` for the loader's external_id
//     (prefixed `"Person:933"` etc.), so we expose the int LDBC id
//     under `.ldbcId` and reference it everywhere `gqlite` uses `.id`.
//     See DIVERGENCES.md.
//   - graphqlite's Cypher dialect rejects `(c:Comment|Post)` label
//     disjunction in patterns (Cypher 5+ feature). We use the
//     equivalent `WHERE c:Comment OR c:Post` form. Same logical query.
//   - Edge labels lowercase (`:knows`, `:hasCreator`) per the loader
//     convention. Spec writeups vary on casing.
MATCH (p:Person {ldbcId: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(message)
WHERE (message:Comment OR message:Post) AND message.creationDate < $maxDate
RETURN friend.ldbcId AS friend_id,
       friend.firstName AS friend_firstName,
       friend.lastName AS friend_lastName,
       message.ldbcId AS message_id,
       COALESCE(message.content, message.imageFile) AS message_content,
       message.creationDate AS message_creationDate
ORDER BY message_creationDate DESC, message_id ASC
LIMIT 20
