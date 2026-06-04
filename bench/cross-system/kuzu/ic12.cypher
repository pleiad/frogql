// openCypher translation of bench/ldbc-queries/ic12.toml for Kuzu.
//
// !! UNVERIFIED cross-system row-equivalence. IC12 projects a list
//    column (`tagNames` = COLLECT_LIST(DISTINCT tag.name)); list
//    element ORDER can differ across engines, so the row-hash may not
//    match even when the sets agree. Latency + replyCount are the
//    comparable parts. Verify + record on the server.
//
// Source-of-truth: bench/ldbc-queries/ic12.toml. Expert search:
// friends whose comments reply to posts tagged with a tag whose class
// is (transitively) $tagClassName.
//
// Kuzu-specific divergences (see DIVERGENCES.md):
//   - `~[:knows]~` → `-[:knows]-` (any-direction single hop).
//   - `[:isSubclassOf]->{0,}` (zero-or-more, the tagClass itself or
//     any ancestor) → Kuzu recursive `-[:isSubclassOf*0..10]->`.
//     isSubclassOf is a shallow DAG, so the bound never truncates.
//   - The toml wraps the operand in the ISO `ACYCLIC` mode to make
//     the `{0,}` repetition finite; Kuzu's bounded recursive join is
//     finite by construction, so no mode keyword is needed.
//   - `COLLECT_LIST(DISTINCT tag.name)` → `collect(DISTINCT tag.name)`.
//   - Implicit GROUP BY on friend.id/firstName/lastName.
MATCH (person:Person {id: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(comment:Comment)-[:replyOf]->(post:Post)-[:hasTag]->(tag:Tag)-[:hasType]->(tagClass:TagClass)-[:isSubclassOf*0..10]->(baseClass:TagClass {name: $tagClassName})
RETURN friend.id AS friendId,
       friend.firstName AS friendFirstName,
       friend.lastName AS friendLastName,
       collect(DISTINCT tag.name) AS tagNames,
       count(comment) AS replyCount
ORDER BY replyCount DESC, friendId ASC
LIMIT 20
