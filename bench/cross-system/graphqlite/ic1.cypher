// Cypher translation of bench/ldbc-queries/ic1.toml for graphqlite
// (colliery-io/graphqlite 0.4.4 from PyPI).
//
// VERDICT: UNSUPPORTED — graphqlite 0.4.4 cannot express IC1's combination of
// (a) a bidirectional variable-length friend search and (b) grouped
// COLLECT_LIST of record-typed rows. The two requirements are mutually
// exclusive in this engine (grouping cannot cross the UNION that the
// variable-length expansion forces).
//
// IC1 ("Transitive friends with a certain name") finds Persons named
// $firstName reachable within 1..3 knows-hops of $personId, ordered by
// distance/lastName/id, and for each returns the friend's profile plus the
// list of universities (studyAt) and companies (workAt) as collected records.
//
// BLOCKERS (verified 2026-06-12 against the loaded SF0.1 DB):
//
//   1. `~[:knows]~{1,3}` is a bidirectional variable-length walk.
//      graphqlite's undirected `-[:knows*1..3]-` is silently FORWARD-ONLY
//      (DIVERGENCES.md, ic9.cypher) — wrong on LDBC, where each knows pair is
//      stored once in one direction. The only correct expansion is a UNION
//      ALL of a 1-hop, 2-hop, and 3-hop branch built from bidirectional
//      single-hop `-[:knows]-`.
//
//   2. The RETURN needs grouped COLLECT_LIST of RECORDs:
//      `collect({uniName, classYear, uniCityName})` per friend. graphqlite
//      groups ONLY via `WITH <key>, <agg>` — but `WITH` inside a UNION branch
//      throws "no such table: _with_0", and a bare `RETURN <key>, collect()`
//      does NOT group (it returns a single global-aggregate row mislabeled
//      with the first row's key). So once the friend set is split across UNION
//      branches, no grouped collect is possible. (This corrects the earlier
//      "bare RETURN groups" assumption — verified false here.)
//
//   3. `collect(DISTINCT x)` does NOT actually dedupe in 0.4.4 — verified:
//      `collect(DISTINCT p.browserUsed)` returns `["Chrome","Chrome"]`. Even
//      a single-branch grouped collect would diverge from gqlite's deduped
//      COLLECT_LIST.
//
//   4. The two `OPTIONAL MATCH` legs (studyAt / workAt) feed two independent
//      record collections per friend. graphqlite has no way to collect two
//      separate per-group lists across a UNION-split friend set.
//
// There is no faithful graphqlite form. No query is shipped (any attempt
// either crashes on the UNION+WITH path or returns ungrouped/global-aggregate
// rows that diverge from the reference on every non-empty param). The runner
// will fail to find a runnable body; this header records the engine verdict.
//
// Shipped best-effort below is the 1-hop-only, non-grouped projection — it
// runs but is SEMANTICALLY WRONG (misses 2- and 3-hop friends, emits one row
// per studyAt/workAt instead of collected lists, no distance column from a
// named path). Reference gqlite returns up to 20 grouped rows with two
// record-list columns.
MATCH (p:Person {ldbcId: $personId})-[:knows]-(friend:Person)
WHERE friend.firstName = $firstName AND p <> friend
OPTIONAL MATCH (friend)-[studyAt:studyAt]->(uni:Organisation)-[:isLocatedIn]->(uniCity:Place)
OPTIONAL MATCH (friend)-[workAt:workAt]->(company:Organisation)-[:isLocatedIn]->(companyCountry:Place)
RETURN toInteger(friend.ldbcId) AS friendId,
       friend.lastName AS friendLastName,
       uni.name AS uniName,
       company.name AS companyName
ORDER BY friendLastName ASC, friendId ASC
LIMIT 20
