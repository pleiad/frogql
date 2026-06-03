//! RECORD constructor with expression-valued fields (ISO §17.x).

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

fn one_node() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "n1", "labels": ["N"], "props": {"name": "Alice", "age": 30}}
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

fn record(pairs: &[(&str, Value)]) -> Value {
    let mut m = std::collections::BTreeMap::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), v.clone());
    }
    Value::Record(m)
}

#[test]
fn test_record_with_attr_lookups() {
    let g = one_node();
    let rows = run(
        &g,
        "MATCH (x:N) RETURN RECORD { who: x.name, years: x.age } AS r",
    );
    assert_eq!(
        rows,
        vec![vec![record(&[
            ("who", Value::Str("Alice".into())),
            ("years", Value::Int(30)),
        ])]]
    );
}

#[test]
fn test_nested_record() {
    let g = one_node();
    // A record value may nest another record.
    let rows = run(
        &g,
        "MATCH (x:N) RETURN RECORD { inner: RECORD { years: x.age } } AS r",
    );
    assert_eq!(
        rows,
        vec![vec![record(&[(
            "inner",
            record(&[("years", Value::Int(30))]),
        )])]]
    );
}

#[test]
fn test_record_with_arithmetic_and_coalesce() {
    let g = one_node();
    let rows = run(
        &g,
        "MATCH (x:N) RETURN RECORD { next: x.age + 1, nm: COALESCE(x.nick, x.name) } AS r",
    );
    assert_eq!(
        rows,
        vec![vec![record(&[
            ("next", Value::Int(31)),
            ("nm", Value::Str("Alice".into())),
        ])]]
    );
}

#[test]
fn test_constant_record_still_folds_to_value() {
    let g = one_node();
    // A fully-constant record (bare braces) keeps the const fast-path.
    let rows = run(&g, "MATCH (x:N) RETURN { a: 1, b: 2 } AS r");
    assert_eq!(
        rows,
        vec![vec![record(&[("a", Value::Int(1)), ("b", Value::Int(2))])]]
    );
}
