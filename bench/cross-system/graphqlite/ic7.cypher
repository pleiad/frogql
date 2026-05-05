// Cypher translation of bench/ldbc-queries/ic7.toml for graphqlite.
//
// Source-of-truth: bench/ldbc-queries/ic7.toml. The toml drops two
// spec features that gqlite's ISO GQL parser can't express today:
//   - the `WITH ... ORDER BY ... head(collect(...))` per-liker top-1
//     dedup (no WITH stage, no collect, no head)
//   - the `minutesLatency` computed column (no /, no toFloat/floor/
//     toInteger)
// For fair cross-system comparison every engine runs the toml's
// shape — graphqlite + Kuzu support the canonical Cypher fully but
// we run the simplified version here so latency numbers reflect
// the same query work, not different work. The toml is the bench's
// source-of-truth; the LDBC reference Cypher is documented in the
// toml's [divergences] for context.
//
// graphqlite-specific divergences from the toml:
//   - `.ldbcId` instead of `.id` (graphqlite reserves `.id`).
//   - `WHERE message:Comment OR message:Post` instead of label
//     disjunction in the pattern.
//   - Edge labels lowercase per loader convention.
MATCH (person:Person {ldbcId: $personId})<-[:hasCreator]-(message)<-[like:likes]-(liker:Person)
WHERE (message:Comment OR message:Post)
OPTIONAL MATCH (liker)-[:knows]-(person)
RETURN liker.ldbcId AS personId,
       liker.firstName AS personFirstName,
       liker.lastName AS personLastName,
       like.creationDate AS likeCreationDate,
       message.ldbcId AS commentOrPostId,
       COALESCE(message.content, message.imageFile) AS commentOrPostContent,
       NOT EXISTS { MATCH (liker)-[:knows]-(person) } AS isNew
ORDER BY likeCreationDate DESC, personId ASC
LIMIT 20
