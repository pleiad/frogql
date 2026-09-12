//! Edge label algebra under the worst-case-optimal join.
//!
//! The index holds one triple per label, and the search binds the
//! predicate position to one label at a time. A pattern naming a single
//! label pins that position; `A|B`, `A&B` and "no label at all" cannot
//! be pinned, so the predicate was left free — and nothing re-checked
//! it. Two failures followed, both silent:
//!
//! - the expression was never enforced, so `-[:A|B]->` matched **every**
//!   edge between the endpoints, and `-[:A&B]->` did too;
//! - an edge carrying two labels was reached through two triples and
//!   counted **twice**, so even the unlabelled `-[r]->` over-counted it.
//!
//! On the movies fixture `-[:ACTED_IN|DIRECTED]->` returned 250 rows
//! where `ACTED_IN` alone is 172 and `DIRECTED` alone is 44 — it was
//! matching `PRODUCED`, `WROTE` and `REVIEWED` as well.
//!
//! The scan / hash-join path checks labels with `DescriptorType::
//! is_subtype` and was always right, so *which* answer a query got
//! depended on which strategy the optimizer picked. That is what the
//! cases below compare: the same query under LTJ and under the fallback
//! must agree, and both must agree with counting by hand.

use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

/// Four parallel edges between the same pair, one of them carrying two
/// labels. The multi-label edge is the whole point: without it the
/// duplicate-per-label bug is invisible.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "n0", "labels": ["N"], "props": {"id": 0}},
        {"id": "n1", "labels": ["N"], "props": {"id": 1}}
      ],
      "edges": [
        {"id": "eA",  "labels": ["A"],      "props": {}, "endpoints": ["n0","n1"], "directionality": "->"},
        {"id": "eB",  "labels": ["B"],      "props": {}, "endpoints": ["n0","n1"], "directionality": "->"},
        {"id": "eC",  "labels": ["C"],      "props": {}, "endpoints": ["n0","n1"], "directionality": "->"},
        {"id": "eAB", "labels": ["A","B"],  "props": {}, "endpoints": ["n0","n1"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn count(q: &str) -> i64 {
    let g = graph();
    let rt = Runtime::new(&g);
    let query = frogql::compile_query(q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rows) => match rows[0][0] {
            Value::Int(n) => n,
            ref other => panic!("expected a count, got {other:?}"),
        },
        other => panic!("expected a projection, got {other:?}"),
    }
}

/// A directed pattern: one row per matching edge.
#[test]
fn directed_label_algebra_counts_edges_not_triples() {
    for (q, want, why) in [
        (
            "MATCH (x:N)-[r:A]->(y:N) RETURN COUNT(*)",
            2,
            "eA and eAB carry A",
        ),
        (
            "MATCH (x:N)-[r:B]->(y:N) RETURN COUNT(*)",
            2,
            "eB and eAB carry B",
        ),
        ("MATCH (x:N)-[r:C]->(y:N) RETURN COUNT(*)", 1, "only eC"),
        (
            "MATCH (x:N)-[r]->(y:N) RETURN COUNT(*)",
            4,
            "four edges — eAB is one edge, not one per label",
        ),
        (
            "MATCH (x:N)-[r:A|B]->(y:N) RETURN COUNT(*)",
            3,
            "eA, eB, eAB — and not eC",
        ),
        (
            "MATCH (x:N)-[r:A&B]->(y:N) RETURN COUNT(*)",
            1,
            "only eAB carries both",
        ),
    ] {
        assert_eq!(count(q), want, "{why}\n  query: {q}");
    }
}

/// The same, without the edge variable — the label check must not depend
/// on whether the edge is projected.
#[test]
fn the_edge_variable_is_not_what_makes_it_work() {
    assert_eq!(count("MATCH (x:N)-[:A|B]->(y:N) RETURN COUNT(*)"), 3);
    assert_eq!(count("MATCH (x:N)-[:A&B]->(y:N) RETURN COUNT(*)"), 1);
    assert_eq!(count("MATCH (x:N)-->(y:N) RETURN COUNT(*)"), 4);
}

/// Any-direction patterns route through the mirrored index, which stores
/// every edge in both senses — so each count doubles, and the label rule
/// has to survive the doubling rather than be confused by it.
#[test]
fn any_direction_label_algebra() {
    assert_eq!(count("MATCH (x:N)-[r]-(y:N) RETURN COUNT(*)"), 8);
    assert_eq!(count("MATCH (x:N)-[r:A|B]-(y:N) RETURN COUNT(*)"), 6);
    assert_eq!(count("MATCH (x:N)-[r:A&B]-(y:N) RETURN COUNT(*)"), 2);
}

/// The differential half, and the one that matters: the answer must not
/// depend on the plan. `FROGQL_DISABLE_ANYDIR_LTJ=1` forces the
/// hash-join fallback for an any-direction pattern, which is the path
/// that was right all along.
#[test]
fn ltj_agrees_with_the_hash_join_fallback() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    for q in [
        "MATCH (x:N)-[r]-(y:N) RETURN COUNT(*)",
        "MATCH (x:N)-[r:A]-(y:N) RETURN COUNT(*)",
        "MATCH (x:N)-[r:A|B]-(y:N) RETURN COUNT(*)",
        "MATCH (x:N)-[r:A&B]-(y:N) RETURN COUNT(*)",
        "MATCH (x:N)-[r:A|C]-(y:N) RETURN COUNT(*)",
    ] {
        std::env::remove_var("FROGQL_DISABLE_ANYDIR_LTJ");
        let ltj = count(q);
        std::env::set_var("FROGQL_DISABLE_ANYDIR_LTJ", "1");
        let fallback = count(q);
        std::env::remove_var("FROGQL_DISABLE_ANYDIR_LTJ");
        assert_eq!(ltj, fallback, "LTJ ≠ hash-join for {q}");
    }
}

/// Both index representations, since the compact one navigates the same
/// tries through a different iterator.
#[test]
fn both_representations_agree() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    for q in [
        "MATCH (x:N)-[r]->(y:N) RETURN COUNT(*)",
        "MATCH (x:N)-[r:A|B]->(y:N) RETURN COUNT(*)",
        "MATCH (x:N)-[r:A&B]->(y:N) RETURN COUNT(*)",
    ] {
        std::env::set_var("FROGQL_LTJ_REPR", "compact");
        let compact = count(q);
        std::env::set_var("FROGQL_LTJ_REPR", "array");
        let array = count(q);
        std::env::remove_var("FROGQL_LTJ_REPR");
        assert_eq!(compact, array, "compact ≠ array for {q}");
    }
}

/// A label expression over a longer chain, so the check is exercised
/// where the predicate is not the only free variable.
#[test]
fn label_algebra_inside_a_chain() {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"id": 0}},
        {"id": "b", "labels": ["N"], "props": {"id": 1}},
        {"id": "c", "labels": ["N"], "props": {"id": 2}}
      ],
      "edges": [
        {"id": "ab1", "labels": ["A","B"], "props": {}, "endpoints": ["a","b"], "directionality": "->"},
        {"id": "ab2", "labels": ["C"],     "props": {}, "endpoints": ["a","b"], "directionality": "->"},
        {"id": "bc1", "labels": ["A"],     "props": {}, "endpoints": ["b","c"], "directionality": "->"}
      ]
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let rt = Runtime::new(&g);
    let q = frogql::compile_query("MATCH (x:N)-[:A|B]->(y:N)-[:A]->(z:N) RETURN COUNT(*)").unwrap();
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rows) => assert_eq!(
            rows[0][0],
            Value::Int(1),
            "only ab1 satisfies A|B, and only bc1 continues it"
        ),
        other => panic!("expected a projection, got {other:?}"),
    }
}
