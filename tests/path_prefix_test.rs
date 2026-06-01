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

/// Sorted first-column string values of a projected result.
fn sorted_names(rows: &[Vec<Value>]) -> Vec<String> {
    let mut names: Vec<String> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            other => panic!("expected Str, got {other:?}"),
        })
        .collect();
    names.sort();
    names
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

// =====================================================================
// Unbounded repetition (`*` / `+`) coupled to SHORTEST via BFS
// =====================================================================

#[test]
fn unbounded_without_prefix_is_rejected() {
    // `+` over a (possibly cyclic) graph has no finite WALK answer set;
    // the typechecker must reject it with an actionable message.
    let err = compile_query("MATCH (s)-[]->+(t) RETURN s.name").unwrap_err();
    assert!(err.to_uppercase().contains("SHORTEST"), "got: {err}");

    let err_star = compile_query("MATCH (s)-[]->*(t) RETURN s.name").unwrap_err();
    assert!(
        err_star.to_uppercase().contains("SHORTEST"),
        "got: {err_star}"
    );
}

#[test]
fn counted_shortest_over_unbounded_is_rejected() {
    // Only single-shortest (ANY/ALL SHORTEST, SHORTEST 1) is supported
    // over unbounded repetition; counted k >= 2 needs k-shortest.
    let err = compile_query("MATCH SHORTEST 2 (s)-[]->+(t) RETURN s.name").unwrap_err();
    assert!(err.to_uppercase().contains("SHORTEST"), "got: {err}");
}

#[test]
fn lower_bounded_unbounded_is_rejected() {
    // `{2,}` with SHORTEST is out of scope for the current BFS.
    let err = compile_query("MATCH ANY SHORTEST (s)-[]->{2,}(t) RETURN s.name").unwrap_err();
    assert!(
        err.to_lowercase().contains("n >= 2") || err.to_lowercase().contains("unbounded"),
        "got: {err}"
    );
}

#[test]
fn any_shortest_plus_finds_the_short_route() {
    // Unbounded `+`: BFS reaches D from A first at length 2 (A→B→D), so
    // ANY SHORTEST returns exactly that one route — the length-3 A→C→E→D
    // walk is longer and pruned.
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH ANY SHORTEST (s)-[]->+(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 1, "ANY SHORTEST over `+` keeps one A→D route");
}

#[test]
fn star_includes_zero_length_self_reach() {
    // `*` admits the length-0 match, so A reaches itself.
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH ANY SHORTEST (s)-[]->*(t) WHERE s.name = 'A' AND t.name = 'A' RETURN s.name",
    );
    assert_eq!(rows.len(), 1, "`*` lets A reach itself in zero hops");
}

#[test]
fn shortest_plus_terminates_on_a_cycle() {
    // The triangle X→Y→Z→X is fully cyclic: under WALK, `+` would loop
    // forever. BFS bounds the depth at |V| = 3, finds the shortest X→X
    // closed walk at length 3, and terminates.
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH ANY SHORTEST (s)-[]->+(t) WHERE s.name = 'X' AND t.name = 'X' RETURN s.name",
    );
    assert_eq!(rows.len(), 1, "shortest closed walk X→Y→Z→X found, no hang");
}

#[test]
fn shortest_plus_reaches_every_node_once() {
    // From X, BFS reaches X, Y and Z (X via the 3-cycle). ANY SHORTEST
    // keeps one path per (first, last) pair, so exactly three targets.
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH ANY SHORTEST (s)-[]->+(t) WHERE s.name = 'X' RETURN t.name",
    );
    assert_eq!(sorted_names(&rows), vec!["X", "Y", "Z"]);
}

// =====================================================================
// Unbounded repetition (`*` / `+` / `{n,}`) under a restrictive mode
// (ACYCLIC / SIMPLE / TRAIL) — finite enumeration, no SHORTEST needed
// =====================================================================

#[test]
fn acyclic_plus_enumerates_simple_paths_and_terminates() {
    // ACYCLIC forbids repeating a node, so on the X→Y→Z→X triangle the
    // closing hop back to X is excluded. From X, `+` yields X→Y and
    // X→Y→Z only — finite, and it must not hang on the cycle.
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH ACYCLIC (s)-[]->+(t) WHERE s.name = 'X' RETURN t.name",
    );
    assert_eq!(sorted_names(&rows), vec!["Y", "Z"]);
}

#[test]
fn simple_plus_allows_the_closing_cycle() {
    // SIMPLE permits first == last, so the closed triangle X→Y→Z→X
    // survives in addition to the open prefixes.
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH SIMPLE (s)-[]->+(t) WHERE s.name = 'X' RETURN t.name",
    );
    assert_eq!(sorted_names(&rows), vec!["X", "Y", "Z"]);
}

#[test]
fn trail_plus_walks_every_edge_once() {
    // TRAIL forbids reusing an edge; the triangle's three edges form one
    // closed trail X→Y→Z→X, so X reaches Y, Z and itself.
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH TRAIL (s)-[]->+(t) WHERE s.name = 'X' RETURN t.name",
    );
    assert_eq!(sorted_names(&rows), vec!["X", "Y", "Z"]);
}

#[test]
fn acyclic_star_keeps_every_simple_route() {
    // Unlike SHORTEST, ACYCLIC `*` keeps *all* simple A→D routes:
    // A→B→D (len 2) and A→C→E→D (len 3).
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH ACYCLIC (s)-[]->*(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 2, "ACYCLIC keeps both simple A→D routes");
}

#[test]
fn lower_bounded_unbounded_is_allowed_under_a_mode() {
    // `{2,}` is infinite under WALK but finite under a restrictive mode,
    // so the typechecker must accept it (it rejected it under SHORTEST).
    assert!(compile_query("MATCH ACYCLIC (s)-[]->{2,}(t) RETURN s.name").is_ok());
}

#[test]
fn counted_shortest_over_unbounded_works_with_a_mode() {
    // SHORTEST 2 alone is rejected over `+`, but combined with ACYCLIC
    // the engine enumerates the finite simple-path set first and then
    // applies the SHORTEST 2 selection. Both A→D routes (len 2 and 3)
    // are the two shortest, so both survive.
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH SHORTEST 2 ACYCLIC (s)-[]->+(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn unbounded_without_shortest_or_mode_is_rejected() {
    // Bare WALK `*` has neither a single-shortest search nor a
    // restrictive mode, so it stays infinite and is rejected.
    let err = compile_query("MATCH WALK (s)-[]->*(t) RETURN s.name").unwrap_err();
    assert!(
        err.to_uppercase().contains("ACYCLIC") || err.to_uppercase().contains("SHORTEST"),
        "got: {err}"
    );
}
