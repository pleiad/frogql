// openCypher translation of bench/ldbc-queries/ic11.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic11.toml. Spec form.
// `:Company` and `:Country` sub-labels are present in Kuzu's
// loader (same as gqlite/graphqlite).
//
// Edge labels lowercase per loader convention.
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)-[workAt:workAt]->(company:Company)-[:isLocatedIn]->(country:Country)
WHERE person <> otherPerson
  AND country.name = $countryName
  AND workAt.workFrom < $workFromYear
RETURN otherPerson.id AS personId,
       otherPerson.firstName AS personFirstName,
       otherPerson.lastName AS personLastName,
       company.name AS organizationName,
       workAt.workFrom AS organizationWorkFromYear
ORDER BY organizationWorkFromYear ASC, personId ASC, organizationName DESC
LIMIT 10
