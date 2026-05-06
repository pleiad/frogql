// openCypher translation of bench/ldbc-queries/ic9.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic9.toml. Mirrors the toml's
// shape — `~[:knows]~{1,2}` becomes Kuzu's `[:knows*1..2]`.
//
// Kuzu-specific divergences — see ic2.cypher header (and
// DIVERGENCES.md) for the full audit. Short version:
//   - `(message:Comment|Post)` pattern disjunction not supported;
//     we use `WHERE label(message) IN ["Comment", "Post"]` —
//     query-level predicate via Kuzu's `label()` builtin.
//   - `~[:knows]~{1,2}` → `-[:knows*1..2]-`. The setup loader
//     materializes both directions of `:knows`, so the
//     undirected-flavored gqlite form and the any-direction Kuzu
//     form match the same friend set.
//   - Edge labels lowercase per loader convention.
//
// IC9's variable-length 1-or-2 hop multiplies the number of
// `(otherPerson)<-[:hasCreator]-(message)` lookups; combined with
// Kuzu's optimizer not pushing `label()` through joins, expect
// per-row latency >> IC2.
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)<-[:hasCreator]-(message)
WHERE label(message) IN ["Comment", "Post"]
  AND person <> otherPerson
  AND message.creationDate < $maxDate
RETURN otherPerson.id AS personId,
       otherPerson.firstName AS personFirstName,
       otherPerson.lastName AS personLastName,
       message.id AS commentOrPostId,
       COALESCE(message.content, message.imageFile) AS commentOrPostContent,
       message.creationDate AS commentOrPostCreationDate
ORDER BY commentOrPostCreationDate DESC, commentOrPostId ASC
LIMIT 20
