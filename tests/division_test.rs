//! Division operator `/` (ISO §24 <solidus>) runtime tests.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

fn one_node() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "n1", "labels": ["N"], "props": {"a": 7, "b": 2, "f": 9.0, "z": 0}}
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
fn test_int_division_truncates() {
    let g = one_node();
    let rows = run(&g, "MATCH (x:N) RETURN x.a / x.b AS d");
    assert_eq!(rows, vec![vec![Value::Int(3)]]);
}

#[test]
fn test_float_division() {
    let g = one_node();
    let rows = run(&g, "MATCH (x:N) RETURN x.f / 2.0 AS d");
    assert_eq!(rows, vec![vec![Value::Float(4.5)]]);
}

#[test]
fn test_mixed_int_float_division_widens() {
    let g = one_node();
    let rows = run(&g, "MATCH (x:N) RETURN x.a / 2.0 AS d");
    assert_eq!(rows, vec![vec![Value::Float(3.5)]]);
}

#[test]
fn test_precedence_division_over_addition() {
    let g = one_node();
    // 1 + 6 / 2 = 1 + 3 = 4 (division binds tighter)
    let rows = run(&g, "MATCH (x:N) RETURN 1 + 6 / 2 AS d");
    assert_eq!(rows, vec![vec![Value::Int(4)]]);
}

#[test]
fn test_division_by_zero_is_null() {
    let g = one_node();
    // Failure (div by zero) surfaces as Null under 3VL.
    let rows = run(&g, "MATCH (x:N) RETURN x.a / x.z AS d");
    assert_eq!(rows, vec![vec![Value::Null]]);
}
