//! `COLLECT_LIST` list/multiset aggregate (ISO §20.9), aliases `COLLECT`
//! and `ARRAY_AGG`. Collects each group's values into a `Value::List`,
//! with null elimination, DISTINCT support, and the all-null-record drop
//! for the empty side of an OPTIONAL MATCH. Unblocks LDBC IC1 / IC12.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

/// Alice authored two posts, Bob one, Carol none.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "alice", "labels": ["Person"], "props": {"name": "Alice"}},
        {"id": "bob",   "labels": ["Person"], "props": {"name": "Bob"}},
        {"id": "carol", "labels": ["Person"], "props": {"name": "Carol"}},
        {"id": "p1", "labels": ["Post"], "props": {"title": "A"}},
        {"id": "p2", "labels": ["Post"], "props": {"title": "B"}},
        {"id": "p3", "labels": ["Post"], "props": {"title": "C"}}
      ],
      "edges": [
        {"id": "c1", "labels": ["created"], "props": {}, "endpoints": ["alice", "p1"], "directionality": "->"},
        {"id": "c2", "labels": ["created"], "props": {}, "endpoints": ["alice", "p2"], "directionality": "->"},
        {"id": "c3", "labels": ["created"], "props": {}, "endpoints": ["bob", "p3"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile failed: {e}\nquery: {q}"));
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected rows, got {other:?}"),
    }
}

/// Sorted strings inside a `Value::List` column, for order-independent
/// assertions (within-group order follows scan order, unspecified).
fn sorted_strs(v: &Value) -> Vec<String> {
    let Value::List(items) = v else {
        panic!("expected a list, got {v:?}");
    };
    let mut out: Vec<String> = items
        .iter()
        .map(|it| match it {
            Value::Str(s) => s.clone(),
            other => panic!("expected string element, got {other:?}"),
        })
        .collect();
    out.sort();
    out
}

#[test]
fn test_collect_list_groups_values() {
    let g = graph();
    let mut rows = run(
        &g,
        "MATCH (per:Person)-[:created]->(post:Post) \
         RETURN per.name AS name, COLLECT_LIST(post.title) AS titles \
         GROUP BY per ORDER BY name",
    );
    // Alice then Bob (Carol has no posts so the inner MATCH drops her).
    assert_eq!(rows.len(), 2);
    rows.sort_by_key(|r| match &r[0] {
        Value::Str(s) => s.clone(),
        _ => String::new(),
    });
    assert_eq!(rows[0][0], Value::Str("Alice".into()));
    assert_eq!(sorted_strs(&rows[0][1]), vec!["A", "B"]);
    assert_eq!(rows[1][0], Value::Str("Bob".into()));
    assert_eq!(sorted_strs(&rows[1][1]), vec!["C"]);
}

#[test]
fn test_collect_list_distinct() {
    let g = graph();
    // Two posts both titled "A" → DISTINCT collapses to one.
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["Person"], "props": {"name": "Alice"}},
        {"id": "p1", "labels": ["Post"], "props": {"title": "A"}},
        {"id": "p2", "labels": ["Post"], "props": {"title": "A"}}
      ],
      "edges": [
        {"id": "c1", "labels": ["created"], "props": {}, "endpoints": ["a", "p1"], "directionality": "->"},
        {"id": "c2", "labels": ["created"], "props": {}, "endpoints": ["a", "p2"], "directionality": "->"}
      ]
    }"#;
    let g2 = MemoryGraphStore::from_json_str(json).unwrap();
    let _ = &g;
    let rows = run(
        &g2,
        "MATCH (per:Person)-[:created]->(post:Post) \
         RETURN COLLECT_LIST(DISTINCT post.title) AS titles",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(sorted_strs(&rows[0][0]), vec!["A"]);
}

#[test]
fn test_collect_list_alias_collect() {
    let g = graph();
    // `COLLECT` is an accepted alias of `COLLECT_LIST`.
    let rows = run(
        &g,
        "MATCH (per:Person)-[:created]->(post:Post) WHERE per.name = 'Bob' \
         RETURN COLLECT(post.title) AS titles",
    );
    assert_eq!(sorted_strs(&rows[0][0]), vec!["C"]);
}

#[test]
fn test_collect_list_drops_empty_optional_records() {
    let g = graph();
    // OPTIONAL MATCH fan-out: Carol has no posts, so her RECORD is all-null
    // and must be dropped — she gets an empty list, not `[{title: null}]`.
    let mut rows = run(
        &g,
        "MATCH (per:Person) \
         OPTIONAL MATCH (per)-[:created]->(post:Post) \
         RETURN per.name AS name, COLLECT_LIST(RECORD { title: post.title }) AS posts \
         GROUP BY per ORDER BY name",
    );
    rows.sort_by_key(|r| match &r[0] {
        Value::Str(s) => s.clone(),
        _ => String::new(),
    });
    assert_eq!(rows.len(), 3);
    // Alice: 2 records, Bob: 1, Carol: 0 (the all-null record was dropped).
    let len_of = |v: &Value| match v {
        Value::List(items) => items.len(),
        other => panic!("expected list, got {other:?}"),
    };
    assert_eq!(rows[0][0], Value::Str("Alice".into()));
    assert_eq!(len_of(&rows[0][1]), 2);
    assert_eq!(rows[1][0], Value::Str("Bob".into()));
    assert_eq!(len_of(&rows[1][1]), 1);
    assert_eq!(rows[2][0], Value::Str("Carol".into()));
    assert_eq!(len_of(&rows[2][1]), 0);
}

#[test]
fn test_collect_list_record_contents() {
    let g = graph();
    // The collected records keep their structure.
    let rows = run(
        &g,
        "MATCH (per:Person)-[:created]->(post:Post) WHERE per.name = 'Bob' \
         RETURN COLLECT_LIST(RECORD { title: post.title }) AS posts",
    );
    let Value::List(items) = &rows[0][0] else {
        panic!("expected list");
    };
    assert_eq!(items.len(), 1);
    let mut expected = std::collections::BTreeMap::new();
    expected.insert("title".to_string(), Value::Str("C".into()));
    assert_eq!(items[0], Value::Record(expected));
}
