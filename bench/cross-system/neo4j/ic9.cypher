// Cypher translation of bench/ldbc-queries/ic9.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic9.toml.
//   - `~[:knows]~{1,2}` → `-[:knows*1..2]-` (any-direction VLR over
//     single-direction storage).
//   - `(message:Comment|Post)` — native label-expression disjunction.
//   - Flat-chain (no WITH DISTINCT) per the toml's no-with-distinct
//     divergence — same variant as every other system in the bench.
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)<-[:hasCreator]-(message:Comment|Post)
WHERE person <> otherPerson
  AND message.creationDate < $maxDate
RETURN otherPerson.id AS personId,
       otherPerson.firstName AS personFirstName,
       otherPerson.lastName AS personLastName,
       message.id AS commentOrPostId,
       coalesce(message.content, message.imageFile) AS commentOrPostContent,
       message.creationDate AS commentOrPostCreationDate
ORDER BY commentOrPostCreationDate DESC, commentOrPostId ASC
LIMIT 20
