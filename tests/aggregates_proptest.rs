//! Property-based tests for aggregate functions (ISO 39075 §20.9).
//!
//! Complements the example-based tests in `count_test.rs` by checking
//! algebraic invariants that must hold for *every* valid (pattern,
//! attribute) combination — not just the handful written by hand.
//!
//! The invariants are non-trivial: `COUNT(*) ≥ COUNT(x.a)` would be wrong
//! once we model NULL (currently equal because we have no NULL); but
//! `COUNT(x.a) ≥ COUNT(DISTINCT x.a)` and `MIN ≤ MAX` are unconditional
//! and good targets for property testing.
//!
//! Inputs are drawn from a curated set of (pattern, var.attr) pairs over
//! the fraud-graph fixture so that every generated query is well-typed
//! and produces a non-empty binding table.

use std::path::Path;

use gqlrust::compile_query;
use gqlrust::model::graph::Graph;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use proptest::prelude::*;

fn fraud_graph() -> Graph {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    Graph::from_file(&p).unwrap()
}

/// Compile + run a full query, return the projected rows.
fn run_projected(g: &Graph, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile failed for {q:?}: {e}"));
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected Projected, got {other:?}"),
    }
}

/// Run a pattern (no RETURN clause) and count the resulting rows. Uses
/// `QueryResult::Raw` because RETURN'ing a bare variable like `x` is a
/// parse error in the current grammar.
fn match_row_count(g: &Graph, q: &str) -> usize {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile failed for {q:?}: {e}"));
    match rt.run_query(&query, 0) {
        QueryResult::Raw(ir) => ir.rows.len(),
        other => panic!("expected Raw, got {other:?}"),
    }
}

/// Extract a single integer from a one-row, one-column projection.
fn one_int(rows: &[Vec<Value>]) -> i64 {
    assert_eq!(rows.len(), 1, "expected one row, got {rows:?}");
    assert_eq!(rows[0].len(), 1, "expected one column, got {rows:?}");
    match &rows[0][0] {
        Value::Int(n) => *n,
        v => panic!("expected Int, got {v:?}"),
    }
}

/// Compare two `Value`s as numerics. Returns `None` when either side is
/// not numeric (the runtime can return `Value::Str("NULL")` as the null
/// sentinel; tests should treat that as "no comparable value").
fn cmp_numeric(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)),
        _ => None,
    }
}

/// Strategy: a pattern binding a variable, paired with the variable name
/// and an attribute that exists on the matched elements. Every pair
/// returns a non-empty binding table on `fraud.json`.
fn pattern_var_attr() -> impl Strategy<Value = (String, String, String)> {
    prop_oneof![
        // Account nodes (4 of them) with string + bool attrs.
        Just((
            "MATCH (x: Account)".to_string(),
            "x".to_string(),
            "owner".to_string()
        )),
        Just((
            "MATCH (x: Account)".to_string(),
            "x".to_string(),
            "isBlocked".to_string()
        )),
        // All nodes (5) — `owner` exists on every node in the fixture.
        Just((
            "MATCH (x)".to_string(),
            "x".to_string(),
            "owner".to_string()
        )),
        // Edges with numeric attribute.
        Just((
            "MATCH ()-[e:Transfer]->()".to_string(),
            "e".to_string(),
            "amount".to_string()
        )),
    ]
}

/// Strategy: pattern + numeric attribute only (subset of pattern_var_attr).
fn pattern_var_numeric_attr() -> impl Strategy<Value = (String, String, String)> {
    prop_oneof![Just((
        "MATCH ()-[e:Transfer]->()".to_string(),
        "e".to_string(),
        "amount".to_string()
    )),]
}

