//! List comprehension `[<var> IN <source> [WHERE <filter>] | <body>]`
//! (ISO §16.x `<list value constructor by enumeration>` with filter/map).
//! Binds `var` to each element of the source list, keeps elements passing
//! the optional filter, and collects `body` per element into a new list.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["Person"], "props": {"id": 10}},
        {"id": "b", "labels": ["Person"], "props": {"id": 20}},
        {"id": "c", "labels": ["Person"], "props": {"id": 30}}
      ],
      "edges": [
        {"id": "e1", "labels": ["knows"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "e2", "labels": ["knows"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn proj(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let query = compile_query(q).unwrap();
    let rt = Runtime::new(g);
    match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("expected projected rows, got {other:?}"),
    }
}

fn list(vs: Vec<i64>) -> Value {
    Value::List(vs.into_iter().map(Value::Int).collect())
}

#[test]
fn map_over_scalar_list() {
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (p:Person {id: 10}) RETURN [x IN [1, 2, 3] | x + 1] AS r",
    );
    assert_eq!(rows, vec![vec![list(vec![2, 3, 4])]]);
}

#[test]
fn filter_then_map() {
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (p:Person {id: 10}) RETURN [x IN [1, 2, 3, 4] WHERE x > 2 | x] AS r",
    );
    assert_eq!(rows, vec![vec![list(vec![3, 4])]]);
}

#[test]
fn node_attribute_extraction_over_path() {
    // The IC14 shape: project the ids of the nodes along a matched path.
    let g = graph();
    let rows = proj(
        &g,
        "MATCH path = (a:Person {id: 10})-[:knows]->{1,2}(b:Person) \
         RETURN [n IN NODES(path) | n.id] AS ids ORDER BY b.id ASC",
    );
    assert_eq!(
        rows,
        vec![vec![list(vec![10, 20])], vec![list(vec![10, 20, 30])]]
    );
}

#[test]
fn nested_comprehension() {
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (p:Person {id: 10}) RETURN [x IN [1, 2] | [y IN [10, 20] | x + y]] AS r",
    );
    assert_eq!(
        rows,
        vec![vec![Value::List(vec![
            list(vec![11, 21]),
            list(vec![12, 22])
        ])]]
    );
}

#[test]
fn empty_source_yields_empty_list() {
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (p:Person {id: 10}) RETURN [x IN [] | x + 1] AS r",
    );
    assert_eq!(rows, vec![vec![Value::List(vec![])]]);
}

#[test]
fn list_literal_with_in_element_still_parses() {
    // `[1, 2, 3]` (no `|`) must stay a plain list literal, not be mistaken
    // for a comprehension.
    let g = graph();
    let rows = proj(&g, "MATCH (p:Person {id: 10}) RETURN [1, 2, 3] AS r");
    assert_eq!(rows, vec![vec![list(vec![1, 2, 3])]]);
}

#[test]
fn comprehension_typechecks() {
    assert!(compile_query("MATCH (p:Person) RETURN [x IN [1, 2, 3] | x * 2] AS r").is_ok());
    assert!(compile_query("MATCH (p:Person) RETURN [x IN [1, 2, 3] WHERE x > 1 | x] AS r").is_ok());
}
