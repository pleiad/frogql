// Cypher translation of bench/ldbc-queries/ic1.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic1.toml. Transitive friends
// (1..3 knows hops) named $firstName, with study/work history.
//
// Neo4j-specific divergences (see DIVERGENCES.md):
//   - `ANY SHORTEST (p)~[:knows]~{1,3}(friend)` → bind both endpoints,
//     then `shortestPath((p)-[:knows*1..3]-(friend))` (one shortest
//     path per pair, length bounded at 3 — a pair whose shortest
//     distance exceeds 3 simply doesn't match, same as the bounded
//     repetition). `length(path)` is the per-friend distance.
//   - No `:University`/`:City`/`:Company`/`:Country` sub-labels: the
//     loader keeps Organisation/Place flat with a `type` property.
//   - Person MVAs are `email` / `language` list properties; the toml
//     names them friend.email / friend.speaks.
//   - `COLLECT_LIST(RECORD {...})` → `collect(CASE WHEN <node> IS
//     NULL THEN NULL ELSE {...} END)` (list of maps). The CASE wrap
//     reproduces gqlite's aggregate null-elimination: a RECORD over
//     unbound OPTIONAL vars evaluates to null there and is dropped
//     from COLLECT_LIST (empty list), whereas Cypher's collect of a
//     map-of-nulls would keep one all-null entry. collect() skips
//     null inputs, so the CASE restores the empty-list behavior.
//   - Implicit GROUP BY on the non-aggregated RETURN terms.
//   - The pre-collect `ORDER BY uni.id, company.id` pins the
//     study/work cross-product rows to ascending organisation id.
//     gqlite produces that order naturally (its OPTIONAL MATCH
//     expansion binds nodes in internal-id order, which is the
//     organisation CSV order = ascending LDBC id); Neo4j's natural
//     traversal order is reverse insertion order, so without the sort
//     the collected lists are permutations of each other and the
//     row hash diverges on order alone. Deterministic either way.
MATCH (p:Person {id: $personId})
MATCH (friend:Person {firstName: $firstName})
  WHERE p <> friend
MATCH path = shortestPath((p)-[:knows*1..3]-(friend))
OPTIONAL MATCH (friend)-[studyAt:studyAt]->(uni:Organisation)-[:isLocatedIn]->(uniCity:Place)
  WHERE uni.type = 'university'
OPTIONAL MATCH (friend)-[workAt:workAt]->(company:Organisation)-[:isLocatedIn]->(companyCountry:Place)
  WHERE company.type = 'company'
WITH path, friend, studyAt, uni, uniCity, workAt, company, companyCountry
ORDER BY uni.id ASC, company.id ASC
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
       collect(CASE WHEN uni IS NULL THEN NULL ELSE {uniName: uni.name, classYear: studyAt.classYear, uniCityName: uniCity.name} END) AS friendUniversities,
       collect(CASE WHEN company IS NULL THEN NULL ELSE {companyName: company.name, workFrom: workAt.workFrom, companyCountryName: companyCountry.name} END) AS friendCompanies
ORDER BY distanceFromPerson ASC, friendLastName ASC, friendId ASC
LIMIT 20
