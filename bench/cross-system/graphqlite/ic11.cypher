// Cypher translation of bench/ldbc-queries/ic11.toml for graphqlite.
//
// Source-of-truth: bench/ldbc-queries/ic11.toml. Spec form.
//
// graphqlite-specific divergences:
//   - `.ldbcId` instead of `.id`.
//   - `~[:knows]~{1, 2}` → `-[:knows*1..2]-`.
//   - Edge labels lowercase per loader convention.
//   - `:Company` and `:Country` sub-labels are present in the
//     graphqlite loader (same as gqlite/Kuzu).
MATCH (person:Person {ldbcId: $personId})-[:knows*1..2]-(otherPerson:Person)-[workAt:workAt]->(company:Company)-[:isLocatedIn]->(country:Country)
WHERE person <> otherPerson
  AND country.name = $countryName
  AND workAt.workFrom < $workFromYear
RETURN otherPerson.ldbcId AS personId,
       otherPerson.firstName AS personFirstName,
       otherPerson.lastName AS personLastName,
       company.name AS organizationName,
       workAt.workFrom AS organizationWorkFromYear
ORDER BY organizationWorkFromYear ASC, personId ASC, organizationName DESC
LIMIT 10
