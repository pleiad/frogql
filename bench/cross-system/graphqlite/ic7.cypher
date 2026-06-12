// Cypher translation of bench/ldbc-queries/ic7.toml for graphqlite
// (colliery-io/graphqlite 0.4.4 from PyPI).
//
// VERDICT: UNSUPPORTED — graphqlite 0.4.4 cannot express IC7's per-liker
// arg-max ("the LATEST like per liker"). The grouping infrastructure exists
// but breaks in exactly the shape IC7 needs.
//
// IC7 ("Recent likers of a person's messages") returns, for each Person who
// liked any of $personId's messages, the liker's profile plus the SINGLE
// latest like (its creationDate, the liked message id/content, and a derived
// minutesLatency), plus an isNew flag, ordered by latestLike date.
//
// BLOCKERS (verified 2026-06-12 against the loaded SF0.1 DB):
//
//   1. ARG-MAX per liker is not expressible. The standard Cypher idiom is a
//      two-stage `WITH liker, l, m ORDER BY l.creationDate DESC` then
//      `WITH liker, head(collect(...))`. graphqlite groups correctly on the
//      FIRST `WITH <key>, <agg>` (verified), but a SECOND aggregating `WITH`
//      after an ORDER BY does NOT re-group by `liker` — it collapses the whole
//      input to a single global group (verified: the double-WITH form returns
//      1 row total, not one per liker). So head(collect(... ORDER BY ...))
//      cannot pick a per-liker latest.
//
//   2. EDGE-property aggregation is BROKEN. `max(l.creationDate)` on the
//      `:likes` edge returns NULL inside a grouped `WITH liker, max(...)`
//      (verified), while `max(m.creationDate)` on a node works. IC7's
//      arg-max key is the EDGE's creationDate, so even a single-stage
//      max-based pick fails.
//
//   3. No VALUE subquery / no RECORD constructor. The toml uses
//      `VALUE { MATCH ... RETURN RECORD {...} ORDER BY ... LIMIT 1 }` to
//      build the per-liker latestLike record. graphqlite has neither a value
//      subquery nor a record/map result cell that survives the per-liker
//      pick; `collect()` returns a JSON STRING in a grouped RETURN
//      (fact-4 encoding) and there is no per-group ORDER+LIMIT-1 reduction.
//
//   4. CAST(FLOOR(... / 60000.0) AS INTEGER) minutesLatency depends on the
//      picked like+message pair, which (per blockers 1-2) can't be isolated
//      per liker.
//
// The likers themselves CAN be grouped (`WITH liker, count(*)` returns 20
// distinct likers, verified) and ordered, but the latestLike record column
// cannot be produced, so the row content diverges on every param.
//
// Shipped best-effort below: grouped likers with `max(m.creationDate)` (a
// NODE proxy, since the edge max is broken) as a stand-in latest-date. It
// RUNS but is SEMANTICALLY WRONG — wrong arg-max key, no latestLike record,
// no minutesLatency, isNew via the paren EXISTS form only. Reference gqlite
// returns 20 rows with a record-typed latestLike column and a boolean isNew.
MATCH (person:Person {ldbcId: $personId})<-[:hasCreator]-(m)<-[l:likes]-(liker:Person)
WHERE m:Comment OR m:Post
WITH liker, max(m.creationDate) AS approxLatestDate
RETURN toInteger(liker.ldbcId) AS personId,
       liker.firstName AS personFirstName,
       liker.lastName AS personLastName,
       approxLatestDate
ORDER BY approxLatestDate DESC, personId ASC
LIMIT 20
