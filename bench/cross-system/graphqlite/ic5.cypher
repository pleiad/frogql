// Cypher translation of bench/ldbc-queries/ic5.toml for graphqlite
// (colliery-io/graphqlite 0.4.4 from PyPI).
//
// VERDICT: UNSUPPORTED — IC5 needs a bidirectional 1..2-hop friend set AND a
// per-forum grouped COUNT. In graphqlite 0.4.4 these two are mutually
// exclusive: the correct friend set forces a UNION, and grouped aggregation
// cannot cross a UNION. 2/15 row-hash match (the two empty-result params,
// where the impossibility is unobservable); the rest diverge or can't be
// produced.
//
// IC5 ("New groups") ranks the forums a person's friends/FoF recently joined
// by the number of posts those friends made in each forum.
//
// ROOT-CAUSE (verified 2026-06-12 against the loaded SF0.1 DB):
//
//   1. `~[:knows]~{1,2}` is a bidirectional variable-length walk. graphqlite's
//      undirected `-[:knows*1..2]-` is silently FORWARD-ONLY (DIVERGENCES.md,
//      ic9.cypher), so for any person whose knows edges are stored reverse it
//      finds ZERO friends. Verified for IC5/IC6 person 30786325579101:
//      bidirectional 1-hop = 9 friends, `*1..2` undirected = 0. The only
//      correct expansion is a UNION ALL of a 1-hop and a 2-hop branch built
//      from bidirectional single-hop `-[:knows]-`.
//
//   2. The RETURN groups per forum: `WITH forum, count(post) AS postCount`.
//      graphqlite groups ONLY through `WITH <key>, <agg>` (verified working,
//      including with a preceding OPTIONAL MATCH). But:
//        - `WITH` inside a UNION branch throws "no such table: _with_0"
//          (verified), so the grouped count cannot live in the UNION branches.
//        - a bare `RETURN forum.title, count(post)` does NOT group — it
//          returns a single global-aggregate row mislabeled with the first
//          row's title (verified; corrects the earlier "bare RETURN groups"
//          note). So pushing the count past the UNION as a bare RETURN is also
//          wrong.
//        - there is no CALL{}/aggregation OVER a UNION to merge per-forum
//          counts from the 1-hop and 2-hop branches.
//
//   So: WITH-grouping requires a non-UNION pattern, but the correct friend set
//   requires UNION. No graphqlite form satisfies both. Forward-only `*1..2`
//   with WITH-grouping runs but returns the WRONG friend set (0 rows for this
//   person); 1-hop-only with WITH-grouping misses all 2-hop friends.
//
// Best-effort shipped below: WITH-grouped over the forward-only `*1..2`
// (correct grouping machinery, WRONG friend set). It runs and is grouped, but
// the friend membership is wrong, so postCount and the forum set diverge from
// the gqlite reference on every non-empty param. `.ldbcId`, lowercase edge
// labels are the usual graphqlite divergences.
MATCH (person:Person {ldbcId: $personId})-[:knows*1..2]-(otherPerson:Person)
WHERE person <> otherPerson
MATCH (otherPerson)<-[hm:hasMember]-(forum:Forum)
WHERE hm.joinDate > $minDate
OPTIONAL MATCH (otherPerson)<-[:hasCreator]-(post:Post)<-[:containerOf]-(forum)
WITH forum, count(post) AS postCount
RETURN forum.title AS forumName,
       forum.ldbcId AS forumId,
       postCount
ORDER BY postCount DESC, forumId ASC
LIMIT 20
