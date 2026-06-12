// Cypher translation of bench/ldbc-queries/ic11.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic11.toml.
//
// Encoding-of-sub-types divergence (vs spec): the LDBC SNB spec uses
// `:Company` / `:Country` sub-labels. Our Neo4j loader keeps the
// parent labels flat (`Organisation` / `Place`) with the sub-type as
// a `type` property — same encoding as the Kuzu loader; gqlite gets
// there via synthesized compound labels. The row-content hash oracle
// confirms identical rows across encodings.
//
// Flat-chain (no WITH DISTINCT) per the toml's no-with-distinct
// divergence. Edge labels lowercase per loader convention.
MATCH (person:Person {id: $personId})-[:knows*1..2]-(otherPerson:Person)-[workAt:workAt]->(company:Organisation)-[:isLocatedIn]->(country:Place)
WHERE person <> otherPerson
  AND company.type = 'company'
  AND country.type = 'country'
  AND country.name = $countryName
  AND workAt.workFrom < $workFromYear
RETURN otherPerson.id AS personId,
       otherPerson.firstName AS personFirstName,
       otherPerson.lastName AS personLastName,
       company.name AS organizationName,
       workAt.workFrom AS organizationWorkFromYear
ORDER BY organizationWorkFromYear ASC, personId ASC, organizationName DESC
LIMIT 10
