// Cypher translation of bench/ldbc-queries/ic9.toml for graphqlite
// (colliery-io/graphqlite 0.4.4 from PyPI).
//
// Source-of-truth: bench/ldbc-queries/ic9.toml. Mirrors the toml's
// shape — `~[:knows]~{1,2}` becomes Cypher's `[:knows*1..2]`,
// label disjunction goes through the Cypher 4.x predicate form.
// NOT a verbatim copy of the LDBC reference Cypher; the toml is
// the bench's source-of-truth.
//
// graphqlite-specific divergences from the toml:
//   - graphqlite reserves `.id` for the loader's prefixed
//     external_id; we read the int LDBC id from `.ldbcId`
//     (see DIVERGENCES.md).
//   - Cypher 4.x dialect rejects `(message:Comment|Post)` label
//     disjunction in patterns; we use `WHERE message:Comment OR
//     message:Post`.
//   - `~[:knows]~{1,2}` → `-[:knows*1..2]-` (Cypher's
//     undirected variable-length form). The setup loader
//     materializes both directions of `:knows` so this matches
//     the same friend set.
//   - Edge labels lowercase per loader convention.
//
// Bare-node comparison `WHERE person <> otherPerson` works in
// graphqlite's Cypher dialect (compares node identity), so the
// spec form is shipped verbatim.
MATCH (person:Person {ldbcId: $personId})-[:knows*1..2]-(otherPerson:Person)<-[:hasCreator]-(message)
WHERE (message:Comment OR message:Post)
  AND person <> otherPerson
  AND message.creationDate < $maxDate
RETURN otherPerson.ldbcId AS personId,
       otherPerson.firstName AS personFirstName,
       otherPerson.lastName AS personLastName,
       message.ldbcId AS commentOrPostId,
       COALESCE(message.content, message.imageFile) AS commentOrPostContent,
       message.creationDate AS commentOrPostCreationDate
ORDER BY commentOrPostCreationDate DESC, commentOrPostId ASC
LIMIT 20
