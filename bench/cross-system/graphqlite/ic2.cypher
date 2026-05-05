// Cypher translation of bench/ldbc-queries/ic2.toml for graphqlite
// (colliery-io/graphqlite 0.4.4 from PyPI).
//
// Source-of-truth: bench/ldbc-queries/ic2.toml. The toml runs the
// closest-to-spec query gqlite's ISO GQL parser supports today
// (anonymous start, COALESCE, ORDER BY, <=). This translation
// mirrors the toml's shape — same columns, same logical query —
// for fair cross-system comparison. NOT a verbatim copy of the
// LDBC reference Cypher; the toml is the bench's source-of-truth.
//
// graphqlite-specific divergences from the toml:
//   - graphqlite reserves `.id` for the loader's prefixed
//     external_id; we read the int LDBC id from `.ldbcId`.
//     See DIVERGENCES.md.
//   - Cypher 4.x dialect rejects `(message:Comment|Post)` label
//     disjunction in patterns; we use `WHERE message:Comment OR
//     message:Post`. Same logical query.
//   - Edge labels lowercase per loader convention.
MATCH (:Person {ldbcId: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(message)
WHERE (message:Comment OR message:Post) AND message.creationDate <= $maxDate
RETURN friend.ldbcId AS personId,
       friend.firstName AS personFirstName,
       friend.lastName AS personLastName,
       message.ldbcId AS postOrCommentId,
       COALESCE(message.content, message.imageFile) AS postOrCommentContent,
       message.creationDate AS postOrCommentCreationDate
ORDER BY postOrCommentCreationDate DESC, postOrCommentId ASC
LIMIT 20
