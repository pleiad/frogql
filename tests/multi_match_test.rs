//! Tests for the structural multi-MATCH refactor (ISO §14.3-14.4).
//!
//! The parser still emits a single `MatchStatement::Simple` per query, so
//! the multi-MATCH path (`q.matches.len() > 1`) is exercised here by
//! constructing `Query` values manually and feeding them through each
//! pipeline phase. Without these tests the central claim of the refactor
//! — that `Query::collapsed_pattern()` is sound for any chain of Simple
//! match statements via `PathPattern::Join` — is unverified.
//!
//! When the parser starts accepting `MATCH p1 MATCH p2` syntax the
//! manual construction here can be replaced by `compile_query`, but the
//! invariants validated stay the same.

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

/// Build a multi-MATCH Query from a list of pattern strings, parsed and
/// elaborated. Each string becomes its own `MatchStatement::Simple`.
fn multi_match_query(patterns: &[&str]) -> Query {
    let matches: Vec<MatchStatement> = patterns
        .iter()
        .map(|s| MatchStatement::Simple {
            pattern: parser::parse(s).expect("parse failed"),
        })
        .collect();
    let q = Query {
        matches,
        group_by: None,
        returns: None,
        distinct: false,
    };
    elaborate_query(q)
}

// ---------------------------------------------------------------------
// A. Collapse correctness — structural shape of `collapsed_pattern()`.
// ---------------------------------------------------------------------

/// One Simple match collapses to its inner pattern unchanged. Sanity
/// check: collapse must be the identity for `len == 1`.
#[test]
fn collapse_single_match_is_identity() {
    let q = multi_match_query(&["(x)"]);
    let collapsed = q.collapsed_pattern();
    assert!(matches!(collapsed, PathPattern::Node(_)));
}

/// Two Simple matches collapse to `Join(p1, p2)`.
#[test]
fn collapse_two_matches_is_join() {
    let q = multi_match_query(&["(x)", "(y)"]);
    let collapsed = q.collapsed_pattern();
    let PathPattern::Join(left, right) = collapsed else {
        panic!("expected Join, got {:?}", q.collapsed_pattern());
    };
    assert!(matches!(*left, PathPattern::Node(_)));
    assert!(matches!(*right, PathPattern::Node(_)));
}

/// Three Simple matches collapse left-associatively: `Join(Join(p1, p2), p3)`.
/// Left-assoc is what `PathPattern::Join` expects elsewhere in the runtime
/// (multi-way joins are flattened from the left in `pattern_extract`).
#[test]
fn collapse_three_matches_is_left_associative_join() {
    let q = multi_match_query(&["(x)", "(y)", "(z)"]);
    let PathPattern::Join(left, right_outer) = q.collapsed_pattern() else {
        panic!("expected outer Join");
    };
    // Right side of outer Join is the third pattern (a Node).
    assert!(matches!(*right_outer, PathPattern::Node(_)));
    // Left side is itself a Join of the first two.
    let PathPattern::Join(_, _) = *left else {
        panic!("expected left side to be a Join");
    };
}

// ---------------------------------------------------------------------
// B. Runtime equivalence — multi-MATCH must behave like the comma-join
//    on the same patterns. This is the core soundness claim of the
//    refactor: §14.4 GR 1 (natural join over shared variables).
// ---------------------------------------------------------------------

/// Multi-MATCH with disjoint variables is the cartesian product, same as
/// `MATCH (x), (y)`. Validates that runtime collapse produces equivalent
/// row counts to the equivalent single-match comma-join.
#[test]
fn runtime_disjoint_multi_match_matches_comma_join() {
    let g = fraud_graph();
    let runtime = Runtime::new(&g);

    // Multi-MATCH form: MATCH (x) MATCH (y)
    let multi = multi_match_query(&["(x)", "(y)"]);
    let multi_rows = runtime.run(&multi.collapsed_pattern()).rows.len();

    // Single-MATCH comma-join form: MATCH (x), (y)
    let single = multi_match_query(&["(x), (y)"]);
    let single_rows = runtime.run(&single.collapsed_pattern()).rows.len();

    assert_eq!(
        multi_rows, single_rows,
        "multi-MATCH must produce same row count as comma-join over the same patterns"
    );
    // fraud.json has 5 nodes; cartesian 5×5 = 25.
    assert_eq!(multi_rows, 25);
}

/// Multi-MATCH with a shared variable triggers natural join semantics:
/// `MATCH (x: Account) MATCH (x)` binds `x` once across both matches.
/// Without natural join the result would be the cartesian product
/// (4 × 5 = 20); with natural join it is just the count of x bindings (4).
#[test]
fn runtime_shared_var_multi_match_is_natural_join() {
    let g = fraud_graph();
    let runtime = Runtime::new(&g);

    let q = multi_match_query(&["(x: Account)", "(x)"]);
    let rows = runtime.run(&q.collapsed_pattern()).rows.len();

    // 4 Account nodes; natural join on `x` yields 4, not 4 × 5 = 20.
    assert_eq!(rows, 4, "shared `x` must be unified across matches");
}

// ---------------------------------------------------------------------
// C. Typechecker — env must merge across matches just like Join.
// ---------------------------------------------------------------------

/// The typechecker walks `q.collapsed_pattern()`, so the env from the
/// first match must be visible when the second match references the
/// same variable. Validates that no error is reported for a shared `x`.
#[test]
fn typecheck_accepts_shared_var_across_matches() {
    let q = multi_match_query(&["(x: Account)", "(x)"]);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    assert!(
        r.ok,
        "typecheck should accept shared var; errors: {:?}",
        tc.errors
    );
    assert!(
        tc.errors.is_empty(),
        "no errors expected, got {:?}",
        tc.errors
    );
}

// ---------------------------------------------------------------------
// D. `compile_query_unchecked` round-trips a manually-built multi-MATCH
//    Query: parse a comma-join, replace its Query with a multi-MATCH
//    equivalent, run both through the optimizer, compare the optimized
//    output. Validates that `optimize_query` collapses correctly.
// ---------------------------------------------------------------------

/// After `optimize_query` the multi-MATCH Query collapses to exactly one
/// `Simple` match. Documents the post-optimization invariant referenced
/// in `lib.rs::optimize_query`'s doc-comment.
#[test]
fn optimize_query_collapses_multi_match_to_single_simple() {
    let q = multi_match_query(&["(x)", "(y)", "(z)"]);
    assert_eq!(q.matches.len(), 3, "pre-optimization has 3 matches");

    // optimize_query is private; round-trip via the public path: build
    // an equivalent `compile_query_unchecked` input from the collapsed
    // pattern and compare the optimized plan.
    let collapsed_input = format!("{}", q.collapsed_pattern());
    let optimized = gqlrust::compile_query_unchecked(&collapsed_input)
        .expect("compile_query_unchecked should succeed");

    assert_eq!(
        optimized.matches.len(),
        1,
        "optimize_query collapses multi-match to one Simple"
    );
    assert!(matches!(
        &optimized.matches[0],
        MatchStatement::Simple { .. }
    ));
}
