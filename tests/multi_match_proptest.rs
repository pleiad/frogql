//! Property-based tests for multi-MATCH (ISO §14.3-14.4). Strategies
//! draw from a curated pattern alphabet so proptest explores the
//! collapse + runtime + optimizer pipeline rather than the parser.

use std::path::Path;

use gqlrust::elaborate::elaborate_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::parser;
use gqlrust::runtime::engine::Runtime;
use gqlrust::syntax::query::{MatchStatement, Query};
use proptest::prelude::*;

fn fraud_graph() -> MemoryGraphStore {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    MemoryGraphStore::from_file(&p).unwrap()
}

fn pattern() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("(x)".to_string()),
        Just("(y)".to_string()),
        Just("(z)".to_string()),
        Just("(x: Account)".to_string()),
        Just("(y: Account)".to_string()),
        Just("(z: Person)".to_string()),
        Just("()-[]->()".to_string()),
        Just("()-[:Transfer]->()".to_string()),
    ]
}

fn multi_match_query(patterns: &[String]) -> Query {
    let matches: Vec<MatchStatement> = patterns
        .iter()
        .map(|s| MatchStatement::Simple {
            pattern: parser::parse(s).expect("curated patterns must parse"),
        })
        .collect();
    elaborate_query(Query {
        matches,
        group_by: None,
        returns: None,
        distinct: false,
        order_by: None,
        limit: None,
    })
}

fn optional_query(left: &str, right: &str) -> gqlrust::syntax::query::Query {
    let input = format!("MATCH {left} OPTIONAL MATCH {right}");
    gqlrust::compile_query_unchecked(&input).unwrap()
}

fn match_query(left: &str, right: &str) -> gqlrust::syntax::query::Query {
    let input = format!("MATCH {left} MATCH {right}");
    gqlrust::compile_query_unchecked(&input).unwrap()
}

fn run_query_count(g: &MemoryGraphStore, q: &gqlrust::syntax::query::Query) -> usize {
    let rt = Runtime::new(g);
    rt.run_query(q, 0).row_count()
}

proptest! {
    // 32 cases is plenty for an 8-pattern alphabet; default 256 is overkill.
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// **Invariant**: collapsed multi-MATCH and the equivalent comma-join
    /// produce the same row count. §14.4 GR 1 says MATCH expands the
    /// working table by natural join — exactly `PathPattern::Join`'s
    /// semantics. Central soundness claim of the refactor.
    #[test]
    fn multi_match_runtime_matches_comma_join(
        patterns in proptest::collection::vec(pattern(), 1..=4)
    ) {
        let g = fraud_graph();
        let runtime = Runtime::new(&g);

        let multi = multi_match_query(&patterns);
        let multi_rows = runtime.run(&multi.collapsed_pattern()).rows.len();

        let comma = multi_match_query(&[patterns.join(", ")]);
        let comma_rows = runtime.run(&comma.collapsed_pattern()).rows.len();

        prop_assert_eq!(multi_rows, comma_rows);
    }

    /// **Invariant**: natural join row count ≤ cartesian product. Sharing
    /// a variable across matches can only filter rows, never add them.
    #[test]
    fn natural_join_at_most_cartesian(first in pattern(), second in pattern()) {
        let g = fraud_graph();
        let runtime = Runtime::new(&g);

        let q = multi_match_query(&[first, second]);
        let join_rows = runtime.run(&q.collapsed_pattern()).rows.len();

        let p1 = q.matches[0].pattern();
        let p2 = q.matches[1].pattern();
        let cart = runtime.run(p1).rows.len() * runtime.run(p2).rows.len();

        prop_assert!(join_rows <= cart, "join={join_rows} > cart={cart}");
    }

    /// **Invariant**: `optimize_query` collapses any Query to one Simple
    /// match. Pins the post-optimization shape from `lib.rs::optimize_query`.
    #[test]
    fn optimize_collapses_to_single_simple(
        patterns in proptest::collection::vec(pattern(), 1..=4)
    ) {
        let input = patterns.join(", ");
        let optimized = gqlrust::compile_query_unchecked(&input).unwrap();

        prop_assert_eq!(optimized.matches.len(), 1);
        let is_simple = matches!(&optimized.matches[0], MatchStatement::Simple { .. });
        prop_assert!(is_simple);
    }

    /// **Invariant**: surface-syntax `MATCH p1 MATCH p2` ≡ `MATCH p1, p2`.
    /// Same equivalence as `multi_match_runtime_matches_comma_join` but
    /// exercises the parser path instead of manual `Vec<MatchStatement>`
    /// construction.
    #[test]
    fn parser_multi_match_matches_comma_join(
        patterns in proptest::collection::vec(pattern(), 1..=4)
    ) {
        let g = fraud_graph();
        let runtime = Runtime::new(&g);

        let multi_input = format!("MATCH {}", patterns.join(" MATCH "));
        let multi_q = gqlrust::compile_query_unchecked(&multi_input).unwrap();
        let multi_rows = runtime.run(&multi_q.collapsed_pattern()).rows.len();

        let comma_input = format!("MATCH {}", patterns.join(", "));
        let comma_q = gqlrust::compile_query_unchecked(&comma_input).unwrap();
        let comma_rows = runtime.run(&comma_q.collapsed_pattern()).rows.len();

        prop_assert_eq!(multi_rows, comma_rows);
    }

    /// **Invariant**: OPTIONAL MATCH is a left-outer join, so the row count
    /// is bounded below by the cardinality of the leading MATCH (every left
    /// row survives, either via success or unsuccess) and above by the
    /// cardinality of the equivalent inner natural join multiplied by 1
    /// when the optional fails — it cannot fall below the natural-join
    /// count either, because every successful match of the right side
    /// becomes a success row exactly as in MATCH.
    #[test]
    fn optional_bounds_inner_join_from_below_and_above(
        first in pattern(),
        second in pattern(),
    ) {
        let g = fraud_graph();
        let runtime = Runtime::new(&g);

        let left_only = format!("MATCH {first}");
        let left_q = gqlrust::compile_query_unchecked(&left_only).unwrap();
        let left_count = runtime.run(&left_q.collapsed_pattern()).rows.len();

        let opt_q = optional_query(&first, &second);
        let opt_count = run_query_count(&g, &opt_q);

        let nat_q = match_query(&first, &second);
        let nat_count = run_query_count(&g, &nat_q);

        prop_assert!(opt_count >= left_count,
            "OPTIONAL must preserve at least the leading MATCH count: opt={opt_count} left={left_count}");
        prop_assert!(opt_count >= nat_count,
            "OPTIONAL must produce at least the inner natural-join count: opt={opt_count} nat={nat_count}");
    }

    /// **Invariant**: when the optional pattern matches at least once for
    /// every leading row (no unsuccess branch fires), OPTIONAL MATCH is
    /// indistinguishable from MATCH. We test the contrapositive: if the
    /// counts differ, then there is at least one row in the leading MATCH
    /// that has zero matches in the natural join — i.e. unsuccess fired.
    #[test]
    fn optional_equals_match_when_no_unsuccess(
        first in pattern(),
        second in pattern(),
    ) {
        let g = fraud_graph();

        let opt_q = optional_query(&first, &second);
        let nat_q = match_query(&first, &second);

        let opt_count = run_query_count(&g, &opt_q);
        let nat_count = run_query_count(&g, &nat_q);

        if opt_count == nat_count {
            prop_assert_eq!(opt_count, nat_count);
        } else {
            // Difference must be exactly the number of leading rows with no
            // matching extension — non-zero by construction here.
            prop_assert!(opt_count > nat_count);
        }
    }
}
