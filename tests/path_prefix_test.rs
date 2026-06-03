//! ISO/IEC 39075:2024 §16.6 path prefix parser/runtime tests.

use gqlrust::compile_query;
use gqlrust::compile_query_unchecked;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::syntax::path_pattern::PathPattern;
use gqlrust::syntax::path_prefix::{PathMode, PathPrefix, PathSearch};
use gqlrust::syntax::query::Query;

/// First explicit prefix in the first match pattern.
fn first_prefix(q: &Query) -> Option<PathPrefix> {
    fn find(p: &PathPattern) -> Option<PathPrefix> {
        match p {
            PathPattern::Selected { prefix, .. } => Some(*prefix),
            PathPattern::Filter(inner, _)
            | PathPattern::Questioned(inner)
            | PathPattern::Repeat { pattern: inner, .. } => find(inner),
            PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
                find(a).or_else(|| find(b))
            }
            _ => None,
        }
    }
    find(q.matches[0].pattern())
}

// =====================================================================
// Fixtures
// =====================================================================

/// Two A -> D routes with lengths 2 and 3.
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

/// Diamond: two A -> D routes of length 2, one of length 3.
fn graph_diamond() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"name": "A"}},
        {"id": "b", "labels": ["N"], "props": {"name": "B"}},
        {"id": "c", "labels": ["N"], "props": {"name": "C"}},
        {"id": "d", "labels": ["N"], "props": {"name": "D"}},
        {"id": "e", "labels": ["N"], "props": {"name": "E"}},
        {"id": "f", "labels": ["N"], "props": {"name": "F"}}
      ],
      "edges": [
        {"id": "ab", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "bd", "labels": ["R"], "props": {}, "endpoints": ["b", "d"], "directionality": "->"},
        {"id": "ac", "labels": ["R"], "props": {}, "endpoints": ["a", "c"], "directionality": "->"},
        {"id": "cd", "labels": ["R"], "props": {}, "endpoints": ["c", "d"], "directionality": "->"},
        {"id": "ae", "labels": ["R"], "props": {}, "endpoints": ["a", "e"], "directionality": "->"},
        {"id": "ef", "labels": ["R"], "props": {}, "endpoints": ["e", "f"], "directionality": "->"},
        {"id": "fd", "labels": ["R"], "props": {}, "endpoints": ["f", "d"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// Directed 3-cycle.
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
    let prefix = first_prefix(&q).expect("ACYCLIC is a non-trivial prefix");
    assert_eq!(prefix.mode, PathMode::Acyclic);
    assert_eq!(prefix.search, PathSearch::All);
}

#[test]
fn walk_all_is_trivial_and_dropped() {
    let q = compile_query_unchecked("MATCH WALK (a)-[]->(b) RETURN a").unwrap();
    assert!(first_prefix(&q).is_none());
    let q2 = compile_query_unchecked("MATCH ALL (a)-[]->(b) RETURN a").unwrap();
    assert!(first_prefix(&q2).is_none());
}

#[test]
fn parses_counted_shortest_path() {
    let q = compile_query_unchecked("MATCH SHORTEST 2 (a)-[]->{1,3}(b) RETURN a").unwrap();
    let prefix = first_prefix(&q).unwrap();
    assert_eq!(prefix.mode, PathMode::Walk);
    assert_eq!(prefix.search, PathSearch::ShortestPaths { count: 2 });
}

#[test]
fn parses_all_shortest_as_one_group() {
    let q = compile_query_unchecked("MATCH ALL SHORTEST (a)-[]->{1,3}(b) RETURN a").unwrap();
    assert_eq!(
        first_prefix(&q).unwrap().search,
        PathSearch::ShortestGroups { count: 1 }
    );
}

#[test]
fn parses_counted_shortest_group_with_path_noise_word() {
    let q =
        compile_query_unchecked("MATCH SHORTEST 2 PATHS GROUPS (a)-[]->{1,3}(b) RETURN a").unwrap();
    assert_eq!(
        first_prefix(&q).unwrap().search,
        PathSearch::ShortestGroups { count: 2 }
    );
}

#[test]
fn parses_any_shortest_as_one_path() {
    let q = compile_query_unchecked("MATCH ANY SHORTEST (a)-[]->{1,3}(b) RETURN a").unwrap();
    assert_eq!(
        first_prefix(&q).unwrap().search,
        PathSearch::ShortestPaths { count: 1 }
    );
}

#[test]
fn parses_any_with_count_and_mode() {
    let q = compile_query_unchecked("MATCH ANY 3 TRAIL (a)-[]->{1,3}(b) RETURN a").unwrap();
    let prefix = first_prefix(&q).unwrap();
    assert_eq!(prefix.mode, PathMode::Trail);
    assert_eq!(prefix.search, PathSearch::Any { count: 3 });
}

#[test]
fn star_is_not_an_any_path_prefix() {
    let err = compile_query_unchecked("MATCH *(a)-[]->(b) RETURN a").unwrap_err();
    assert!(err.contains("expected path pattern"), "got: {err}");
}

#[test]
fn parses_lowercase_any_as_path_prefix_soft_keyword() {
    let q = compile_query_unchecked("MATCH any shortest (a)-[]->{1,3}(b) RETURN a").unwrap();
    assert_eq!(
        first_prefix(&q).unwrap().search,
        PathSearch::ShortestPaths { count: 1 }
    );
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

#[test]
fn leading_prefix_binds_only_the_first_comma_operand() {
    // Prefixes are scoped per comma operand.
    let q =
        compile_query_unchecked("MATCH SHORTEST 1 (a)-[]->{1,3}(b), (b)-[]->(c) RETURN a").unwrap();
    let PathPattern::Join(left, right) = q.matches[0].pattern() else {
        panic!("expected a comma-join at the top of the clause pattern");
    };
    assert!(
        matches!(**left, PathPattern::Selected { .. }),
        "the first operand carries the SHORTEST prefix"
    );
    assert!(
        !right.has_selected(),
        "the prefix must NOT span the list — the second operand stays bare"
    );
}

#[test]
fn prefix_binds_to_its_own_comma_operand() {
    // Prefixes may also appear on later comma operands.
    let q =
        compile_query_unchecked("MATCH (a)-[]->(b), SHORTEST 1 (b)-[]->{1,3}(c) RETURN a").unwrap();
    let PathPattern::Join(left, right) = q.matches[0].pattern() else {
        panic!("expected a comma-join at the top of the clause pattern");
    };
    assert!(!left.has_selected(), "the first operand is bare");
    assert!(
        matches!(**right, PathPattern::Selected { .. }),
        "the second operand carries the SHORTEST prefix"
    );
}

// Runtime path search.

#[test]
fn all_paths_returns_every_route() {
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
fn raw_runtime_limit_is_honored_after_selected_pattern() {
    let g = graph_two_routes();
    let rt = Runtime::new(&g);
    let q = compile_query("MATCH SHORTEST 2 (s)-[]->{1,3}(t) RETURN s.name").unwrap();
    let ir = rt.run_with_limit(q.matches[0].pattern(), 1);
    assert_eq!(ir.rows.len(), 1);
}

#[test]
fn all_shortest_keeps_the_shortest_length_group() {
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH ALL SHORTEST (s)-[]->{1,3}(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 1);
}

// Runtime path modes.

#[test]
fn acyclic_removes_the_cycle() {
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH ACYCLIC (s)-[]->{3,3}(t) WHERE s.name = 'X' AND t.name = 'X' RETURN s.name",
    );
    assert_eq!(rows.len(), 0, "ACYCLIC rejects the X→Y→Z→X cycle");
}

#[test]
fn simple_allows_the_closing_cycle() {
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

// Unbounded repetition with SHORTEST.

#[test]
fn unbounded_without_prefix_is_rejected() {
    let err = compile_query("MATCH (s)-[]->+(t) RETURN s.name").unwrap_err();
    assert!(err.to_uppercase().contains("SHORTEST"), "got: {err}");

    let err_star = compile_query("MATCH (s)-[]->*(t) RETURN s.name").unwrap_err();
    assert!(
        err_star.to_uppercase().contains("SHORTEST"),
        "got: {err_star}"
    );
}

#[test]
fn lower_bounded_unbounded_is_rejected() {
    let err = compile_query("MATCH ANY SHORTEST (s)-[]->{2,}(t) RETURN s.name").unwrap_err();
    assert!(
        err.to_lowercase().contains("n >= 2") || err.to_lowercase().contains("unbounded"),
        "got: {err}"
    );
}

#[test]
fn any_shortest_plus_finds_the_short_route() {
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH ANY SHORTEST (s)-[]->+(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 1, "ANY SHORTEST over `+` keeps one A→D route");
}

#[test]
fn star_includes_zero_length_self_reach() {
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH ANY SHORTEST (s)-[]->*(t) WHERE s.name = 'A' AND t.name = 'A' RETURN s.name",
    );
    assert_eq!(rows.len(), 1, "`*` lets A reach itself in zero hops");
}

#[test]
fn shortest_plus_terminates_on_a_cycle() {
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH ANY SHORTEST (s)-[]->+(t) WHERE s.name = 'X' AND t.name = 'X' RETURN s.name",
    );
    assert_eq!(rows.len(), 1, "shortest closed walk X→Y→Z→X found, no hang");
}

#[test]
fn shortest_plus_reaches_every_node_once() {
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH ANY SHORTEST (s)-[]->+(t) WHERE s.name = 'X' RETURN t.name",
    );
    assert_eq!(sorted_names(&rows), vec!["X", "Y", "Z"]);
}

// Unbounded repetition with restrictive modes.

#[test]
fn acyclic_plus_enumerates_simple_paths_and_terminates() {
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH ACYCLIC (s)-[]->+(t) WHERE s.name = 'X' RETURN t.name",
    );
    assert_eq!(sorted_names(&rows), vec!["Y", "Z"]);
}

