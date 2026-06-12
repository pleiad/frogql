// Cypher translation of bench/ldbc-queries/ic12.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic12.toml. Expert search:
// friends whose comments reply to posts tagged with a tag whose class
// is (transitively) $tagClassName.
//
// Neo4j-specific divergences (see DIVERGENCES.md):
//   - `~[:knows]~` → `-[:knows]-` (any-direction single hop).
//   - `[:isSubclassOf]->{0,}` under the toml's ACYCLIC prefix →
//     Neo4j's unbounded recursive `-[:isSubclassOf*0..]->`. The
//     isSubclassOf hierarchy is a DAG and Neo4j's VLR never repeats
//     a relationship, so the unbounded form is finite and equals the
//     ACYCLIC-prefixed repetition.
//   - `COLLECT_LIST(DISTINCT tag.name)` → `collect(DISTINCT tag.name)`;
//     the list column is re-encoded by run.py into froGQL's
//     Value::List Debug form for the row hash (element order is each
//     engine's production order — see DIVERGENCES.md).
//   - Implicit GROUP BY on friend.id/firstName/lastName.
MATCH (person:Person {id: $personId})-[:knows]-(friend:Person)<-[:hasCreator]-(comment:Comment)-[:replyOf]->(post:Post)-[:hasTag]->(tag:Tag)-[:hasType]->(tagClass:TagClass)-[:isSubclassOf*0..]->(baseClass:TagClass {name: $tagClassName})
RETURN friend.id AS friendId,
       friend.firstName AS friendFirstName,
       friend.lastName AS friendLastName,
       collect(DISTINCT tag.name) AS tagNames,
       count(comment) AS replyCount
ORDER BY replyCount DESC, friendId ASC
LIMIT 20
