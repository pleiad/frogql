// openCypher translation of bench/ldbc-queries/ic7.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic7.toml. Mirrors the toml's
// shape — drops the `WITH ... head(collect(...))` per-liker top-1
// dedup AND the `minutesLatency` computed column for
// cross-system fairness. Kuzu fully supports both, but running them
// here would have Kuzu doing more work than gqlite, biasing the
// latency comparison. The bench compares engines on the SAME query;
// the toml is the source-of-truth simplified spec.
//
// Kuzu-specific divergences from the toml:
//   - `(message)` unlabeled instead of `(message:Comment|Post)`;
//     Kuzu's multi-typed REL TABLE on hasCreator constrains the
//     label implicitly. Same as ic2.cypher.
//   - Edge labels lowercase per loader convention.
//   - Kuzu uses `-[:knows]-` (dash-only) for undirected edges, not
//     `~[:knows]~` (tilde). Same semantics, different syntax.
MATCH (person:Person {id: $personId})<-[:hasCreator]-(message)<-[like:likes]-(liker:Person)
OPTIONAL MATCH (liker)-[:knows]-(person)
RETURN liker.id AS personId,
       liker.firstName AS personFirstName,
       liker.lastName AS personLastName,
       like.creationDate AS likeCreationDate,
       message.id AS commentOrPostId,
       COALESCE(message.content, message.imageFile) AS commentOrPostContent,
       NOT EXISTS { MATCH (liker)-[:knows]-(person) } AS isNew
ORDER BY likeCreationDate DESC, personId ASC
LIMIT 20
