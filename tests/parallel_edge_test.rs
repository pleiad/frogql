//! Regression tests for parallel-edge handling on the LTJ path.
//!
//! Three failure modes used to surface here before commit, all because
//! result reconstruction never propagated the source edge id out of the
//! LTJ trie:
//! 1. `()-[e]->()` returned one row per distinct `(src, label, tgt)`
//!    triple, dropping parallel edges that shared the triple.
//! 2. `(a)-[e:L]->(b)` returned eids re-aliased by `find_edge(src, tgt)`
//!    when the same `(a, b)` pair also carried a different-labelled edge.
//! 3. Parallel edges sharing exactly `(src, label, tgt)` collapsed inside
//!    the trie and lost every eid past the first.
//!
//! Fix 2 added `ResultTuple::triple_eids` and replaced `find_edge` with
//! the eid carried out of the LTJ trie. Fix 3 then made the base case
//! enumerate every entry in the trie's bottom range for triples that
//! bind an edge variable — one row per parallel edge instead of one per
//! distinct triple.

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::typing::label_type::LabelType;

fn build_parallel_graph() -> MemoryGraphStore {
    // Three nodes; the (a, b) pair carries two edges with different
    // labels, and the (b, c) pair carries one edge.
    //
    //     a --[:KNOWS]--> b --[:KNOWS]--> c
    //       \--[:LIKES]-/
    let node_names = vec!["a".into(), "b".into(), "c".into()];
    let node_labels = vec![
        LabelType::Label("Person".into()),
        LabelType::Label("Person".into()),
        LabelType::Label("Person".into()),
    ];
    let node_props = vec![Default::default(), Default::default(), Default::default()];

    let edge_names = vec!["e0".into(), "e1".into(), "e2".into()];
    let edge_labels = vec![
        LabelType::Label("KNOWS".into()),
        LabelType::Label("LIKES".into()),
        LabelType::Label("KNOWS".into()),
    ];
    let edge_props = vec![Default::default(), Default::default(), Default::default()];
    let edge_src = vec![0, 0, 1];
    let edge_tgt = vec![1, 1, 2];
    let edge_directed = vec![true, true, true];

    MemoryGraphStore::from_raw(
        node_names,
        node_labels,
        node_props,
        edge_names,
        edge_labels,
        edge_props,
        edge_src,
        edge_tgt,
        edge_directed,
    )
}

#[test]
fn free_label_edge_var_returns_every_edge() {
    let g = build_parallel_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH ()-[e]->() RETURN count(*)").unwrap();
    let result = r.run_query(&q, 0);
    let QueryResult::Projected(rows) = result else {
        panic!("expected projection")
    };
    assert_eq!(rows[0][0], Value::Int(3));
}

#[test]
fn free_label_edge_var_unique_eids() {
    let g = build_parallel_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH ()-[e]->() RETURN count(DISTINCT e)").unwrap();
    let result = r.run_query(&q, 0);
    let QueryResult::Projected(rows) = result else {
        panic!("expected projection")
    };
    assert_eq!(rows[0][0], Value::Int(3));
}

#[test]
fn free_label_edge_var_lists_each_edge_once() {
    let g = build_parallel_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH ()-[e]->() RETURN e").unwrap();
    let result = r.run_query(&q, 0);
    let QueryResult::Projected(rows) = result else {
        panic!("expected projection")
    };
    let mut ids: Vec<u32> = rows
        .iter()
        .map(|row| match row[0] {
            Value::Edge(id) => id,
            _ => panic!("expected Edge"),
        })
        .collect();
    ids.sort();
    assert_eq!(ids, vec![0, 1, 2]);
}

