//! Phase 2: Record values — JSON loading of nested objects, nested field
//! access with chained dots, record literals and types in expressions,
//! `.gdb` storage roundtrip.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::store::lazy::LazyGraphStore;

fn graph_with_records() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {
            "name": "Alice",
            "address": {"street": "Main", "city": "Boston", "zip": 2108}
        }},
        {"id": "u2", "labels": ["User"], "props": {
            "name": "Bob",
            "address": {"street": "Oak", "city": "Boston", "zip": 2110}
        }},
        {"id": "u3", "labels": ["User"], "props": {
            "name": "Carol",
            "address": {"street": "Pine", "city": "Seattle", "zip": 98101}
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
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_json_loads_nested_record() {
    let g = graph_with_records();
    let r = run(
        &g,
        "MATCH (x: User) WHERE x.name = 'Alice' RETURN x.address",
    );
    let mut expected = std::collections::BTreeMap::new();
    expected.insert("city".into(), Value::Str("Boston".into()));
    expected.insert("street".into(), Value::Str("Main".into()));
    expected.insert("zip".into(), Value::Int(2108));
    assert_eq!(r, vec![vec![Value::Record(expected)]]);
}

#[test]
fn test_nested_field_access() {
    let g = graph_with_records();
    // Two chained dots: first resolves variable→property, second indexes the record.
    let r = run(
        &g,
        "MATCH (x: User) WHERE x.address.city = 'Seattle' RETURN x.name",
    );
    assert_eq!(r, vec![vec![Value::Str("Carol".into())]]);
}

#[test]
fn test_nested_field_in_return() {
    let g = graph_with_records();
    let r = run(
        &g,
        "MATCH (x: User) WHERE x.name = 'Alice' RETURN x.address.city",
    );
    assert_eq!(r, vec![vec![Value::Str("Boston".into())]]);
}

#[test]
fn test_record_equality() {
    let g = graph_with_records();
    let r = run(
        &g,
        "MATCH (x: User) WHERE x.address = {street: 'Main', city: 'Boston', zip: 2108} RETURN x.name",
    );
    assert_eq!(r, vec![vec![Value::Str("Alice".into())]]);
}

#[test]
fn test_record_type_predicate() {
    let g = graph_with_records();
    let r = run(
        &g,
        "MATCH (x: User) WHERE x.address {street str, city str, zip int} RETURN x.name",
    );
    assert_eq!(r.len(), 3);
}

#[test]
fn test_record_type_rejects_wrong_shape() {
    let g = graph_with_records();
    // address has 3 fields; this type has 2 — must reject all.
    let r = run(
        &g,
        "MATCH (x: User) WHERE x.address {street str, city str} RETURN x.name",
    );
    assert_eq!(r.len(), 0);
}

#[test]
fn test_record_type_in_descriptor() {
    let g = graph_with_records();
    // Use the new `is` form inside a descriptor for a record-typed field.
    let r = run(
        &g,
        "MATCH (x: User {address {street str, city str, zip int}}) RETURN x.name",
    );
    assert_eq!(r.len(), 3);
}

#[test]
fn test_record_storage_roundtrip() {
    let g = graph_with_records();
    let tmp = std::env::temp_dir().join("gqlite_record_roundtrip.gdb");
    let _ = std::fs::remove_file(&tmp);
    g.save(&tmp).unwrap();

    let store = LazyGraphStore::open(&tmp).unwrap();
    let rt = Runtime::new(&store);
    let q = compile_query("MATCH (x: User) WHERE x.address.city = 'Seattle' RETURN x.address.zip")
        .unwrap();
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => assert_eq!(rs, vec![vec![Value::Int(98101)]]),
        _ => panic!("expected projected"),
    }
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_record_literal_in_return() {
    let g = graph_with_records();
    let r = run(
        &g,
        "MATCH (x: User) WHERE x.name = 'Alice' RETURN {name: 'ok', count: 1}",
    );
    let mut expected = std::collections::BTreeMap::new();
    expected.insert("count".into(), Value::Int(1));
    expected.insert("name".into(), Value::Str("ok".into()));
    assert_eq!(r, vec![vec![Value::Record(expected)]]);
}

#[test]
fn test_deep_nested_record() {
    let json = r#"{
      "nodes": [
        {"id": "n1", "labels": ["T"], "props": {
            "meta": {"inner": {"leaf": 42}}
        }}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let r = run(&g, "MATCH (x: T) RETURN x.meta.inner.leaf");
    assert_eq!(r, vec![vec![Value::Int(42)]]);
}

#[test]
fn test_list_of_records() {
    let json = r#"{
      "nodes": [
        {"id": "n1", "labels": ["T"], "props": {
            "people": [
              {"name": "A", "age": 30},
              {"name": "B", "age": 25}
            ]
        }}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    // List of records: each element is a Record.
    let r = run(
        &g,
        "MATCH (x: T) WHERE x.people [{name str, age int}] RETURN x.people",
    );
    assert_eq!(r.len(), 1);
}
