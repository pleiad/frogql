// openCypher translation of bench/ldbc-queries/ic2.toml for Kuzu
// (kuzudb 0.11.3 from PyPI).
//
// Source-of-truth: bench/ldbc-queries/ic2.toml. Mirrors the toml's
// shape (anonymous start, COALESCE, ORDER BY, <=, canonical column
// names). NOT a verbatim copy of the LDBC reference Cypher; the
// toml is the bench's source-of-truth.
//
// Kuzu-specific divergences from the toml — see DIVERGENCES.md for
// the full audit. Short version: Kuzu's openCypher dialect rejects
// `(message:Comment|Post)` (no pipe-disjunction in patterns) and
// `WHERE message:Comment OR message:Post` (no `node:Label` runtime
// predicate). The `label()` builtin works and is what we use:
// `WHERE label(message) IN ["Comment", "Post"]`. This is a real
// query-level predicate, evaluated per row, not a schema-level
// data-shape constraint. Equivalent semantically to gqlite's
// `(message:Comment|Post)` and graphqlite's `WHERE c:Comment OR
// c:Post`.
//
// NOTE: Kuzu 0.11.3's optimizer doesn't push `label()` through
// multi-hop joins. IC2 (1-hop friend) is moderate (~520 ms vs
// ~30 ms with schema-constrained form). IC8 (4-hop chain) is
// pathological (~14 s/iter). These are honest measurements of
// Kuzu's plan-time + execution behavior on the spec-faithful
// predicate; we don't mask them with a schema-level workaround.
//   - Edge labels lowercase per loader convention.
MATCH (:Person {id: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(message)
WHERE label(message) IN ["Comment", "Post"]
  AND message.creationDate <= $maxDate
RETURN friend.id AS personId,
       friend.firstName AS personFirstName,
       friend.lastName AS personLastName,
       message.id AS postOrCommentId,
       COALESCE(message.content, message.imageFile) AS postOrCommentContent,
       message.creationDate AS postOrCommentCreationDate
ORDER BY postOrCommentCreationDate DESC, postOrCommentId ASC
LIMIT 20
