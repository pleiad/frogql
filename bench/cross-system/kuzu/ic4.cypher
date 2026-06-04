// openCypher translation of bench/ldbc-queries/ic4.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic4.toml. New topics: tags a
// friend posted about inside the window but never before it.
//
// Kuzu-specific divergences (see DIVERGENCES.md):
//   - `~[:knows]~` → `-[:knows]-` (any-direction single hop).
//   - `DURATION({days:N})` → `N*86400000` ms; creationDate is int
//     epoch-millis, so the window bound is int arithmetic.
//   - `NOT EXISTS { MATCH ... }` is Kuzu's existential subquery
//     (supported since 0.4.x). The anti-join keeps only tags with no
//     pre-window post by the same friend.
//   - GROUP BY is implicit on the non-aggregated RETURN term
//     (tag.name); ORDER BY uses the `tagName` alias.
MATCH (person:Person {id: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(post:Post)-[:hasTag]->(tag:Tag)
WHERE post.creationDate >= $startDate
  AND post.creationDate < $startDate + $durationDays * 86400000
  AND NOT EXISTS {
        MATCH (friend)<-[:hasCreator]-(post2:Post)-[:hasTag]->(tag)
        WHERE post2.creationDate < $startDate
  }
RETURN tag.name AS tagName, count(post) AS postCount
ORDER BY postCount DESC, tagName ASC
LIMIT 10
