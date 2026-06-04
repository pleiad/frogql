// openCypher translation of bench/ldbc-queries/ic1.toml for Kuzu.
//
// !! UNVERIFIED cross-system row-equivalence. IC1 projects two
//    list-of-struct columns (friendUniversities / friendCompanies)
//    and two Person multi-valued attributes (email / language). The
//    canonical row-hash oracle compares struct/list columns by repr,
//    which differs across engines, so a hash MATCH is not expected
//    here — the value of this translation is the LATENCY measurement
//    and the scalar columns. Verify on the server and record the
//    divergence rather than assuming parity.
//
// Source-of-truth: bench/ldbc-queries/ic1.toml. Transitive friends
// (1..3 knows hops) named $firstName, with their study/work history.
//
// Kuzu-specific divergences (see DIVERGENCES.md):
//   - `ANY SHORTEST (p)~[:knows]~{1,3}(friend {firstName})` → Kuzu's
//     recursive SHORTEST join to each friend, with the firstName /
//     `p <> friend` filter on the bound endpoint. `length(path)` is
//     the per-friend shortest distance.
//   - No `:University` / `:City` / `:Company` / `:Country` sub-labels:
//     reached via `Organisation`/`Place` filtered by `type`.
//   - Person MVAs are `email` / `language` columns (STRING[]); the
//     toml names them friend.email / friend.speaks.
//   - `COLLECT_LIST(RECORD {...})` → Kuzu `collect({...})` (list of
//     STRUCTs). With OPTIONAL non-matches the struct fields are NULL,
//     mirroring the toml's null-filled record.
//   - Implicit GROUP BY on the non-aggregated RETURN terms.
MATCH (p:Person {id: $personId})
MATCH path = (p)-[:knows* SHORTEST 1..3]-(friend:Person)
WHERE friend.firstName = $firstName AND p <> friend
OPTIONAL MATCH (friend)-[studyAt:studyAt]->(uni:Organisation)-[:isLocatedIn]->(uniCity:Place)
  WHERE uni.type = 'university'
OPTIONAL MATCH (friend)-[workAt:workAt]->(company:Organisation)-[:isLocatedIn]->(companyCountry:Place)
  WHERE company.type = 'company'
RETURN friend.id AS friendId,
       friend.lastName AS friendLastName,
       length(path) AS distanceFromPerson,
       friend.birthday AS friendBirthday,
       friend.creationDate AS friendCreationDate,
       friend.gender AS friendGender,
       friend.browserUsed AS friendBrowserUsed,
       friend.locationIP AS friendLocationIp,
       friend.email AS friendEmails,
       friend.language AS friendLanguages,
       uniCity.name AS friendCityName,
       collect({uniName: uni.name, classYear: studyAt.classYear, uniCityName: uniCity.name}) AS friendUniversities,
       collect({companyName: company.name, workFrom: workAt.workFrom, companyCountryName: companyCountry.name}) AS friendCompanies
ORDER BY distanceFromPerson ASC, friendLastName ASC, friendId ASC
LIMIT 20
