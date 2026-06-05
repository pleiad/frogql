//! Regression: LTJ path reconstruction must orient reverse (`<-`) and
//! undirected (`~`) edges correctly.
//!
//! `convert_results` used to push every triple's stored `tgt` as the next
//! path node. `decompose_flat_chain` stores a reverse edge as
//! `(src = next node, tgt = current boundary)`, so for any reverse edge that
//! is not the first triple in the chain this re-emitted the boundary node and
//! dropped the real endpoint — producing a path with a duplicated node id.
//! The bindings stayed correct (they come from the LTJ tuple by var id), so
//! attribute projection looked fine, but the malformed path silently broke
//! every path-aware feature: ACYCLIC/SIMPLE/TRAIL mode filters, SHORTEST
//! lengths, and the §20.16 named-path functions.
//!
//! Surfaced via LDBC IC12, whose `(person)~[:knows]~(friend)<-[:hasCreator]-...`
//! chain under an `ACYCLIC` prefix was rejected for a phantom repeated node
//! and returned zero rows.

use gqlrust::compile_query_unchecked;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::PathValue;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

/// X -r-> Y <-s- Z : a forward edge followed by a reverse edge.
fn g_fwd_rev() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "X", "labels": ["N"], "props": {"id": 1}},
        {"id": "Y", "labels": ["N"], "props": {"id": 2}},
        {"id": "Z", "labels": ["N"], "props": {"id": 3}}
      ],
      "edges": [
        {"id": "xy", "labels": ["r"], "props": {}, "endpoints": ["X", "Y"], "directionality": "->"},
        {"id": "zy", "labels": ["s"], "props": {}, "endpoints": ["Z", "Y"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// A ~knows~ B <-hasCreator- C : the IC12 shape (undirected then reverse).
fn g_undir_rev() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "A", "labels": ["Person"], "props": {"id": 1}},
        {"id": "B", "labels": ["Person"], "props": {"id": 2}},
        {"id": "C", "labels": ["Comment"], "props": {"id": 3}}
      ],
      "edges": [
        {"id": "ab", "labels": ["knows"], "props": {}, "endpoints": ["A", "B"], "directionality": "~~"},
        {"id": "cb", "labels": ["hasCreator"], "props": {}, "endpoints": ["C", "B"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// The distinct node ids visited by the (single) path of the (single) row.
fn path_node_ids(rt: &Runtime<MemoryGraphStore>, query: &str) -> Vec<u32> {
    let q = compile_query_unchecked(query).unwrap();
    let res = rt.run_query(&q, 0);
    let QueryResult::Raw(ir) = res else {
        panic!("expected raw pattern result");
    };
    assert_eq!(ir.rows.len(), 1, "expected exactly one matching row");
    ir.rows[0]
        .path()
        .0
        .iter()
        .filter_map(|pv| match pv {
            PathValue::Node(n) => Some(*n),
            _ => None,
        })
        .collect()
}

#[test]
fn forward_then_reverse_path_has_no_duplicate_node() {
    let store = g_fwd_rev();
    let rt = Runtime::new(&store);
    // X(0) -> Y(1) <- Z(2): three distinct nodes, no repeat.
    let ids = path_node_ids(&rt, "MATCH (x:N)-[:r]->(y:N)<-[:s]-(z:N)");
    assert_eq!(ids.len(), 3, "path should visit 3 nodes, got {ids:?}");
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 3, "path must not duplicate a node: {ids:?}");
}

#[test]
fn undirected_then_reverse_path_has_no_duplicate_node() {
    let store = g_undir_rev();
    let rt = Runtime::new(&store);
    let ids = path_node_ids(
        &rt,
        "MATCH (a:Person)~[:knows]~(b:Person)<-[:hasCreator]-(c:Comment)",
    );
    assert_eq!(ids.len(), 3, "path should visit 3 nodes, got {ids:?}");
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 3, "path must not duplicate a node: {ids:?}");
}

#[test]
fn acyclic_accepts_chain_with_reverse_edge() {
    // The IC12 failure mode: ACYCLIC over a chain containing a reverse edge.
    // Before the fix the malformed path carried a phantom repeated node and
    // ACYCLIC dropped the row, yielding zero results.
    let store = g_undir_rev();
    let rt = Runtime::new(&store);
    let q = compile_query_unchecked(
        "MATCH ACYCLIC (a:Person)~[:knows]~(b:Person)<-[:hasCreator]-(c:Comment) RETURN c.id AS cid",
    )
    .unwrap();
    let res = rt.run_query(&q, 0);
    let QueryResult::Projected(rows) = res else {
        panic!("expected projected result");
    };
    assert_eq!(
        rows.len(),
        1,
        "ACYCLIC must accept the genuinely acyclic chain"
    );
}
