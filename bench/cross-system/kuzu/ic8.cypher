// openCypher translation of bench/ldbc-queries/ic8.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic8.toml. Mirrors the toml's
// shape — `start` is the queried Person, `person` is the comment
// author, canonical column names.
//
// Kuzu-specific divergences from the toml — see ic2.cypher header
// (and DIVERGENCES.md) for the audit. Short version: query-level
// label predicate via Kuzu's `label()` builtin —
// `WHERE label(message) IN ["Comment", "Post"]` — which is the
// honest form (not a schema-level data-shape constraint).
// Kuzu 0.11.3's optimizer doesn't push `label()` through this
// 4-hop chain, so iter latency is ~14 s rather than ~80 ms. We
// keep the spec-faithful predicate and let the bench report
// Kuzu's actual plan-time behavior; we don't mask it with a
// schema-level workaround.
//   - Edge labels lowercase per loader convention.
MATCH (start:Person {id: $personId})<-[:hasCreator]-(message)<-[:replyOf]-(comment:Comment)-[:hasCreator]->(person:Person)
WHERE label(message) IN ["Comment", "Post"]
RETURN person.id AS personId,
       person.firstName AS personFirstName,
       person.lastName AS personLastName,
       comment.creationDate AS commentCreationDate,
       comment.id AS commentId,
       comment.content AS commentContent
ORDER BY commentCreationDate DESC, commentId ASC
LIMIT 20
