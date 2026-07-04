//! Differential suite: compact CLTJ index ≡ array index (issue #66).
//!
//! `GQLITE_LTJ_COMPACT=1` builds the LOUDS succinct-trie representation
//! instead of the six sorted arrays. Both feed the same LTJ algorithm
//! through `LtjIterator`, so every query must produce the same bag of rows.
//! The battery covers the LTJ-eligible shapes (chains, comma-joins,
//! reverse / undirected / any-direction edges, parallel-edge multiplicity,
//! self-loops, constant pins) plus fallback shapes to prove the switch is
//! inert outside LTJ.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use std::path::Path;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Rich multigraph: reciprocal pair, parallel edges, self-loop, undirected
/// edge, second label. ids a=0..e=4.
fn rich() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"id": 0}},
        {"id": "b", "labels": ["N"], "props": {"id": 1}},
        {"id": "c", "labels": ["N"], "props": {"id": 2}},
        {"id": "d", "labels": ["N"], "props": {"id": 3}},
        {"id": "e", "labels": ["N"], "props": {"id": 4}}
      ],
      "edges": [
        {"id": "ab", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "ba", "labels": ["R"], "props": {}, "endpoints": ["b", "a"], "directionality": "->"},
        {"id": "bc", "labels": ["R"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"},
        {"id": "cc", "labels": ["R"], "props": {}, "endpoints": ["c", "c"], "directionality": "->"},
        {"id": "ad1", "labels": ["R"], "props": {}, "endpoints": ["a", "d"], "directionality": "->"},
        {"id": "ad2", "labels": ["R"], "props": {}, "endpoints": ["a", "d"], "directionality": "->"},
        {"id": "de", "labels": ["R"], "props": {}, "endpoints": ["d", "e"], "directionality": "~~"},
        {"id": "bcS", "labels": ["S"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run(g: &MemoryGraphStore, q: &str, compact: bool) -> Vec<String> {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if compact {
        std::env::set_var("GQLITE_LTJ_COMPACT", "1");
    } else {
        std::env::remove_var("GQLITE_LTJ_COMPACT");
    }
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    let out = match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("expected projected, got {other:?}"),
    };
    std::env::remove_var("GQLITE_LTJ_COMPACT");
    let mut keys: Vec<String> = out.iter().map(|r| format!("{r:?}")).collect();
    keys.sort();
    keys
}

fn assert_reprs_agree_on(g: &MemoryGraphStore, q: &str) {
    let array = run(g, q, false);
    let compact = run(g, q, true);
    assert_eq!(
        array, compact,
        "array ≠ compact multiset for {q}\n  array={array:?}\n  compact={compact:?}"
    );
}

fn assert_reprs_agree(q: &str) {
    let g = rich();
    assert_reprs_agree_on(&g, q);
}

#[test]
fn agree_one_hop() {
    assert_reprs_agree("MATCH (x:N)-[:R]->(y:N) RETURN x.id, y.id");
}

#[test]
fn agree_one_hop_unlabeled() {
    assert_reprs_agree("MATCH (x:N)-[]->(y:N) RETURN x.id, y.id");
}

#[test]
fn agree_two_hop_chain() {
    assert_reprs_agree("MATCH (x:N)-[:R]->(y:N)-[:R]->(z:N) RETURN x.id, y.id, z.id");
}

#[test]
fn agree_three_hop_chain() {
    assert_reprs_agree("MATCH (w:N)-[]->(x:N)-[]->(y:N)-[]->(z:N) RETURN w.id, z.id");
}

#[test]
fn agree_reverse_edge() {
    assert_reprs_agree("MATCH (x:N)<-[:R]-(y:N) RETURN x.id, y.id");
}

#[test]
fn agree_undirected_edge() {
    assert_reprs_agree("MATCH (x:N)~[:R]~(y:N) RETURN x.id, y.id");
}

#[test]
fn agree_any_direction() {
    // Mirrored compact index (`from_graph_anydir` under GQLITE_LTJ_COMPACT).
    assert_reprs_agree("MATCH (x:N)-[e]-(y:N) RETURN x.id, y.id");
}

#[test]
fn agree_mixed_direction_chain() {
    assert_reprs_agree("MATCH (x:N)-[:R]->(y:N)-[]-(z:N) RETURN x.id, z.id");
}

#[test]
fn agree_comma_join() {
    assert_reprs_agree("MATCH (x:N)-[]->(y:N), (y:N)-[]->(z:N) RETURN x.id, y.id, z.id");
}

#[test]
fn agree_triangle_join() {
    assert_reprs_agree("MATCH (x)-[]->(y), (y)-[]->(z), (x)-[]->(z) RETURN x.id, y.id, z.id");
}

#[test]
fn agree_parallel_edge_multiplicity() {
    // a→d twice: bag semantics must fan out per eid in both reprs.
    assert_reprs_agree("MATCH (x:N)-[e:R]->(y:N) WHERE y.id = 3 RETURN x.id, y.id");
    assert_reprs_agree("MATCH (x:N)-[:R]->(y:N) WHERE y.id = 3 RETURN x.id, y.id");
}

#[test]
fn agree_self_loop() {
    assert_reprs_agree("MATCH (x:N)-[:R]->(x) RETURN x.id");
}

#[test]
fn agree_value_pin() {
    // Pushed value predicate → in-loop filter / index fold.
    assert_reprs_agree("MATCH (x:N)-[]->(y:N) WHERE x.id = 1 RETURN x.id, y.id");
}

#[test]
fn agree_label_disjunction() {
    assert_reprs_agree("MATCH (x)-[:R|S]->(y) RETURN x.id, y.id");
}

#[test]
fn agree_adjacent_edges() {
    // Adjacent edges synthesize an anonymous boundary variable.
    assert_reprs_agree("MATCH (x:N)-[]->()-[]->(z:N) RETURN x.id, z.id");
}

#[test]
fn agree_distinct_and_count() {
    assert_reprs_agree("MATCH (x:N)-[]->(y:N) RETURN DISTINCT x.id");
    assert_reprs_agree("MATCH (x:N)-[]->(y:N) RETURN COUNT(y) AS c GROUP BY x");
}

#[test]
fn agree_union_and_repetition_fallbacks() {
    // Non-LTJ shapes: the repr switch must be inert.
    assert_reprs_agree("MATCH (x:N)(-[:R]->|<-[:R]-)(y:N) RETURN x.id, y.id");
    assert_reprs_agree("MATCH (x:N)-[:R]->{1,2}(y:N) RETURN x.id, y.id");
}

#[test]
fn agree_on_fraud_fixture() {
    let g = MemoryGraphStore::from_file(Path::new("test_data/fraud.json")).unwrap();
    for q in [
        "MATCH (a)-[:Transfer]->(b) RETURN a, b",
        "MATCH (a)-[:Transfer]->(b)-[:Transfer]->(c) RETURN a, b, c",
        "MATCH (a)-[]->(b), (b)-[]->(c), (a)-[]->(c) RETURN a, b, c",
        "MATCH (a:Account)-[]->(b) RETURN a, b",
    ] {
        assert_reprs_agree_on(&g, q);
    }
}

#[test]
fn agree_on_movies_fixture() {
    let g = MemoryGraphStore::from_file(Path::new("test_data/movies.json")).unwrap();
    for q in [
        "MATCH (p:Person)-[:ACTED_IN]->(m:Movie) RETURN p, m",
        "MATCH (p)-[:ACTED_IN]->(m)<-[:DIRECTED]-(d) RETURN p, m, d",
        "MATCH (a)-[:ACTED_IN]->(m), (b)-[:ACTED_IN]->(m) RETURN a, b, m",
    ] {
        assert_reprs_agree_on(&g, q);
    }
}

#[test]
fn compact_index_is_smaller_on_fixture() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let g = MemoryGraphStore::from_file(Path::new("test_data/movies.json")).unwrap();

    std::env::remove_var("GQLITE_LTJ_COMPACT");
    let array = Runtime::new(&g).warm_triple_index();
    std::env::set_var("GQLITE_LTJ_COMPACT", "1");
    let compact = Runtime::new(&g).warm_triple_index();
    std::env::remove_var("GQLITE_LTJ_COMPACT");

    assert_eq!(array.len(), compact.len(), "triple counts must match");
    assert!(
        compact.heap_bytes() < array.heap_bytes(),
        "compact ({} B) should undercut array ({} B)",
        compact.heap_bytes(),
        array.heap_bytes()
    );
}
