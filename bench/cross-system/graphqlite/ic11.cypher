// Cypher translation of bench/ldbc-queries/ic11.toml for graphqlite.
//
// Source-of-truth: bench/ldbc-queries/ic11.toml.
//
// graphqlite-specific divergences:
//   - `.ldbcId` instead of `.id`.
//   - `~[:knows]~{1, 2}` → `-[:knows*1..2]-`.
//   - Edge labels lowercase per loader convention.
//
// Encoding-of-sub-types divergence (vs spec): the LDBC SNB spec uses
// `:Company` and `:Country` as schema-level sub-labels of
// `:Organisation` and `:Place`. Our graphqlite loader keeps the
// parent labels flat (`Organisation` / `Place`), with the sub-type
// carried as a `type` property (lowercase: 'company'/'university',
// 'country'/'city'/'continent'). Filtering by `type` is semantically
// equivalent to the spec's `:Company` / `:Country` match. gqlite's
// loader achieves the same end-state via a synthesized compound
// label (`Organisation & Company`); the cross-system row-content
// hash oracle confirms identical rows across encodings.
MATCH (person:Person {ldbcId: $personId})-[:knows*1..2]-(otherPerson:Person)-[workAt:workAt]->(company:Organisation)-[:isLocatedIn]->(country:Place)
WHERE person <> otherPerson
  AND company.type = 'company'
  AND country.type = 'country'
  AND country.name = $countryName
  AND workAt.workFrom < $workFromYear
RETURN otherPerson.ldbcId AS personId,
       otherPerson.firstName AS personFirstName,
       otherPerson.lastName AS personLastName,
       company.name AS organizationName,
       workAt.workFrom AS organizationWorkFromYear
ORDER BY organizationWorkFromYear ASC, personId ASC, organizationName DESC
LIMIT 10
