//! Phase 1: List values — JSON loading, `in` operator, type predicates,
//! list literals, `[T]` type annotations, and .gdb storage roundtrip.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::store::lazy::LazyGraphStore;

fn graph_with_lists() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a1", "labels": ["Actor"], "props": {"name": "Keanu", "roles": ["Neo", "Agent"]}},
        {"id": "a2", "labels": ["Actor"], "props": {"name": "Carrie", "roles": ["Trinity"]}},
        {"id": "a3", "labels": ["Actor"], "props": {"name": "Laurence", "roles": []}}
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
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_json_loads_list_property() {
    let g = graph_with_lists();
    let r = run(&g, "MATCH (x: Actor) WHERE x.name = 'Keanu' RETURN x.roles");
    assert_eq!(
        r,
        vec![vec![Value::List(vec![
            Value::Str("Neo".into()),
            Value::Str("Agent".into())
        ])]]
    );
}

#[test]
fn test_is_list_type_predicate() {
    let g = graph_with_lists();
    let r = run(&g, "MATCH (x: Actor) WHERE x.roles [str] RETURN x.name");
    assert_eq!(r.len(), 3); // all three match: empty list matches any element type
}

#[test]
fn test_is_list_star_predicate() {
    let g = graph_with_lists();
    let r = run(&g, "MATCH (x: Actor) WHERE x.roles [*] RETURN x.name");
    assert_eq!(r.len(), 3);
}

#[test]
fn test_in_operator_membership() {
    let g = graph_with_lists();
    // Find actors who played Neo.
    let r = run(&g, "MATCH (x: Actor) WHERE 'Neo' in x.roles RETURN x.name");
    assert_eq!(r, vec![vec![Value::Str("Keanu".into())]]);
}

#[test]
fn test_in_operator_literal_list() {
    let g = graph_with_lists();
    let r = run(
        &g,
        "MATCH (x: Actor) WHERE x.name in ['Keanu', 'Carrie'] RETURN x.name",
    );
    assert_eq!(r.len(), 2);
}

#[test]
fn test_list_literal_in_return() {
    let g = graph_with_lists();
    let r = run(
        &g,
        "MATCH (x: Actor) WHERE x.name = 'Keanu' RETURN [1, 2, 3]",
    );
    assert_eq!(
        r,
        vec![vec![Value::List(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3)
        ])]]
    );
}

#[test]
fn test_empty_list_literal() {
    let g = graph_with_lists();
    let r = run(
        &g,
        "MATCH (x: Actor) WHERE x.name = 'Laurence' RETURN x.roles",
    );
    assert_eq!(r, vec![vec![Value::List(vec![])]]);
}

#[test]
fn test_list_storage_roundtrip() {
    let g = graph_with_lists();
    let tmp = std::env::temp_dir().join("gqlite_list_roundtrip.gdb");
    let _ = std::fs::remove_file(&tmp);
    g.save(&tmp).unwrap();

    let store = LazyGraphStore::open(&tmp).unwrap();
    let rt = Runtime::new(&store);
    let q = compile_query("MATCH (x: Actor) WHERE x.name = 'Keanu' RETURN x.roles").unwrap();
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => {
            assert_eq!(
                rs,
                vec![vec![Value::List(vec![
                    Value::Str("Neo".into()),
                    Value::Str("Agent".into())
                ])]]
            );
        }
        _ => panic!("expected projected"),
    }
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_list_type_in_descriptor() {
    // `{roles: [str]}` as a type ascription inside a descriptor.
    let g = graph_with_lists();
    let r = run(&g, "MATCH (x: Actor {roles [str]}) RETURN x.name");
    assert_eq!(r.len(), 3);
}

#[test]
fn test_list_type_discriminates_element_types() {
    let json = r#"{
      "nodes": [
        {"id": "n1", "labels": ["T"], "props": {"xs": [1, 2, 3]}},
        {"id": "n2", "labels": ["T"], "props": {"xs": [true, false]}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    // n1 is a list of int; n2 is a list of bool. `is [int]` must pick only n1.
    let r = run(&g, "MATCH (x: T) WHERE x.xs [int] RETURN x.xs");
    assert_eq!(r.len(), 1);
}
