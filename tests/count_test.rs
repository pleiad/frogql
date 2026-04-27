//! Aggregate-function runtime tests (ISO 39075 §20.9).
//!
//! Covers the four shapes the aggregate runtime supports incrementally:
//!  - Commit 4 (this file's first batch): `COUNT(*)`.
//!  - Commit 5: `COUNT(expr)` and `COUNT(DISTINCT expr)`.
//!  - Commit 6: `SUM`, `AVG`, `MIN`, `MAX`.
//!  - Commit 7: implicit GROUP BY (RETURN mixing aggregates with plain exprs).

use gqlrust::compile_query;
use gqlrust::model::graph::Graph;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

/// Three-user fixture used across most aggregate tests. Two users in
/// Boston, one in Seattle; ages 30/25/40.
fn graph_three_users() -> Graph {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {
            "name": "Alice", "city": "Boston", "age": 30
        }},
        {"id": "u2", "labels": ["User"], "props": {
            "name": "Bob", "city": "Boston", "age": 25
        }},
        {"id": "u3", "labels": ["User"], "props": {
            "name": "Carol", "city": "Seattle", "age": 40
        }}
      ],
      "edges": []
    }"#;
    Graph::from_json_str(json).unwrap()
}

/// Helper: compile + run a query and return the projected rows.
fn run(g: &Graph, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected result for query {q:?}"),
    }
}

// =======================================================================
// COUNT(*) — ISO §20.9 `COUNT(*)` form (no inner expression, no DISTINCT)
// =======================================================================

#[test]
fn test_count_star_total() {
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(*)"),
        vec![vec![Value::Int(3)]]
    );
}

#[test]
fn test_count_star_no_match_emits_zero_row() {
    // ISO §20.9 GR 7a-i + the implicit-group-by edge case: a pure-
    // aggregate query over zero matches still emits one row with the
    // empty-group result. For COUNT, that's 0 — never the empty table.
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: ImpossibleLabel) RETURN COUNT(*)"),
        vec![vec![Value::Int(0)]]
    );
}

#[test]
fn test_count_star_with_filter() {
    let g = graph_three_users();
    // Two users have age > 25 (Alice 30, Carol 40).
    assert_eq!(
        run(&g, "MATCH (x: User) WHERE x.age > 25 RETURN COUNT(*)"),
        vec![vec![Value::Int(2)]]
    );
}

#[test]
fn test_count_star_with_alias() {
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(*) AS total"),
        vec![vec![Value::Int(3)]]
    );
}

// =======================================================================
// COUNT(expr) — ISO §20.9 <general set function>, kind = Count
//
// Differs from COUNT(*): null inputs are eliminated before counting.
// In gqlite that means "Failure" results from run_expr (e.g. attribute
// not present on the node) drop out instead of counting as 1.
// =======================================================================

#[test]
fn test_count_expr_total() {
    let g = graph_three_users();
    // Every user has a name → COUNT(x.name) == 3.
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(x.name)"),
        vec![vec![Value::Int(3)]]
    );
}

#[test]
fn test_count_expr_skips_failures() {
    // Two of the three users have a "nickname" property; the third
    // doesn't. ISO null-elimination drops that row from COUNT(x.nickname).
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Alice", "nickname": "Al"}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Bob", "nickname": "Bo"}},
        {"id": "u3", "labels": ["User"], "props": {"name": "Carol"}}
      ],
      "edges": []
    }"#;
    let g = Graph::from_json_str(json).unwrap();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(x.nickname)"),
        vec![vec![Value::Int(2)]]
    );
}

#[test]
fn test_count_distinct() {
    let g = graph_three_users();
    // Cities: Boston, Boston, Seattle → 2 distinct.
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(DISTINCT x.city)"),
        vec![vec![Value::Int(2)]]
    );
}

#[test]
fn test_count_distinct_with_alias() {
    let g = graph_three_users();
    assert_eq!(
        run(
            &g,
            "MATCH (x: User) RETURN COUNT(DISTINCT x.city) AS unique_cities"
        ),
        vec![vec![Value::Int(2)]]
    );
}

#[test]
fn test_count_distinct_skips_failures() {
    // DISTINCT applies AFTER null-elimination, so missing nickname
    // doesn't count as "a distinct null". Two users have nicknames
    // ("Al" and "Bo"), both distinct → 2.
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"nickname": "Al"}},
        {"id": "u2", "labels": ["User"], "props": {"nickname": "Bo"}},
        {"id": "u3", "labels": ["User"], "props": {}}
      ],
      "edges": []
    }"#;
    let g = Graph::from_json_str(json).unwrap();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(DISTINCT x.nickname)"),
        vec![vec![Value::Int(2)]]
    );
}

#[test]
fn test_count_expr_zero_match_emits_zero_row() {
    // Same edge case as COUNT(*) but for COUNT(expr): pure-aggregate
    // query over zero matches still emits one row with count = 0.
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: ImpossibleLabel) RETURN COUNT(x.name)"),
        vec![vec![Value::Int(0)]]
    );
}

#[test]
fn test_count_star_groupby_implicit() {
    // Two non-aggregate groups (Boston: 2, Seattle: 1) + COUNT(*) per group.
    // The output order is the order the runtime first sees each group key.
    let g = graph_three_users();
    let rs = run(&g, "MATCH (x: User) RETURN x.city, COUNT(*)");
    assert_eq!(rs.len(), 2);

    // Sort by city for deterministic comparison (insertion order depends
    // on graph node ordering, which is stable but fixture-dependent).
    let mut sorted = rs.clone();
    sorted.sort_by(|a, b| match (&a[0], &b[0]) {
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    });
    assert_eq!(
        sorted,
        vec![
            vec![Value::Str("Boston".into()), Value::Int(2)],
            vec![Value::Str("Seattle".into()), Value::Int(1)],
        ]
    );
}