#[test]
fn labelled_edge_var_resolves_to_correct_eid() {
    // (a)-[:KNOWS]->(b) and (a)-[:LIKES]->(b) share the same (src, tgt).
    // The pre-fix `find_edge` returned the first outgoing edge regardless
    // of label, so :LIKES rows got the :KNOWS edge id. After fix 2 the
    // per-triple eid carried by ResultTuple recovers the right one.
    let g = build_parallel_graph();
    let r = Runtime::new(&g);

    let q = gqlrust::compile_query("MATCH (a)-[e:LIKES]->(b) RETURN e").unwrap();
    let result = r.run_query(&q, 0);
    let QueryResult::Projected(rows) = result else {
        panic!("expected projection")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Edge(1));

    let q = gqlrust::compile_query("MATCH (a)-[e:KNOWS]->(b) RETURN e").unwrap();
    let result = r.run_query(&q, 0);
    let QueryResult::Projected(rows) = result else {
        panic!("expected projection")
    };
    let mut ids: Vec<u32> = rows
        .iter()
        .map(|row| match row[0] {
            Value::Edge(id) => id,
            _ => panic!("expected Edge"),
        })
        .collect();
    ids.sort();
    assert_eq!(ids, vec![0, 2]);
}

fn build_same_label_parallel_graph() -> MemoryGraphStore {
    // Two nodes and three :REFERENCES edges all going a → b. Mirrors the
    // rocq.gdb case where a Lemma references the same Constructor several
    // times: ISO §6.7 wants one row per physical edge, but the LTJ trie
    // collapses entries that share `(src, label, tgt)` so without fix 3
    // only the first eid surfaces.
    let node_names = vec!["a".into(), "b".into()];
    let node_labels = vec![
        LabelType::Label("Defn".into()),
        LabelType::Label("Defn".into()),
    ];
    let node_props = vec![Default::default(); 2];

    let edge_names = vec!["e0".into(), "e1".into(), "e2".into()];
    let edge_labels = vec![
        LabelType::Label("REFERENCES".into()),
        LabelType::Label("REFERENCES".into()),
        LabelType::Label("REFERENCES".into()),
    ];
    let edge_props = vec![Default::default(); 3];
    let edge_src = vec![0, 0, 0];
    let edge_tgt = vec![1, 1, 1];
    let edge_directed = vec![true, true, true];

    MemoryGraphStore::from_raw(
        node_names,
        node_labels,
        node_props,
        edge_names,
        edge_labels,
        edge_props,
        edge_src,
        edge_tgt,
        edge_directed,
    )
}

#[test]
fn same_label_parallels_under_free_label() {
    let g = build_same_label_parallel_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH (a)-[e]->(b) RETURN e").unwrap();
    let result = r.run_query(&q, 0);
    let QueryResult::Projected(rows) = result else {
        panic!("expected projection")
    };
    let mut ids: Vec<u32> = rows
        .iter()
        .map(|row| match row[0] {
            Value::Edge(id) => id,
            _ => panic!("expected Edge"),
        })
        .collect();
    ids.sort();
    assert_eq!(ids, vec![0, 1, 2]);
}

#[test]
fn same_label_parallels_under_constant_label() {
    // The pre-fix-3 LTJ trie collapsed the three (a, REFERENCES, b)
    // entries into one and returned a single eid. Fix 3 fans out one row
    // per physical entry sharing the (src, label, tgt) prefix.
    let g = build_same_label_parallel_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH (a)-[e:REFERENCES]->(b) RETURN e").unwrap();
    let result = r.run_query(&q, 0);
    let QueryResult::Projected(rows) = result else {
        panic!("expected projection")
    };
    let mut ids: Vec<u32> = rows
        .iter()
        .map(|row| match row[0] {
            Value::Edge(id) => id,
            _ => panic!("expected Edge"),
        })
        .collect();
    ids.sort();
    assert_eq!(ids, vec![0, 1, 2]);
}

#[test]
fn multi_hop_with_edge_var_in_middle() {
    // (a)-[e:KNOWS]->(b)-[:KNOWS]->(c). Fix 1 bails out of LTJ on the
    // free-label *any* edge case; here every term has a label so we should
    // stay on the LTJ path AND still recover the correct middle eid.
    let g = build_parallel_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH (a)-[e:KNOWS]->(b)-[:KNOWS]->(c) RETURN e").unwrap();
    let result = r.run_query(&q, 0);
    let QueryResult::Projected(rows) = result else {
        panic!("expected projection")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Edge(0));
}