#[test]
fn simple_plus_allows_the_closing_cycle() {
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH SIMPLE (s)-[]->+(t) WHERE s.name = 'X' RETURN t.name",
    );
    assert_eq!(sorted_names(&rows), vec!["X", "Y", "Z"]);
}

#[test]
fn trail_plus_walks_every_edge_once() {
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH TRAIL (s)-[]->+(t) WHERE s.name = 'X' RETURN t.name",
    );
    assert_eq!(sorted_names(&rows), vec!["X", "Y", "Z"]);
}

#[test]
fn acyclic_star_keeps_every_simple_route() {
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH ACYCLIC (s)-[]->*(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 2, "ACYCLIC keeps both simple A→D routes");
}

#[test]
fn lower_bounded_unbounded_is_allowed_under_a_mode() {
    assert!(compile_query("MATCH ACYCLIC (s)-[]->{2,}(t) RETURN s.name").is_ok());
}

#[test]
fn counted_shortest_over_unbounded_works_with_a_mode() {
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH SHORTEST 2 ACYCLIC (s)-[]->+(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn unbounded_without_shortest_or_mode_is_rejected() {
    let err = compile_query("MATCH WALK (s)-[]->*(t) RETURN s.name").unwrap_err();
    assert!(
        err.to_uppercase().contains("ACYCLIC") || err.to_uppercase().contains("SHORTEST"),
        "got: {err}"
    );
}

// k-shortest over unbounded repetition.

#[test]
fn shortest_2_paths_over_unbounded_keeps_two_routes() {
    let g = graph_two_routes();
    let rows = run_projected(
        &g,
        "MATCH SHORTEST 2 (s)-[]->+(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn shortest_2_paths_picks_the_two_shortest_in_the_diamond() {
    let g = graph_diamond();
    let rows = run_projected(
        &g,
        "MATCH SHORTEST 2 (s)-[]->+(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn shortest_3_paths_includes_the_longer_route() {
    let g = graph_diamond();
    let rows = run_projected(
        &g,
        "MATCH SHORTEST 3 (s)-[]->+(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 3);
}

#[test]
fn shortest_2_groups_keeps_every_path_in_the_two_shortest_lengths() {
    let g = graph_diamond();
    let rows = run_projected(
        &g,
        "MATCH SHORTEST 2 GROUPS (s)-[]->+(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 3);
}

#[test]
fn all_shortest_is_one_group_over_unbounded() {
    let g = graph_diamond();
    let rows = run_projected(
        &g,
        "MATCH ALL SHORTEST (s)-[]->+(t) WHERE s.name = 'A' AND t.name = 'D' RETURN s.name",
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn shortest_2_terminates_on_a_cycle_with_repeated_laps() {
    let g = graph_triangle();
    let rows = run_projected(
        &g,
        "MATCH SHORTEST 2 (s)-[]->+(t) WHERE s.name = 'X' AND t.name = 'X' RETURN s.name",
    );
    assert_eq!(rows.len(), 2, "one-lap and two-lap closed walks");
}

// ISO §16.6 SR 5-8: selective patterns may share only boundary variables.

#[test]
fn selective_endpoint_variable_may_join_other_clauses() {
    assert!(
        compile_query("MATCH ANY SHORTEST (s)-[]->{1,3}(t) MATCH (t)-[]->(u) RETURN u.name")
            .is_ok(),
        "joining on a boundary variable must be permitted"
    );
}

#[test]
fn selective_interior_variable_shared_with_another_clause_is_rejected() {
    let err =
        compile_query("MATCH ANY SHORTEST (s)-[]->(m)-[]->(t) MATCH (m)-[]->(x) RETURN x.name")
            .unwrap_err();
    assert!(
        err.contains("§16.6") && err.to_lowercase().contains("interior"),
        "got: {err}"
    );
}

#[test]
fn selective_interior_variable_in_isolation_is_accepted() {
    assert!(compile_query("MATCH ANY SHORTEST (s)-[]->(m)-[]->(t) RETURN s.name, t.name").is_ok());
}

#[test]
fn non_selective_clause_may_share_any_variable() {
    assert!(compile_query("MATCH ALL (s)-[]->(m)-[]->(t) MATCH (m)-[]->(x) RETURN x.name").is_ok());
    assert!(compile_query("MATCH (s)-[]->(m)-[]->(t) MATCH (m)-[]->(x) RETURN x.name").is_ok());
}

#[test]
fn restrictive_mode_only_clause_is_not_selective() {
    assert!(
        compile_query("MATCH ACYCLIC (s)-[]->(m)-[]->(t) MATCH (m)-[]->(x) RETURN x.name").is_ok()
    );
}

// =====================================================================
// Regression: zero-length inner under unbounded repetition
//
// An inner pattern that can match the empty path (e.g. a bare node)
// makes unbounded repetition non-terminating: a SHORTEST-GROUPS / TRAIL
// search keeps appending zero-length laps and never fills its budget.
// The typechecker must reject it as a hard error (not a warning), since
// the runtime's finite-evaluation path assumes each lap adds an edge.
// =====================================================================

#[test]
fn zero_length_inner_under_unbounded_is_rejected() {
    for q in [
        "MATCH ALL SHORTEST (x)* RETURN x",
        "MATCH ALL SHORTEST (x)+ RETURN x",
        "MATCH ANY SHORTEST (x)* RETURN x",
        "MATCH SHORTEST 2 GROUPS (x)* RETURN x",
        "MATCH TRAIL (x)* RETURN x",
        "MATCH SIMPLE (x)+ RETURN x",
        "MATCH ACYCLIC (x){1,} RETURN x",
    ] {
        assert!(
            compile_query(q).is_err(),
            "zero-length inner under unbounded repetition must be rejected: {q}"
        );
    }
}

#[test]
fn edge_inner_under_unbounded_is_accepted() {
    // A genuine edge inner contributes length >= 1 per lap, so unbounded
    // repetition is well-defined and must still compile.
    assert!(compile_query("MATCH ALL SHORTEST (a)-[]->*(b) RETURN a.name").is_ok());
    assert!(compile_query("MATCH TRAIL (a)-[]->+(b) RETURN a.name").is_ok());
}

#[test]
fn bounded_zero_length_inner_still_compiles() {
    // Bounded repetition with an empty-matching inner is degenerate but
    // terminates, so it stays a warning (compiles successfully).
    assert!(compile_query("MATCH (x){1,3} RETURN x").is_ok());
    assert!(compile_query("MATCH (x)? RETURN x").is_ok());
}

// =====================================================================
// Regression: `ANY` as a label wildcard. `ANY` lexes to its own token
// for the §16.6 path-search prefix, but in label position it remains an
// alias for the `*` "any label" wildcard (worked before path prefixes).
// =====================================================================

#[test]
fn any_is_a_label_wildcard() {
    assert!(
        compile_query("MATCH (x:ANY) RETURN x").is_ok(),
        "(x:ANY) must parse as the any-label wildcard"
    );
    // Equivalent to the `*` spelling.
    assert!(compile_query("MATCH (x:*) RETURN x").is_ok());
    // And `ANY` as a search prefix still works in the same query family.
    assert!(compile_query("MATCH ANY SHORTEST (s)-[]->(t) RETURN s.name").is_ok());
}
