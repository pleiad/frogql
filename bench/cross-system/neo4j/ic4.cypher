// Cypher translation of bench/ldbc-queries/ic4.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic4.toml. New topics: tags a
// friend posted about inside the window but never before it.
//
// Neo4j-specific divergences (see DIVERGENCES.md):
//   - `~[:knows]~` → `-[:knows]-` (any-direction single hop).
//   - `DURATION({days:N})` → `N*86400000` ms int arithmetic.
//   - `NOT EXISTS { MATCH ... }` → Neo4j 5 existential subquery,
//     same shape as the toml's.
//   - GROUP BY is implicit on the non-aggregated RETURN term.
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
