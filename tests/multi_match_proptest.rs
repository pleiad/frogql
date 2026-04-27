//! Property-based tests for the multi-MATCH refactor (ISO §14.3-14.4).
//!
//! Complements the example-based suite in `multi_match_test.rs` by
//! exploring the space of `Vec<MatchStatement>` inputs that the parser
//! does not (yet) emit. Each property captures an invariant that must
//! hold for *any* sequence of Simple match statements; if the invariant
//! breaks for some sequence, the refactor's soundness claim is wrong.
//!
//! Strategies are restricted to a curated set of valid pattern strings
//! so the parser doesn't dominate the search space — proptest is for
//! exploring `collapsed_pattern()` and the runtime/optimizer pipeline,
//! not the parser.

use std::path::Path;

use gqlrust::elaborate::elaborate_query;
use gqlrust::model::graph::Graph;
use gqlrust::parser;
use gqlrust::runtime::engine::Runtime;
use gqlrust::syntax::query::{MatchStatement, Query};
use proptest::prelude::*;

fn fraud_graph() -> Graph {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    Graph::from_file(&p).unwrap()
}

/// Strategy: a single-pattern string drawn from a curated set of valid
/// patterns over the fraud graph schema. Mixing these in different
/// sequences is what proptest will explore.
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

/// Build a multi-MATCH Query from pattern strings, parsed and elaborated.
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
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        // 32 cases is plenty for 8-pattern alphabet; default 256 is overkill.
        cases: 32,
        ..ProptestConfig::default()
    })]

    /// **Invariant**: for any sequence of patterns, the collapsed multi-MATCH
    /// query produces the same row count as the equivalent single-MATCH
    /// comma-join. This is the central soundness claim of the refactor —
    /// §14.4 GR 1 says MATCH expands the working table by natural join,
    /// which is exactly what `PathPattern::Join` already encodes.
    ///
    /// If this property breaks for any sequence, the collapse is unsound
    /// and the runtime is silently producing wrong results for what the
    /// parser will eventually emit as `MATCH p1 MATCH p2 ...`.
    #[test]
    fn multi_match_runtime_matches_comma_join(
        patterns in proptest::collection::vec(pattern(), 1..=4)
    ) {
        let g = fraud_graph();
        let runtime = Runtime::new(&g);

        // Multi-MATCH form: each pattern is its own Simple match statement.
        let multi = multi_match_query(&patterns);
        let multi_rows = runtime.run(&multi.collapsed_pattern()).rows.len();

        // Comma-join form: all patterns joined into a single Simple via `,`.
        let comma_form = patterns.join(", ");
        let comma = multi_match_query(&[comma_form]);
        let comma_rows = runtime.run(&comma.collapsed_pattern()).rows.len();

        prop_assert_eq!(multi_rows, comma_rows);
    }

    /// **Invariant**: a multi-MATCH Query where some patterns share a
    /// variable (natural join) cannot produce more rows than the same
    /// set treated as disjoint (cartesian product). Natural join ⊆
    /// cartesian product set-theoretically.
    ///
    /// Concretely: shared `x` either binds to the same node across both
    /// matches (filtering) or eliminates rows. It never adds rows.
    #[test]
    fn natural_join_at_most_cartesian(
        first in pattern(),
        second in pattern()
    ) {
        let g = fraud_graph();
        let runtime = Runtime::new(&g);

        let q = multi_match_query(&[first, second]);
        let join_rows = runtime.run(&q.collapsed_pattern()).rows.len();

        // Each individual MATCH's row count.
        let p1 = q.matches[0].pattern();
        let p2 = q.matches[1].pattern();
        let cardinality_1 = runtime.run(p1).rows.len();
        let cardinality_2 = runtime.run(p2).rows.len();

        prop_assert!(
            join_rows <= cardinality_1 * cardinality_2,
            "join={} > cartesian={}*{}={}",
            join_rows, cardinality_1, cardinality_2,
            cardinality_1 * cardinality_2
        );
    }

    /// **Invariant**: after running through the optimizer, every Query
    /// has exactly one `MatchStatement::Simple`. The collapse-then-optimize
    /// strategy in `lib.rs::optimize_query` documents this as the post-
    /// optimization shape; the property test pins it across the input
    /// space.
    #[test]
    fn optimize_collapses_to_single_simple(
        patterns in proptest::collection::vec(pattern(), 1..=4)
    ) {
        // Build the equivalent single-MATCH input string and round-trip
        // through the public `compile_query_unchecked`, which calls
        // `optimize_query` under the hood.
        let input = patterns.join(", ");
        let optimized = gqlrust::compile_query_unchecked(&input)
            .expect("curated patterns must compile");

        prop_assert_eq!(optimized.matches.len(), 1);
        let is_simple = matches!(&optimized.matches[0], MatchStatement::Simple { .. });
        prop_assert!(is_simple);
    }
}
