//! `VALUE { ... }` value query expression (ISO §16.x) tests.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

/// One person `p` who liked two posts at different times. The VALUE
/// subquery must pick the most recent like per (p) via ORDER BY + LIMIT 1.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "p1", "labels": ["Person"], "props": {"name": "Alice"}},
        {"id": "p2", "labels": ["Person"], "props": {"name": "Bob"}},
        {"id": "m1", "labels": ["Post"], "props": {"t": "early"}},
        {"id": "m2", "labels": ["Post"], "props": {"t": "late"}}
      ],
      "edges": [
        {"id": "l1", "labels": ["likes"], "props": {"ts": 100}, "endpoints": ["p1", "m1"], "directionality": "->"},
        {"id": "l2", "labels": ["likes"], "props": {"ts": 200}, "endpoints": ["p1", "m2"], "directionality": "->"},
        {"id": "l3", "labels": ["likes"], "props": {"ts": 50},  "endpoints": ["p2", "m1"], "directionality": "->"}
      ]
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
fn test_correlated_value_subquery_picks_latest() {
    let g = graph();
    // For Alice (p1) the latest like is on m2 (ts 200). Correlated on `p`.
    let rows = run(
        &g,
        "MATCH (p:Person) WHERE p.name = 'Alice' \
         RETURN p.name AS name, VALUE { \
             MATCH (p)-[l:likes]->(m:Post) \
             RETURN RECORD { ts: l.ts, post: m.t } AS latest \
             ORDER BY latest.ts DESC LIMIT 1 \
         } AS latestLike",
    );
    let mut rec = std::collections::BTreeMap::new();
    rec.insert("ts".to_string(), Value::Int(200));
    rec.insert("post".to_string(), Value::Str("late".into()));
    assert_eq!(
        rows,
        vec![vec![Value::Str("Alice".into()), Value::Record(rec)]]
    );
}

#[test]
fn test_value_subquery_empty_yields_null() {
    let g = graph();
    // A person with no likes → the subquery is empty → Null.
    let json = r#"{"nodes":[{"id":"p9","labels":["Person"],"props":{"name":"Zoe"}}],"edges":[]}"#;
    let g2 = MemoryGraphStore::from_json_str(json).unwrap();
    let _ = &g;
    let rows = run(
        &g2,
        "MATCH (p:Person) \
         RETURN p.name AS name, VALUE { \
             MATCH (p)-[l:likes]->(m:Post) \
             RETURN RECORD { ts: l.ts } AS latest \
             ORDER BY latest.ts DESC LIMIT 1 \
         } AS latestLike",
    );
    assert_eq!(rows, vec![vec![Value::Str("Zoe".into()), Value::Null]]);
}

#[test]
fn test_value_subquery_distinct_per_outer_row() {
    let g = graph();
    // Both Alice and Bob: each gets their own latest like (correlation).
    let mut rows = run(
        &g,
        "MATCH (p:Person) \
         RETURN p.name AS name, VALUE { \
             MATCH (p)-[l:likes]->(m:Post) \
             RETURN l.ts AS ts \
             ORDER BY ts DESC LIMIT 1 \
         } AS latestTs ORDER BY name ASC",
    );
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("Alice".into()), Value::Int(200)],
            vec![Value::Str("Bob".into()), Value::Int(50)],
        ]
    );
}
