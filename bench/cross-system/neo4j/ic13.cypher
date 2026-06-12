// Cypher translation of bench/ldbc-queries/ic13.toml for Neo4j 5.
//
// Source-of-truth: bench/ldbc-queries/ic13.toml. Single shortest
// path: the number of `knows` hops between two persons, -1 if none.
//
// Neo4j-specific divergences (see DIVERGENCES.md):
//   - `ANY SHORTEST (p1)~[:knows]~*(p2)` → Neo4j's
//     `shortestPath((p1)-[:knows*]-(p2))` (one shortest path,
//     unbounded hop count, any-direction over single-direction
//     storage).
//   - The endpoints are matched first, then an OPTIONAL shortestPath
//     binds the path so a disconnected pair yields a row with NULL
//     path → -1 (the toml's CASE WHEN PATH_LENGTH IS NULL).
//   - `PATH_LENGTH(path)` → `length(p)` (edge count).
MATCH (p1:Person {id: $person1Id}), (p2:Person {id: $person2Id})
OPTIONAL MATCH p = shortestPath((p1)-[:knows*]-(p2))
RETURN CASE WHEN p IS NULL THEN -1 ELSE length(p) END AS shortestPathLength
