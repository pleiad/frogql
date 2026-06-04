// openCypher translation of bench/ldbc-queries/ic13.toml for Kuzu.
//
// Source-of-truth: bench/ldbc-queries/ic13.toml. Single shortest
// path: the number of `knows` hops between two persons, -1 if none.
//
// Kuzu-specific divergences (see DIVERGENCES.md):
//   - `ANY SHORTEST (p1)~[:knows]~*(p2)` → Kuzu's recursive SHORTEST
//     join `-[:knows* SHORTEST 1..30]-`. Kuzu requires a bounded
//     upper hop count on recursive joins; SF0.1's `knows` component
//     diameter is well under 30, so the bound never truncates a real
//     shortest path (a -1 means the two persons are in different
//     components, matching the toml's NULL→-1).
//   - The endpoints are matched first, then an OPTIONAL recursive
//     SHORTEST binds the path so a disconnected pair yields a row
//     with NULL path → -1 (the toml's CASE WHEN PATH_LENGTH IS NULL).
//   - `PATH_LENGTH(path)` → `length(p)` (edge count of the bound path).
MATCH (p1:Person {id: $person1Id}), (p2:Person {id: $person2Id})
OPTIONAL MATCH p = (p1)-[:knows* SHORTEST 1..30]-(p2)
RETURN CASE WHEN p IS NULL THEN -1 ELSE length(p) END AS shortestPathLength
