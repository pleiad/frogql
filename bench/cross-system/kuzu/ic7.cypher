// openCypher translation of bench/ldbc-queries/ic7.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic7.toml. Same minutesLatency
// drop as the gqlite version — applied here too for cross-system
// fairness (every system runs the same 7-column shape).
//
// Kuzu-specific divergences:
//   - `(message)` unlabeled instead of `(message:Comment|Post)`;
//     Kuzu's multi-typed REL TABLE on hasCreator constrains the
//     label implicitly. Same as ic2.cypher.
//   - Edge labels lowercase per loader convention.
//
// Note: Kuzu's `~[:knows]~` undirected pattern in the OPTIONAL
// MATCH and EXISTS clauses uses `-[:knows]-` here because Kuzu
// expresses undirected REL TABLES with the dash-only form (not the
// tilde). Same semantics, different syntax.
MATCH (person:Person {id: $personId})<-[:hasCreator]-(message)<-[like:likes]-(liker:Person)
OPTIONAL MATCH (liker)-[:knows]-(person)
RETURN liker.id AS person_id,
       liker.firstName AS person_firstName,
       liker.lastName AS person_lastName,
       like.creationDate AS like_creationDate,
       message.id AS commentOrPost_id,
       COALESCE(message.content, message.imageFile) AS commentOrPost_content,
       NOT EXISTS { MATCH (liker)-[:knows]-(person) } AS isNew
ORDER BY like_creationDate DESC, person_id ASC
LIMIT 20
