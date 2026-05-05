// openCypher translation of bench/ldbc-queries/ic2.toml for Kuzu
// (kuzudb 0.11.3 from PyPI).
//
// Source-of-truth IC2 lives in that toml; this file is a language
// translation. Substitution placeholders use Cypher's native `$param`
// syntax — Kuzu supports this directly via
// `Connection.execute(query, parameters)` where parameters is a dict
// whose keys do NOT include the `$` prefix.
//
// Divergences from spec, applied to every system for apples-to-apples:
//   - no ORDER BY (gqlite parser doesn't support it; we drop it from
//     this translation even though Kuzu handles it fine — fairness
//     with our own engine)
//   - no `coalesce(c.content, c.imageFile)` — we return c.content
//     directly (Comment+Post both have a `content` column in our
//     loaded subset; LDBC uses imageFile only for image-only Posts
//     which aren't in the subset we load)
//   - lowercase :knows / :hasCreator (loader convention)
//
// Structural shape — same as gqlite's: one MATCH with the message
// node `(c)` left unlabeled. Kuzu can resolve `c` automatically
// because `hasCreator` is declared as a multi-typed REL TABLE
// (`FROM Comment TO Person, FROM Post TO Person`), so the engine
// constrains `c` to be a Comment or a Post implicitly via the
// relationship type. This matches the canonical IC2 shape (one
// MATCH + implicit label disjunction) and avoids the `UNION ALL`
// rewrite we initially tried — which Kuzu accepts but which has
// awkward LIMIT semantics (LIMIT applies only to the second branch).
// See DIVERGENCES.md for the LIMIT-over-UNION ALL story.
MATCH (p:Person {id: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(c)
WHERE c.creationDate <= $maxDate
RETURN friend.id AS friend_id, friend.firstName AS friend_firstName,
       friend.lastName AS friend_lastName,
       c.id AS c_id, c.content AS c_content, c.creationDate AS c_creationDate
LIMIT 20