/// Strategy: a pattern that produces a non-empty binding table on
/// fraud.json. Used for COUNT(*) invariants where no attribute is
/// referenced.
fn nonempty_pattern() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("MATCH (x)".to_string()),
        Just("MATCH (x: Account)".to_string()),
        Just("MATCH (x: Person)".to_string()),
        Just("MATCH ()-[e:Transfer]->()".to_string()),
        Just("MATCH (x: Account)-[:Transfer]->(y: Account)".to_string()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        ..ProptestConfig::default()
    })]

    /// **Invariant** (§20.9 GR 2): `COUNT(*)` is the cardinality of the
    /// binding table. For any pattern P, `MATCH P RETURN COUNT(*)` must
    /// produce one row whose value equals the number of rows produced by
    /// running `MATCH P` raw.
    #[test]
    fn count_star_equals_match_cardinality(p in nonempty_pattern()) {
        let g = fraud_graph();

        // Same pattern as full query (RETURN COUNT(*)) and as bare match
        // (no RETURN — runtime returns the raw binding rows).
        let count_q = format!("{p} RETURN COUNT(*)");
        let count = one_int(&run_projected(&g, &count_q));
        let raw_count = match_row_count(&g, &p);

        prop_assert_eq!(count, raw_count as i64);
    }

    /// **Invariant** (§20.9 GR 5b on DISTINCT): `COUNT(DISTINCT x.a)` ≤
    /// `COUNT(x.a)` for any pattern, var, and attribute. Distinct
    /// values are a subset of all values.
    #[test]
    fn count_distinct_at_most_count(
        (pattern, var, attr) in pattern_var_attr()
    ) {
        let g = fraud_graph();
        let q = format!(
            "{pattern} RETURN COUNT({var}.{attr}), COUNT(DISTINCT {var}.{attr})"
        );
        let rows = run_projected(&g, &q);

        prop_assert_eq!(rows.len(), 1);
        prop_assert_eq!(rows[0].len(), 2);
        let total = match &rows[0][0] {
            Value::Int(n) => *n,
            v => panic!("expected Int, got {v:?}"),
        };
        let distinct = match &rows[0][1] {
            Value::Int(n) => *n,
            v => panic!("expected Int, got {v:?}"),
        };
        prop_assert!(
            distinct <= total,
            "COUNT(DISTINCT {var}.{attr}) = {distinct} > COUNT({var}.{attr}) = {total}"
        );
    }

    /// **Invariant** (ordering, §22.14): `MIN(x.a) ≤ MAX(x.a)` for any
    /// non-empty input over a numeric attribute. Both must be `<=`-
    /// comparable as numerics; the implementation is allowed to use
    /// any total order on the value type as long as it is consistent.
    #[test]
    fn min_at_most_max_numeric(
        (pattern, var, attr) in pattern_var_numeric_attr()
    ) {
        let g = fraud_graph();
        let q = format!("{pattern} RETURN MIN({var}.{attr}), MAX({var}.{attr})");
        let rows = run_projected(&g, &q);

        prop_assert_eq!(rows.len(), 1);
        prop_assert_eq!(rows[0].len(), 2);
        let min = &rows[0][0];
        let max = &rows[0][1];
        let ord = cmp_numeric(min, max).unwrap_or_else(|| {
            panic!("MIN/MAX results not numerically comparable: {min:?}, {max:?}")
        });
        prop_assert!(
            ord != std::cmp::Ordering::Greater,
            "MIN({var}.{attr}) = {min:?} > MAX({var}.{attr}) = {max:?}"
        );
    }

    /// **Invariant**: `MIN(x.a) ≤ AVG(x.a) ≤ MAX(x.a)` for any non-empty
    /// numeric input. AVG is bounded by the extrema by definition. This
    /// catches bugs in numeric promotion in AVG that wouldn't be caught
    /// by MIN/MAX alone.
    #[test]
    fn avg_between_min_and_max(
        (pattern, var, attr) in pattern_var_numeric_attr()
    ) {
        let g = fraud_graph();
        let q = format!(
            "{pattern} RETURN MIN({var}.{attr}), AVG({var}.{attr}), MAX({var}.{attr})"
        );
        let rows = run_projected(&g, &q);

        prop_assert_eq!(rows.len(), 1);
        prop_assert_eq!(rows[0].len(), 3);
        let min = &rows[0][0];
        let avg = &rows[0][1];
        let max = &rows[0][2];

        let min_le_avg = cmp_numeric(min, avg).expect("MIN/AVG must be comparable");
        let avg_le_max = cmp_numeric(avg, max).expect("AVG/MAX must be comparable");
        prop_assert!(min_le_avg != std::cmp::Ordering::Greater);
        prop_assert!(avg_le_max != std::cmp::Ordering::Greater);
    }
}
