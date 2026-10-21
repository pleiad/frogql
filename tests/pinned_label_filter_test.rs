//! A pin fixes a variable's node id but says nothing about its labels.
//! The OPTIONAL MATCH bind-pushdown and the correlated EXISTS / VALUE
//! paths pin ids taken from outer rows, so the inner pattern's label
//! constraint on that variable must still be checked.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// Alice (Person) and Carla (Person & Student) each created one post.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["Person"], "props": {"name": "Alice"}},
        {"id": "c", "labels": ["Person", "Student"], "props": {"name": "Carla"}},
        {"id": "p1", "labels": ["Post"], "props": {"content": "Hola"}},
        {"id": "p3", "labels": ["Post"], "props": {"content": "GQL"}}
      ],
      "edges": [
        {"id": "e1", "labels": ["CREATED"], "props": {}, "endpoints": ["a", "p1"], "directionality": "->"},
        {"id": "e3", "labels": ["CREATED"], "props": {}, "endpoints": ["c", "p3"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn rows(q: &str) -> Vec<Vec<Value>> {
    let g = graph();
    let query = compile_query(q).unwrap();
    let mut rs = match Runtime::new(&g).run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected result for {q:?}"),
    };
    rs.sort_by_key(|r| format!("{r:?}"));
    rs
}

fn s(x: &str) -> Value {
    Value::Str(x.into())
}

#[test]
fn optional_pushdown_keeps_label_of_shared_variable() {
    assert_eq!(
        rows("MATCH (p:Person) OPTIONAL MATCH (p:Student)-[:CREATED]->(m:Post) RETURN p.name, m.content"),
        vec![vec![s("Alice"), Value::Null], vec![s("Carla"), s("GQL")]]
    );
}

#[test]
fn correlated_exists_keeps_label_of_shared_variable() {
    assert_eq!(
        rows("MATCH (p:Person) WHERE EXISTS { MATCH (p:Student)-[:CREATED]->(m:Post) } RETURN p.name"),
        vec![vec![s("Carla")]]
    );
    assert_eq!(
        rows("MATCH (p:Person) WHERE NOT EXISTS { MATCH (p:Student)-[:CREATED]->(m:Post) } RETURN p.name"),
        vec![vec![s("Alice")]]
    );
}

#[test]
fn correlated_value_keeps_label_of_shared_variable() {
    assert_eq!(
        rows("MATCH (p:Person) RETURN p.name, VALUE { MATCH (p:Student)-[:CREATED]->(m:Post) RETURN m.content LIMIT 1 } AS post"),
        vec![vec![s("Alice"), Value::Null], vec![s("Carla"), s("GQL")]]
    );
}

#[test]
fn optional_pushdown_without_inner_label_is_unchanged() {
    assert_eq!(
        rows("MATCH (p:Person) OPTIONAL MATCH (p)-[:CREATED]->(m:Post) RETURN p.name, m.content"),
        vec![vec![s("Alice"), s("Hola")], vec![s("Carla"), s("GQL")]]
    );
}
