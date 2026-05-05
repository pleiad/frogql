// OpenCypher translation of bench/ldbc-queries/ic2.toml for
// auksys/gqlite (gqlite.org, distribution `gqlitedb` on PyPI).
//
// Source-of-truth IC2 lives in that toml; this file is a language
// translation. Substitution placeholders use Cypher's $-prefixed
// parameter syntax — auksys/gqlite supports it natively via
// `Connection.execute_oc_query(query, bindings)` where `bindings`
// is a dict whose keys INCLUDE the `$` prefix (see DIVERGENCES.md).
//
// Divergences from spec, applied to every system for apples-to-apples:
//   - no ORDER BY (gqlite parser doesn't support it; we drop it
//     from this translation even though auksys/gqlite handles it
//     fine — fairness with our own engine)
//   - no `coalesce(c.content, c.imageFile)` — we return c.content
//     directly (Comment+Post both have a `content` column in our
//     loaded subset; the original LDBC spec uses imageFile only for
//     image-only Posts which we don't represent)
//   - lowercase :knows / :hasCreator (loader convention)
//
// Structural shape: one MATCH with a label-disjunction predicate.
// auksys/gqlite supports `(c:Comment|Post)` syntax in standalone
// patterns (Cypher 5+ feature) — but in multi-hop patterns
// containing it, the planner errors with
// `CompileTime: UnknownFunction: get_source` (gqlitedb 1.5.1).
// Reproducer is one MATCH with two named edges and a union-label
// terminal node:
//
//   MATCH (a)-[:knows]-(b)<-[:hasCreator]-(c:Comment|Post)
//
// fails, while the same query with `(c)` and `WHERE c:Comment OR
// c:Post` works. We use the OR-form here, which is also what
// graphqlite uses for the same logical query (graphqlite's dialect
// doesn't accept `:A|B` at all). The structural shape — one MATCH
// then a label predicate — matches gqlite's `(c: Comment | Post)`.
MATCH (p:Person {id: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(c)
WHERE (c:Comment OR c:Post) AND c.creationDate <= $maxDate
RETURN friend.id AS friend_id, friend.firstName AS friend_firstName,
       friend.lastName AS friend_lastName,
       c.id AS c_id, c.content AS c_content, c.creationDate AS c_creationDate
LIMIT 20
