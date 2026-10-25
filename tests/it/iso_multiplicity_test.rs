//! ISO edge-pattern bag multiplicity (team decision, issue #71).
//!
//! GQL binding tables are bags: every distinct edge match is a row, even
//! when the edge variable is not projected. So parallel edges sharing the
//! same `(src, label, tgt)` each contribute a row. The LTJ base case used
//! to collapse those siblings when no edge variable was bound (a set-on-
//! vertices optimization); this suite pins the ISO-correct counts by hand
//! (NOT against the scan fallback, which had its own gaps) and is the
//! oracle for the fan-out fix.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// ids: a=0, b=1, c=2, d=3.
/// Edges (label R): a→b, a→b (parallel pair), b→c, c→c (self-loop).
/// Edge (label S): a→b (distinct label, single).
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"id": 0}},
        {"id": "b", "labels": ["N"], "props": {"id": 1}},
        {"id": "c", "labels": ["N"], "props": {"id": 2}},
        {"id": "d", "labels": ["N"], "props": {"id": 3}}
      ],
      "edges": [
        {"id": "ab1", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "ab2", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "bc", "labels": ["R"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"},
        {"id": "cc", "labels": ["R"], "props": {}, "endpoints": ["c", "c"], "directionality": "->"},
        {"id": "abS", "labels": ["S"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn proj(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("expected projected, got {other:?}"),
    }
}

fn sorted_ints(rows: &[Vec<Value>], col: usize) -> Vec<i64> {
    let mut v: Vec<i64> = rows
        .iter()
        .map(|r| match &r[col] {
            Value::Int(n) => *n,
            other => panic!("expected Int, got {other:?}"),
        })
        .collect();
    v.sort();
    v
}

#[test]
fn parallel_edges_unbound_var_are_distinct_rows() {
    // (a)-[:R]->(b) has TWO parallel edges → two rows, even though the edge
    // is not bound. ISO bag semantics; the collapse would give one.
    let g = graph();
    let rows = proj(&g, "MATCH (x:N)-[:R]->(y:N) WHERE x.id = 0 RETURN y.id");
    assert_eq!(sorted_ints(&rows, 0), vec![1, 1], "parallel R edges a→b");
}

#[test]
fn parallel_edges_bound_var_unchanged() {
    // Binding the edge already fanned out; behavior must be unchanged.
    let g = graph();
    let rows = proj(&g, "MATCH (x:N)-[r:R]->(y:N) WHERE x.id = 0 RETURN y.id");
    assert_eq!(sorted_ints(&rows, 0), vec![1, 1]);
}

#[test]
fn distinct_labels_each_count_once() {
    // (a)-[]->(b): two R edges + one S edge, all a→b, edge unbound → 3 rows.
    let g = graph();
    let rows = proj(&g, "MATCH (x:N)-[]->(y:N) WHERE x.id = 0 RETURN y.id");
    assert_eq!(sorted_ints(&rows, 0), vec![1, 1, 1]);
}

#[test]
fn two_hop_multiplies_parallel_edges() {
    // (a)-[:R]->(b)-[:R]->(c): two ways to reach b × one b→c edge → 2 rows.
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (x:N)-[:R]->(y:N)-[:R]->(z:N) WHERE x.id = 0 RETURN z.id",
    );
    assert_eq!(sorted_ints(&rows, 0), vec![2, 2]);
}

#[test]
fn no_parallel_edges_single_row() {
    // Control: b→c is a single edge → exactly one row, no change.
    let g = graph();
    let rows = proj(&g, "MATCH (x:N)-[:R]->(y:N) WHERE x.id = 1 RETURN y.id");
    assert_eq!(sorted_ints(&rows, 0), vec![2]);
}

#[test]
fn distinct_dedups_parallel_edges() {
    // RETURN DISTINCT collapses the parallel-edge rows back to one — the
    // user-facing way to get set semantics.
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (x:N)-[:R]->(y:N) WHERE x.id = 0 RETURN DISTINCT y.id",
    );
    assert_eq!(sorted_ints(&rows, 0), vec![1]);
}
