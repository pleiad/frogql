// openCypher translation of bench/ldbc-queries/ic8.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic8.toml. Mirrors the toml's
// shape — `start` is the queried Person, `person` is the comment
// author, canonical column names.
//
// Kuzu-specific divergences from the toml — see ic2.cypher header
// (and DIVERGENCES.md) for the audit. Short version: schema-level
// constraint via the multi-typed `hasCreator` REL TABLE rather than
// query-level `label()` predicate, because Kuzu's optimizer doesn't
// push label() through multi-hop joins (~175× slower on this query).
// Same data, different audit trail.
//   - Edge labels lowercase per loader convention.
MATCH (start:Person {id: $personId})<-[:hasCreator]-(message)<-[:replyOf]-(comment:Comment)-[:hasCreator]->(person:Person)
RETURN person.id AS personId,
       person.firstName AS personFirstName,
       person.lastName AS personLastName,
       comment.creationDate AS commentCreationDate,
       comment.id AS commentId,
       comment.content AS commentContent
ORDER BY commentCreationDate DESC, commentId ASC
LIMIT 20
