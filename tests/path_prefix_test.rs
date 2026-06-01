//! ISO/IEC 39075:2024 §16.6 `<path pattern prefix>` — path modes
//! (WALK / TRAIL / SIMPLE / ACYCLIC) and path search prefixes
//! (ALL / ANY / SHORTEST). End-to-end: parser AST shape + runtime
//! selection over bounded repetition.

use gqlrust::compile_query;
use gqlrust::compile_query_unchecked;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::syntax::path_prefix::{PathMode, PathSearch};

// =====================================================================
// Fixtures
// =====================================================================

/// Two routes A → D of different length:
///   A -> B -> D            (length 2)
///   A -> C -> E -> D       (length 3)
/// plus the single edges so every adjacent pair is also reachable.
fn graph_two_routes() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"name": "A"}},
        {"id": "b", "labels": ["N"], "props": {"name": "B"}},
        {"id": "c", "labels": ["N"], "props": {"name": "C"}},
        {"id": "d", "labels": ["N"], "props": {"name": "D"}},
        {"id": "e", "labels": ["N"], "props": {"name": "E"}}
      ],
      "edges": [
        {"id": "ab", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "bd", "labels": ["R"], "props": {}, "endpoints": ["b", "d"], "directionality": "->"},
        {"id": "ac", "labels": ["R"], "props": {}, "endpoints": ["a", "c"], "directionality": "->"},
        {"id": "ce", "labels": ["R"], "props": {}, "endpoints": ["c", "e"], "directionality": "->"},
        {"id": "ed", "labels": ["R"], "props": {}, "endpoints": ["e", "d"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// Directed triangle X -> Y -> Z -> X (a 3-cycle).
fn graph_triangle() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "x", "labels": ["N"], "props": {"name": "X"}},
        {"id": "y", "labels": ["N"], "props": {"name": "Y"}},
        {"id": "z", "labels": ["N"], "props": {"name": "Z"}}
      ],
      "edges": [
        {"id": "xy", "labels": ["R"], "props": {}, "endpoints": ["x", "y"], "directionality": "->"},
        {"id": "yz", "labels": ["R"], "props": {}, "endpoints": ["y", "z"], "directionality": "->"},
        {"id": "zx", "labels": ["R"], "props": {}, "endpoints": ["z", "x"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run_projected(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected for {q:?}"),
    }
}

// =====================================================================
// Parser — AST shape
// =====================================================================

#[test]
fn parses_bare_path_mode() {
    let q = compile_query_unchecked("MATCH ACYCLIC (a)-[]->{1,3}(b) RETURN a").unwrap();
    let prefix = q.matches[0]
        .prefix()
        .expect("ACYCLIC is a non-trivial prefix");
    assert_eq!(prefix.mode, PathMode::Acyclic);
    assert_eq!(prefix.search, PathSearch::All);
}

#[test]
fn walk_all_is_trivial_and_dropped() {
    // The implicit `WALK ALL` constrains nothing, so the parser stores None.
    let q = compile_query_unchecked("MATCH WALK (a)-[]->(b) RETURN a").unwrap();
    assert!(q.matches[0].prefix().is_none());
    let q2 = compile_query_unchecked("MATCH ALL (a)-[]->(b) RETURN a").unwrap();
    assert!(q2.matches[0].prefix().is_none());
}

#[test]
fn parses_counted_shortest_path() {
    let q = compile_query_unchecked("MATCH SHORTEST 2 (a)-[]->{1,3}(b) RETURN a").unwrap();
    let prefix = q.matches[0].prefix().unwrap();
    assert_eq!(prefix.mode, PathMode::Walk);
    assert_eq!(prefix.search, PathSearch::ShortestPaths { count: 2 });
}

#[test]
fn parses_all_shortest_as_one_group() {
    let q = compile_query_unchecked("MATCH ALL SHORTEST (a)-[]->{1,3}(b) RETURN a").unwrap();
    assert_eq!(
        q.matches[0].prefix().unwrap().search,
        PathSearch::ShortestGroups { count: 1 }
    );
}

#[test]
fn parses_any_shortest_as_one_path() {
    let q = compile_query_unchecked("MATCH ANY SHORTEST (a)-[]->{1,3}(b) RETURN a").unwrap();
    assert_eq!(
        q.matches[0].prefix().unwrap().search,
        PathSearch::ShortestPaths { count: 1 }
    );
}

#[test]
fn parses_any_with_count_and_mode() {
    let q = compile_query_unchecked("MATCH ANY 3 TRAIL (a)-[]->{1,3}(b) RETURN a").unwrap();
    let prefix = q.matches[0].prefix().unwrap();
    assert_eq!(prefix.mode, PathMode::Trail);
    assert_eq!(prefix.search, PathSearch::Any { count: 3 });
}

#[test]
fn shortest_without_count_or_groups_is_an_error() {
    let err = compile_query_unchecked("MATCH SHORTEST (a)-[]->{1,3}(b) RETURN a").unwrap_err();
    assert!(err.to_lowercase().contains("shortest"), "got: {err}");
}

#[test]
fn zero_count_is_rejected() {
    let err = compile_query_unchecked("MATCH ANY 0 (a)-[]->(b) RETURN a").unwrap_err();
    assert!(err.contains("positive"), "got: {err}");
}

// =====================================================================
// Runtime — path search selection
// =====================================================================

#[test]
fn all_paths_returns_every_route() {
    // Unprefixed: both the length-2 and length-3 A→D routes appear.
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH (s)-[]->{1,3}(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 2, "ALL keeps both A→D routes");
}

#[test]
fn shortest_one_keeps_only_the_short_route() {
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH SHORTEST 1 (s)-[]->{1,3}(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 1, "SHORTEST 1 keeps only the length-2 route");
}

#[test]
fn all_shortest_keeps_the_shortest_length_group() {
    // Only one route has the minimum length, so the shortest group is a
    // singleton here — same count as SHORTEST 1 but via the GROUP path.
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH ALL SHORTEST (s)-[]->{1,3}(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 1);
}

// =====================================================================
// Runtime — path modes over a cycle
// =====================================================================

#[test]
fn acyclic_removes_the_cycle() {
    // X →→→ X in exactly 3 hops is the triangle cycle; ACYCLIC forbids the
    // repeated node X, so nothing comes back.
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH ACYCLIC (s)-[]->{3,3}(t) WHERE s.name = 'X' AND t.name = 'X' RETURN s.name",
    );
    assert_eq!(rows.len(), 0, "ACYCLIC rejects the X→Y→Z→X cycle");
}

#[test]
fn simple_allows_the_closing_cycle() {
    // SIMPLE permits the single first==last coincidence, so the closed
    // triangle survives.
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH SIMPLE (s)-[]->{3,3}(t) WHERE s.name = 'X' AND t.name = 'X' RETURN s.name",
    );
    assert_eq!(rows.len(), 1, "SIMPLE allows first == last");
}

#[test]
fn walk_keeps_the_cycle_too() {
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH (s)-[]->{3,3}(t) WHERE s.name = 'X' AND t.name = 'X' RETURN s.name",
    );
    assert_eq!(rows.len(), 1, "the default WALK keeps the cycle");
}
