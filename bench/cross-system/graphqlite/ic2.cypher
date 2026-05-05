// Cypher translation of bench/ldbc-queries/ic2.toml.
//
// Source-of-truth IC2 lives in that toml; this file is a language
// translation. Substitution placeholders use graphqlite's native
// $-prefixed parameter syntax.
//
// Divergences from spec, applied to every system for apples-to-apples:
//   - no ORDER BY (gqlite parser doesn't support it)
//   - no `coalesce(c.content, c.imageFile)` — we return c.content directly
//   - lowercase :knows / :hasCreator (loader convention)
//
// graphqlite-specific divergences:
//   - The start-node predicate is `{ldbcId: $personId}` not `{id: $personId}`
//     because graphqlite's `.id` accessor returns the loader's
//     prefixed external_id (`"Person:933"`), not the int LDBC id.
//     Same reason `friend.ldbcId` and `c.ldbcId` appear in RETURN
//     where gqlite uses `.id`. See DIVERGENCES.md.
//
// Structural shape: single MATCH with a label predicate in WHERE.
// graphqlite's Cypher dialect rejects `(c:Comment|Post)` (Cypher 5+
// feature, not in this dialect), so we express the same logical
// query with `WHERE c:Comment OR c:Post`. This matches the
// structural shape of gqlite's `(c: Comment | Post)` form — both
// systems do one MATCH then a label-disjunction filter — so the
// cross-system comparison reflects each engine's evaluation of
// the same query shape, not different shapes of the same logical
// query. (graphqlite's UNION ALL of two MATCHes is faster on this
// engine, but that's a structurally different query than what
// gqlite runs; using it here would compare apples to oranges.)
MATCH (p:Person {ldbcId: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(c)
WHERE (c:Comment OR c:Post) AND c.creationDate <= $maxDate
RETURN friend.ldbcId AS friend_id, friend.firstName AS friend_firstName,
       friend.lastName AS friend_lastName,
       c.ldbcId AS c_id, c.content AS c_content, c.creationDate AS c_creationDate
LIMIT 20
