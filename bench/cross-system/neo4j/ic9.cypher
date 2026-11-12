// Cypher translation of bench/ldbc-queries/ic9.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic9.toml.
//   - `~[:knows]~{1,2}` → `-[:knows*1..2]-` (any-direction VLR over
//     single-direction storage).
//   - `(message:Comment|Post)` — native label-expression disjunction.
//   - Friends-of-friends set: `WITH DISTINCT otherPerson` right after
//     the knows hop, as in the LDBC reference; the toml gets the same
//     set from `ANY SHORTEST` (its `friends-set` divergence).
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
WITH DISTINCT otherPerson
MATCH (otherPerson)<-[:hasCreator]-(message:Comment|Post)
WHERE message.creationDate < $maxDate
RETURN otherPerson.id AS personId,
       otherPerson.firstName AS personFirstName,
       otherPerson.lastName AS personLastName,
       message.id AS commentOrPostId,
       coalesce(message.content, message.imageFile) AS commentOrPostContent,
       message.creationDate AS commentOrPostCreationDate
ORDER BY commentOrPostCreationDate DESC, commentOrPostId ASC
LIMIT 20
