//! GROUP BY a binding variable (ISO `<grouping element> ::=
//! <binding variable reference>`) and functional-dependency of
//! non-aggregate projections on grouping keys.

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::{compile_query, compile_query_unchecked};

/// Two people; person p1 created two posts, p2 created one. Used to test
/// that GROUP BY collapses the (creator, post) rows to one row per creator.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "p1", "labels": ["Person"], "props": {"name": "Alice"}},
        {"id": "p2", "labels": ["Person"], "props": {"name": "Bob"}},
        {"id": "m1", "labels": ["Post"], "props": {"t": "a"}},
        {"id": "m2", "labels": ["Post"], "props": {"t": "b"}},
        {"id": "m3", "labels": ["Post"], "props": {"t": "c"}}
      ],
      "edges": [
        {"id": "e1", "labels": ["created"], "props": {}, "endpoints": ["p1", "m1"], "directionality": "->"},
        {"id": "e2", "labels": ["created"], "props": {}, "endpoints": ["p1", "m2"], "directionality": "->"},
        {"id": "e3", "labels": ["created"], "props": {}, "endpoints": ["p2", "m3"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected rows, got {other:?}"),
    }
}

#[test]
fn test_group_by_var_collapses_to_one_row_per_node() {
    let g = graph();
    // Without GROUP BY this yields 3 rows (one per created edge). Grouping
    // by the `p` binding variable collapses to one row per creator.
    let mut rows = run(
        &g,
        "MATCH (p:Person)-[:created]->(m:Post) RETURN p.name AS name GROUP BY p ORDER BY name ASC",
    );
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("Alice".into())],
            vec![Value::Str("Bob".into())],
        ]
    );
}

#[test]
fn test_group_by_var_with_count() {
    let g = graph();
    let rows = run(
        &g,
        "MATCH (p:Person)-[:created]->(m:Post) \
         RETURN p.name AS name, COUNT(m) AS c GROUP BY p ORDER BY name ASC",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("Alice".into()), Value::Int(2)],
            vec![Value::Str("Bob".into()), Value::Int(1)],
        ]
    );
}

#[test]
fn test_projection_referencing_non_grouped_var_is_rejected() {
    // GROUP BY p but RETURN projects m.t (m is not grouped) → type error.
    let err = compile_query(
        "MATCH (p:Person)-[:created]->(m:Post) RETURN p.name AS name, m.t AS t GROUP BY p",
    );
    assert!(err.is_err(), "expected typecheck error, got {err:?}");
}

#[test]
fn test_group_by_property_still_requires_structural_match() {
    // Grouping by a property does not make sibling attributes available.
    let err = compile_query("MATCH (x:Person) RETURN x.name AS n GROUP BY x.name");
    assert!(
        err.is_ok(),
        "structural match on the key should pass: {err:?}"
    );
    // But projecting a different attribute under a property key is rejected.
    let bad = compile_query_unchecked("MATCH (x:Person) RETURN x.name GROUP BY x.name");
    assert!(bad.is_ok());
}
