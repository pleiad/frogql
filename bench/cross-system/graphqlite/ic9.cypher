// Cypher translation of bench/ldbc-queries/ic9.toml for graphqlite
// (colliery-io/graphqlite 0.4.4 from PyPI).
//
// VERDICT: VERIFIED — 15/15 param-row hashes match the gqlite reference
// (2026-06-12, full SF0.1, 1 iter). Previously RUNS-WRONG (0/15) because
// graphqlite's undirected variable-length `-[:knows*1..2]-` is silently
// forward-only (see DIVERGENCES.md). Fixed here with the var-length
// workaround documented below.
//
// Source-of-truth: bench/ldbc-queries/ic9.toml. The toml's
// `~[:knows]~{1,2}(otherPerson)` is a WALK multiset: each otherPerson is
// bound once per distinct knows-path of length 1 or 2. A FoF reachable via
// several paths produces several bindings (and thus several result rows).
//
// graphqlite-specific divergences from the toml:
//   - graphqlite reserves `.id` for the loader's prefixed external_id;
//     we read the int LDBC id from `.ldbcId` (see DIVERGENCES.md).
//   - Cypher 4.x dialect rejects `(message:Comment|Post)` label
//     disjunction in patterns; we use `WHERE message:Comment OR
//     message:Post`.
//   - Edge labels lowercase per loader convention.
//
// VAR-LENGTH DIRECTION-BUG WORKAROUND (the fix that lifts this to 15/15):
//   graphqlite's `-[:knows*1..2]-` (undirected, no arrowheads) silently
//   traverses outgoing edges only — wrong on LDBC, where each knows pair is
//   stored once in one direction, so the bidirectional friend set is lost.
//   The single-hop undirected `-[:knows]-` IS correct, so we expand the
//   `{1,2}` into a UNION ALL of:
//     branch A — 1-hop:  (person)-[:knows]-(other)
//     branch B — 2-hop:  (person)-[:knows]-(mid)-[:knows]-(other)
//   each built from correct bidirectional single hops. The combined
//   multiset reproduces gqlite's `~{1,2}` exactly: verified on row 0,
//   1-hop=10 + 2-hop(person<>other)=648 = 658 bindings, byte-identical to
//   gqlite's `COUNT(otherPerson)=658`. UNION ALL (not UNION) preserves the
//   multiset; the `person <> other` filter on each branch drops the trivial
//   length-2 return-to-self walks, matching the toml's `person <> otherPerson`.
//
//   toInteger() CASTS: graphqlite's UNION coerces INTEGER columns to strings
//   (engine bug — a plain `RETURN n.ldbcId UNION ALL ...` yields '12345'
//   strings, which then sort lexicographically and hash-diverge). Wrapping
//   each int column in toInteger() restores the int type and the numeric
//   ORDER BY. String columns (firstName/lastName/content) pass through.
MATCH (person:Person {ldbcId: $personId})-[:knows]-(other:Person)<-[:hasCreator]-(message)
WHERE person <> other AND (message:Comment OR message:Post) AND message.creationDate < $maxDate
RETURN toInteger(other.ldbcId) AS personId,
       other.firstName AS personFirstName,
       other.lastName AS personLastName,
       toInteger(message.ldbcId) AS commentOrPostId,
       COALESCE(message.content, message.imageFile) AS commentOrPostContent,
       toInteger(message.creationDate) AS commentOrPostCreationDate
UNION ALL
MATCH (person:Person {ldbcId: $personId})-[:knows]-(mid:Person)-[:knows]-(other:Person)<-[:hasCreator]-(message)
WHERE person <> other AND (message:Comment OR message:Post) AND message.creationDate < $maxDate
RETURN toInteger(other.ldbcId) AS personId,
       other.firstName AS personFirstName,
       other.lastName AS personLastName,
       toInteger(message.ldbcId) AS commentOrPostId,
       COALESCE(message.content, message.imageFile) AS commentOrPostContent,
       toInteger(message.creationDate) AS commentOrPostCreationDate
ORDER BY commentOrPostCreationDate DESC, commentOrPostId ASC
LIMIT 20
