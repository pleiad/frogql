//! Tests for the `Value::Null` variant, the `null` literal, and the
//! `IS NULL` / `IS NOT NULL` operators.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

/// Three users, with Carol missing the `email` property. Used to exercise
/// the missing-attribute path that the engine treats as null.
fn graph_with_optional_email() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {
            "name": "Alice", "email": "alice@example.com"
        }},
        {"id": "u2", "labels": ["User"], "props": {
            "name": "Bob", "email": "bob@example.com"
        }},
        {"id": "u3", "labels": ["User"], "props": {
            "name": "Carol"
        }}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected result for query {q:?}"),
    }
}

#[test]
fn test_is_null_matches_missing_property() {
    let g = graph_with_optional_email();
    let rows = run(&g, "MATCH (x: User) WHERE x.email IS NULL RETURN x.name");
    assert_eq!(rows, vec![vec![Value::Str("Carol".into())]]);
}

#[test]
fn test_is_not_null_matches_present_property() {
    let g = graph_with_optional_email();
    let mut names: Vec<String> = run(
        &g,
        "MATCH (x: User) WHERE x.email IS NOT NULL RETURN x.name",
    )
    .into_iter()
    .map(|row| match &row[0] {
        Value::Str(s) => s.clone(),
        other => panic!("expected Str, got {other:?}"),
    })
    .collect();
    names.sort();
    assert_eq!(names, vec!["Alice", "Bob"]);
}

#[test]
fn test_eq_null_drops_all_rows_under_3vl() {
    // Comparison against null literal yields false → predicate is null →
    // no row passes. This is SQL behavior; users must use IS NULL.
    let g = graph_with_optional_email();
    let rows = run(&g, "MATCH (x: User) WHERE x.email = null RETURN x.name");
    assert!(rows.is_empty(), "expected no rows, got {rows:?}");
}

#[test]
fn test_null_literal_in_return() {
    let g = graph_with_optional_email();
    let rows = run(&g, "MATCH (x: User) RETURN null");
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(row, &vec![Value::Null]);
    }
}

#[test]
fn test_is_null_uppercase() {
    // `NULL` keyword in upper case parses identically.
    let g = graph_with_optional_email();
    let rows = run(&g, "MATCH (x: User) WHERE x.email IS NULL RETURN x.name");
    assert_eq!(rows, vec![vec![Value::Str("Carol".into())]]);
}

#[test]
fn test_empty_aggregate_returns_null_value() {
    // Pure-aggregate over an empty set produces `Value::Null`, not a
    // string sentinel. Spelling: the typechecker accepts the query but
    // the schema rejects ImpossibleLabel; switch to a label that exists
    // and a filter that excludes everything.
    let g = graph_with_optional_email();
    let rows = run(
        &g,
        "MATCH (x: User) WHERE x.name = 'Nobody' RETURN SUM(x.age)",
    );
    assert_eq!(rows, vec![vec![Value::Null]]);
}
