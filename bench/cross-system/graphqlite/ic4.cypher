// Cypher translation of bench/ldbc-queries/ic4.toml for graphqlite
// (colliery-io/graphqlite 0.4.4 from PyPI).
//
// VERDICT: UNSUPPORTED — graphqlite 0.4.4 cannot express IC4's
// date-restricted NOT EXISTS anti-join.
//
// IC4 ("New topics") returns, for each tag, the count of a friend's posts
// in a date window for tags that are NEW to the friend — i.e. the friend
// has NO post with that tag created BEFORE the window. That anti-join needs
// a correlated existence check carrying a `creationDate < startDate`
// predicate:
//     NOT EXISTS { (friend)<-[:hasCreator]-(post2:Post)-[:hasTag]->(tag)
//                  WHERE post2.creationDate < $startDate }
//
// graphqlite blockers (verified against the 0.4.4 grammar + the loaded
// SF0.1 DB, 2026-06-12):
//
//   1. EXISTS has NO WHERE clause. The grammar (src/backend/parser/
//      cypher_gram.y) only defines `EXISTS '(' pattern_list ')'` and
//      `EXISTS(property)`. The brace subquery form `EXISTS { pattern
//      WHERE expr }` is declared in the AST (cypher_ast.h: where_clause /
//      is_subquery) but NO grammar rule populates it, so:
//          ... WHERE NOT EXISTS { (f)<-[:hasCreator]-(p:Post)
//                                 WHERE p.creationDate < $startDate } ...
//      → "Error: Line 1, Col 54: syntax error, unexpected '{',
//         expecting '('".
//      The paren form `NOT EXISTS((friend)<-[:hasCreator]-(:Post)
//      -[:hasTag]->(tag))` DOES correlate the outer friend+tag (verified:
//      it returns 0 rows because the current in-window post always
//      satisfies it), but it cannot restrict to posts BEFORE the window —
//      so it filters out every tag, not just the non-new ones.
//
//   2. Pattern comprehensions do NOT correlate outer variables. A
//      `size([ (friend)<-[:hasCreator]-(post2:Post)
//             WHERE post2.creationDate < $startDate | post2 ])` as an
//      anti-join predicate evaluates to the SAME constant (28160, the
//      global count of pre-window posts) for every (friend, tag) row —
//      the comprehension ignores the outer `friend` binding. So the
//      list-comprehension route to a correlated date anti-join also fails.
//      Two-hop pattern comprehensions don't even parse (the grammar allows
//      a single rel_pattern + node_pattern only).
//
//   3. Bonus bug found here: graphqlite does NOT implicitly GROUP BY on a
//      `RETURN tag.name, COUNT(post)` clause — it collapses to a single
//      global aggregate row. Grouping only works through `WITH tag.name
//      AS tagName, COUNT(post) AS postCount` (verified). IC4's translation
//      would therefore also need the WITH form even if the anti-join were
//      expressible.
//
// Best faithful attempt (shipped so the runner records the engine's own
// verdict). It runs but is SEMANTICALLY WRONG — the un-dated NOT EXISTS
// filters out every tag, yielding 0 rows on every param. Reference gqlite
// returns 1-10 grouped rows on the non-empty params.
MATCH (person:Person {ldbcId: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(post:Post)-[:hasTag]->(tag:Tag)
WHERE post.creationDate >= $startDate
  AND post.creationDate < $startDate + $durationDays * 86400000
  AND NOT EXISTS((friend)<-[:hasCreator]-(:Post)-[:hasTag]->(tag))
WITH tag.name AS tagName, COUNT(post) AS postCount
RETURN tagName, postCount
ORDER BY postCount DESC, tagName ASC
LIMIT 10
