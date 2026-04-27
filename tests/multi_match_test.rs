//! Tests for the structural multi-MATCH refactor (ISO §14.3-14.4).
//!
//! Constructs `Query` values manually with `len(matches) > 1` to exercise
//! the multi-MATCH path without going through the parser. Without this,
//! the central claim of the refactor — that `Query::collapsed_pattern()`
//! is sound for any chain of Simple match statements via `PathPattern::Join`
//! — would be unverified by example tests.

use std::path::Path;

use gqlrust::elaborate::elaborate_query;
use gqlrust::model::graph::Graph;
use gqlrust::parser;
use gqlrust::runtime::engine::Runtime;
use gqlrust::syntax::path_pattern::PathPattern;
use gqlrust::syntax::query::{MatchStatement, Query};
use gqlrust::typing::checker::Typechecker;

fn fraud_graph() -> Graph {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    Graph::from_file(&p).unwrap()
}

fn multi_match_query(patterns: &[&str]) -> Query {
    let matches: Vec<MatchStatement> = patterns
        .iter()
        .map(|s| MatchStatement::Simple {
            pattern: parser::parse(s).expect("parse failed"),
        })
        .collect();
    elaborate_query(Query {
        matches,
        group_by: None,
        returns: None,
        distinct: false,
    })
}

#[test]
fn collapse_single_match_is_identity() {
    let q = multi_match_query(&["(x)"]);
    assert!(matches!(q.collapsed_pattern(), PathPattern::Node(_)));
}

#[test]
fn collapse_two_matches_is_join() {
    let q = multi_match_query(&["(x)", "(y)"]);
    let PathPattern::Join(left, right) = q.collapsed_pattern() else {
        panic!("expected Join");
    };
    assert!(matches!(*left, PathPattern::Node(_)));
    assert!(matches!(*right, PathPattern::Node(_)));
}

/// Left-assoc fold: `Join(Join(p1, p2), p3)` is what `pattern_extract`
/// flattens elsewhere in the runtime.
#[test]
fn collapse_three_matches_is_left_associative_join() {
    let q = multi_match_query(&["(x)", "(y)", "(z)"]);
    let PathPattern::Join(left, right_outer) = q.collapsed_pattern() else {
        panic!("expected outer Join");
    };
    assert!(matches!(*right_outer, PathPattern::Node(_)));
    assert!(matches!(*left, PathPattern::Join(_, _)));
}

#[test]
fn runtime_disjoint_multi_match_matches_comma_join() {
    let g = fraud_graph();
    let runtime = Runtime::new(&g);

    let multi = multi_match_query(&["(x)", "(y)"]);
    let multi_rows = runtime.run(&multi.collapsed_pattern()).rows.len();

    let single = multi_match_query(&["(x), (y)"]);
    let single_rows = runtime.run(&single.collapsed_pattern()).rows.len();

    assert_eq!(multi_rows, single_rows);
    assert_eq!(multi_rows, 25, "fraud.json has 5 nodes; cartesian 5×5 = 25");
}

/// `MATCH (x: Account) MATCH (x)` binds `x` once across both matches.
/// Cartesian would be 4×5=20; natural join is 4.
#[test]
fn runtime_shared_var_multi_match_is_natural_join() {
    let g = fraud_graph();
    let runtime = Runtime::new(&g);
    let q = multi_match_query(&["(x: Account)", "(x)"]);
    let rows = runtime.run(&q.collapsed_pattern()).rows.len();
    assert_eq!(rows, 4);
}

#[test]
fn typecheck_accepts_shared_var_across_matches() {
    let q = multi_match_query(&["(x: Account)", "(x)"]);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    assert!(r.ok, "errors: {:?}", tc.errors);
    assert!(tc.errors.is_empty());
}

/// `optimize_query` is private; round-trip via `compile_query_unchecked`
/// which calls it. After optimization, `matches.len() == 1`.
#[test]
fn optimize_query_collapses_multi_match_to_single_simple() {
    let q = multi_match_query(&["(x)", "(y)", "(z)"]);
    assert_eq!(q.matches.len(), 3);

    let input = format!("{}", q.collapsed_pattern());
    let optimized = gqlrust::compile_query_unchecked(&input).unwrap();

    assert_eq!(optimized.matches.len(), 1);
    assert!(matches!(
        &optimized.matches[0],
        MatchStatement::Simple { .. }
    ));
}
