//! Label negation over elements that carry more than one label.
//!
//! An element's labels are a set: `Person & Student` is one node with both.
//! `!Student` must reject it, and `Person & !Student` must too. The label
//! check (`LabelType::is_subtype`, shared by the scan and by LTJ) used to
//! split the stored conjunction before looking at the negation, asking
//! whether *some* label avoided `Student` — and `Person` does — so the node
//! got through.
//!
//! The single-node queries take the scan path; the ones with an edge take
//! LTJ. Both must agree with the set semantics.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// alice: Person; carla: Person & Student; hola: Post.
/// alice and carla both created hola, so a pattern with an edge reaches both.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "alice", "labels": ["Person"], "props": {"name": "Alice"}},
        {"id": "carla", "labels": ["Person", "Student"], "props": {"name": "Carla"}},
        {"id": "hola", "labels": ["Post"], "props": {"name": "Hola"}}
      ],
      "edges": [
        {"id": "e1", "labels": ["CREATED"], "props": {}, "endpoints": ["alice", "hola"], "directionality": "->"},
        {"id": "e2", "labels": ["CREATED"], "props": {}, "endpoints": ["carla", "hola"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn names(q: &str) -> Vec<String> {
    let g = graph();
    let rt = Runtime::new(&g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(rows) => rows,
        other => panic!("expected a projection, got {other:?}"),
    };
    let mut out: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
    out.sort();
    out
}

fn assert_names(q: &str, expected: &[&str]) {
    let got = names(q);
    assert_eq!(got.len(), expected.len(), "row count for {q}: {got:?}");
    for name in expected {
        assert!(
            got.iter().any(|r| r.contains(name)),
            "{name} missing from {q}: {got:?}"
        );
    }
    for name in ["Alice", "Carla", "Hola"] {
        if !expected.contains(&name) {
            assert!(
                !got.iter().any(|r| r.contains(name)),
                "{name} must not match {q}: {got:?}"
            );
        }
    }
}

#[test]
fn negated_label_rejects_a_node_that_has_it_among_others_scan() {
    assert_names("MATCH (x:!Student) RETURN x.name", &["Alice", "Hola"]);
}

#[test]
fn conjunction_with_negation_rejects_a_multi_label_node_scan() {
    assert_names("MATCH (x:Person&!Student) RETURN x.name", &["Alice"]);
}

#[test]
fn positive_labels_still_match_a_multi_label_node_scan() {
    assert_names("MATCH (x:Person) RETURN x.name", &["Alice", "Carla"]);
    assert_names("MATCH (x:Person&Student) RETURN x.name", &["Carla"]);
}

#[test]
fn negated_label_rejects_a_multi_label_node_ltj() {
    assert_names(
        "MATCH (x:!Student)-[:CREATED]->(m:Post) RETURN x.name",
        &["Alice"],
    );
    assert_names(
        "MATCH (x:Person&!Student)-[:CREATED]->(m:Post) RETURN x.name",
        &["Alice"],
    );
}

/// Negation and alternation on single-label nodes, under LTJ. The scan got
/// these right; LTJ kept only the labels every match must carry, so a
/// negated or alternative label filtered nothing.
fn creators() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "alice", "labels": ["Person"], "props": {"name": "Alice"}},
        {"id": "robo", "labels": ["Bot"], "props": {"name": "Robo"}},
        {"id": "hola", "labels": ["Post"], "props": {"name": "Hola"}}
      ],
      "edges": [
        {"id": "e1", "labels": ["CREATED"], "props": {}, "endpoints": ["alice", "hola"], "directionality": "->"},
        {"id": "e2", "labels": ["CREATED"], "props": {}, "endpoints": ["robo", "hola"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn creator_names(q: &str) -> Vec<String> {
    let g = creators();
    let rt = Runtime::new(&g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(rows) => rows,
        other => panic!("expected a projection, got {other:?}"),
    };
    let mut out: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
    out.sort();
    out
}

#[test]
fn negated_label_filters_under_ltj() {
    let got = creator_names("MATCH (x:!Person)-[:CREATED]->(m:Post) RETURN x.name");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("Robo"), "{got:?}");
}

#[test]
fn label_alternative_filters_under_ltj() {
    let got = creator_names("MATCH (x:Person|Tag)-[:CREATED]->(m:Post) RETURN x.name");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("Alice"), "{got:?}");
    let both = creator_names("MATCH (x:Person|Bot)-[:CREATED]->(m:Post) RETURN x.name");
    assert_eq!(both.len(), 2, "{both:?}");
}
