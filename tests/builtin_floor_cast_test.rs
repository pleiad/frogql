//! FLOOR + CAST built-in function tests (ISO §20.x).

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

fn one_node() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "n1", "labels": ["N"], "props": {"x": 7.8, "n": 5, "s": "42"}}
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
        other => panic!("expected projected rows, got {other:?}"),
    }
}

#[test]
fn test_floor_float() {
    let g = one_node();
    let rows = run(&g, "MATCH (x:N) RETURN FLOOR(x.x) AS f");
    assert_eq!(rows, vec![vec![Value::Float(7.0)]]);
}

#[test]
fn test_floor_int_is_identity_as_float() {
    let g = one_node();
    let rows = run(&g, "MATCH (x:N) RETURN FLOOR(x.n) AS f");
    assert_eq!(rows, vec![vec![Value::Float(5.0)]]);
}

#[test]
fn test_cast_float_to_integer_truncates() {
    let g = one_node();
    let rows = run(&g, "MATCH (x:N) RETURN CAST(x.x AS INTEGER) AS i");
    assert_eq!(rows, vec![vec![Value::Int(7)]]);
}

#[test]
fn test_cast_int_to_float() {
    let g = one_node();
    let rows = run(&g, "MATCH (x:N) RETURN CAST(x.n AS FLOAT) AS f");
    assert_eq!(rows, vec![vec![Value::Float(5.0)]]);
}

#[test]
fn test_cast_string_to_integer() {
    let g = one_node();
    let rows = run(&g, "MATCH (x:N) RETURN CAST(x.s AS INTEGER) AS i");
    assert_eq!(rows, vec![vec![Value::Int(42)]]);
}

#[test]
fn test_ic7_composition_cast_floor_division() {
    let g = one_node();
    // CAST(FLOOR(CAST(x.x AS FLOAT) / 2.0) AS INTEGER) = FLOOR(3.9) = 3
    let rows = run(
        &g,
        "MATCH (x:N) RETURN CAST(FLOOR(CAST(x.x AS FLOAT) / 2.0) AS INTEGER) AS r",
    );
    assert_eq!(rows, vec![vec![Value::Int(3)]]);
}

#[test]
fn test_floor_and_cast_usable_as_identifiers() {
    // Soft keywords: `floor` / `cast` not before a paren stay identifiers.
    let json = r#"{"nodes":[{"id":"n1","labels":["N"],"props":{"floor":3,"cast":4}}],"edges":[]}"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let rows = run(&g, "MATCH (x:N) RETURN x.floor AS a, x.cast AS b");
    assert_eq!(rows, vec![vec![Value::Int(3), Value::Int(4)]]);
}
