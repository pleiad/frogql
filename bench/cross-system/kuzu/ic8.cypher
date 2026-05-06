// openCypher translation of bench/ldbc-queries/ic8.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic8.toml. Mirrors the toml's
// shape — `start` is the queried Person, `person` is the comment
// author, canonical column names.
//
// Kuzu-specific divergences from the toml:
//   - Kuzu has no label-disjunction mechanism (see ic2.cypher
//     header comment for the full enumeration of forms tested).
//     We use `(message)` bound but unlabeled; the multi-typed REL
//     TABLEs on hasCreator AND replyOf both constrain `message` to
//     Comment-or-Post at the schema level. Tested both `()`
//     (anonymous) and `(message)` (bound) — Kuzu's optimizer picks
//     a substantially worse plan for the anonymous form (~15 s/iter
//     vs ~80 ms for the bound form), reasons not investigated. The
//     bound form is also what we use in ic2.cypher; consistent
//     across IC2 and IC8.
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
