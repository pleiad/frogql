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
// predicate). The `label()` builtin works but Kuzu's optimizer
// doesn't push it through multi-hop joins (~175× slower on IC8).
// We rely on the schema-level constraint from the multi-typed
// `hasCreator` REL TABLE: declared FROM Comment AND FROM Post in
// setup.py, so any node X reachable via `<-[:hasCreator]-` is
// implicitly Comment-or-Post. Same numbers as the explicit-predicate
// form on our data load, but the constraint lives in the schema
// (CREATE REL TABLE), not in the query body. Reviewers should know
// the cypher doesn't itself say "Comment or Post" — that's
// declared once at load time.
//   - Edge labels lowercase per loader convention.
MATCH (:Person {id: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(message)
WHERE message.creationDate <= $maxDate
RETURN friend.id AS personId,
       friend.firstName AS personFirstName,
       friend.lastName AS personLastName,
       message.id AS postOrCommentId,
       COALESCE(message.content, message.imageFile) AS postOrCommentContent,
       message.creationDate AS postOrCommentCreationDate
ORDER BY postOrCommentCreationDate DESC, postOrCommentId ASC
LIMIT 20
