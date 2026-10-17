//! An edge variable named in two edge patterns of one comma-join denotes
//! the same edge in both (ISO §14.4 GR1: the operands' bindings are
//! natural-joined). The LTJ keys its trie on node variables only, so it
//! must reject a tuple whose two triples bound that variable to different
//! edges instead of letting the second binding overwrite the first.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// Alice→Bob→Dani→Alice plus Alice→Carla, all KNOWS.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "alice", "labels": ["Person"], "props": {"name": "Alice"}},
        {"id": "bob", "labels": ["Person"], "props": {"name": "Bob"}},
        {"id": "carla", "labels": ["Person"], "props": {"name": "Carla"}},
        {"id": "dani", "labels": ["Person"], "props": {"name": "Dani"}}
      ],
      "edges": [
        {"id": "k1", "labels": ["KNOWS"], "props": {}, "endpoints": ["alice", "bob"], "directionality": "->"},
        {"id": "k2", "labels": ["KNOWS"], "props": {}, "endpoints": ["bob", "dani"], "directionality": "->"},
        {"id": "k3", "labels": ["KNOWS"], "props": {}, "endpoints": ["dani", "alice"], "directionality": "->"},
        {"id": "k4", "labels": ["KNOWS"], "props": {}, "endpoints": ["alice", "carla"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn rows(q: &str) -> Vec<Vec<Value>> {
    let g = graph();
    let query = compile_query(q).unwrap();
    match Runtime::new(&g).run_query(&query, 0) {
        QueryResult::Projected(mut rs) => {
            rs.sort_by_key(|r| format!("{r:?}"));
            rs
        }
        _ => panic!("expected a projected result for {q:?}"),
    }
}

fn s(x: &str) -> Value {
    Value::Str(x.into())
}

#[test]
fn comma_join_unifies_a_repeated_edge_variable() {
    let got = rows(
        "MATCH (a)-[e:KNOWS]->(b), (x)-[e]->(y) \
         WHERE a.name = 'Alice' RETURN b.name, x.name, y.name",
    );
    // Without the check this was every Alice edge times every edge.
    assert_eq!(
        got,
        vec![
            vec![s("Bob"), s("Alice"), s("Bob")],
            vec![s("Carla"), s("Alice"), s("Carla")],
        ]
    );
}

#[test]
fn chained_match_unifies_a_repeated_edge_variable() {
    let got = rows("MATCH (a)-[e:KNOWS]->(b) MATCH (x)-[e]->(y) RETURN count(*) AS n");
    assert_eq!(got, vec![vec![Value::Int(4)]]);
}

#[test]
fn edge_variable_repeated_inside_one_path_needs_one_edge() {
    // b→c reuses a→b's edge, which only a self-loop could satisfy.
    let got = rows("MATCH (a)-[e:KNOWS]->(b)-[e]->(c) RETURN count(*) AS n");
    assert_eq!(got, vec![vec![Value::Int(0)]]);
}
