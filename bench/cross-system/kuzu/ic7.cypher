// openCypher translation of bench/ldbc-queries/ic7.toml for Kuzu.
//
// !! UNVERIFIED cross-system row-equivalence. IC7 projects a RECORD
//    column (`latestLike`) — the canonical row-hash compares structs
//    by repr, which differs across engines, so a hash MATCH is not
//    expected. The latency and the scalar columns are the comparable
//    parts. Verify + record the divergence on the server.
//
// Source-of-truth: bench/ldbc-queries/ic7.toml. Recent likers of a
// person's messages, with the LATEST like per liker (arg-max) and an
// isNew flag (liker is not already a friend).
//
// Kuzu-specific divergences (see DIVERGENCES.md):
//   - `(:Comment|Post)` → unlabeled bound var + `label(m) IN [...]`.
//   - The ISO `VALUE { MATCH ... ORDER BY ... LIMIT 1 }` arg-max is
//     expressed with Cypher's collect/order idiom: order likes per
//     liker by (likeCreationDate DESC, message.id ASC), collect into
//     a list of structs, take element [1] (Kuzu lists are 1-indexed)
//     as the latest. Same per-liker latest record.
//   - `minutesLatency` truncates `(likeDate - msgDate)/60000.0` to an
//     int; latency is non-negative so trunc == FLOOR (the toml form).
//   - `NOT EXISTS { (liker)~[:knows]~(person) }` → Kuzu existential
//     subquery; isNew = true when liker and person are not friends.
//   - GROUP BY liker (binding var) is implicit per-liker grouping
//     after the collect.
MATCH (person:Person {id: $personId})<-[:hasCreator]-(message)<-[liked:likes]-(liker:Person)
WHERE label(message) IN ['Comment', 'Post']
WITH person, liker, message, liked
ORDER BY liked.creationDate DESC, message.id ASC
// Kuzu requires SKIP/LIMIT after an ORDER BY in a WITH clause. A LIMIT far
// above any possible (liker, message, like) row count for one person never
// truncates, so the ordered collect below still sees every row — same result,
// just satisfies the binder. (1e9 ≫ likes-per-person through SF1000.)
LIMIT 1000000000
WITH person, liker, collect({
        likeCreationDate: liked.creationDate,
        commentOrPostId: message.id,
        commentOrPostContent: coalesce(message.content, message.imageFile),
        minutesLatency: cast((liked.creationDate - message.creationDate) / 60000.0 AS INT64)
     }) AS likeList
RETURN liker.id AS personId,
       liker.firstName AS personFirstName,
       liker.lastName AS personLastName,
       likeList[1] AS latestLike,
       NOT EXISTS { MATCH (liker)-[:knows]-(person) } AS isNew
ORDER BY latestLike.likeCreationDate DESC, personId ASC
LIMIT 20
