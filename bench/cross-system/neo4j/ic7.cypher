// Cypher translation of bench/ldbc-queries/ic7.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic7.toml. Recent likers of a
// person's messages, with the LATEST like per liker (arg-max) and an
// isNew flag (liker is not already a friend).
//
// Neo4j-specific divergences (see DIVERGENCES.md):
//   - `(:Comment|Post)` — native label-expression disjunction.
//   - The ISO `VALUE { MATCH ... ORDER BY ... LIMIT 1 }` arg-max is
//     expressed with Cypher's ORDER BY + collect()[0] idiom: order
//     the (liker, message, like) rows by (likeCreationDate DESC,
//     message.id ASC), collect the per-liker maps in that order, and
//     take the head as the latest like. Same per-liker record.
//   - `minutesLatency` = toInteger(floor(Δms / 60000.0)); latency is
//     non-negative so this equals the toml's CAST(FLOOR(...)).
//   - `NOT EXISTS { (liker)~[:knows]~(person) }` → Neo4j existential
//     pattern subquery; isNew = true when they are not friends.
//   - The `latestLike` map column is re-encoded by run.py into
//     froGQL's Value::Record Debug form for the row hash.
MATCH (person:Person {id: $personId})<-[:hasCreator]-(message:Comment|Post)<-[liked:likes]-(liker:Person)
WITH person, liker, message, liked
ORDER BY liked.creationDate DESC, message.id ASC
WITH person, liker, collect({
        likeCreationDate: liked.creationDate,
        commentOrPostId: message.id,
        commentOrPostContent: coalesce(message.content, message.imageFile),
        minutesLatency: toInteger(floor((liked.creationDate - message.creationDate) / 60000.0))
     })[0] AS latestLike
RETURN liker.id AS personId,
       liker.firstName AS personFirstName,
       liker.lastName AS personLastName,
       latestLike,
       NOT EXISTS { (liker)-[:knows]-(person) } AS isNew
ORDER BY latestLike.likeCreationDate DESC, personId ASC
LIMIT 20
