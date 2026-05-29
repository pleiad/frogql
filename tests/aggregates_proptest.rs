//! Property-based tests for aggregate functions (ISO §20.9).
//!
//! Algebraic invariants checked against random (pattern, var.attr) tuples
//! drawn from a curated set over the fraud-graph fixture.

use std::path::Path;

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use proptest::prelude::*;

fn fraud_graph() -> MemoryGraphStore {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    MemoryGraphStore::from_file(&p).unwrap()
}

fn run_projected(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile failed for {q:?}: {e}"));
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected Projected, got {other:?}"),
    }
}

/// Bare-pattern row count via `QueryResult::Raw`. Can't `RETURN x` (bare
/// variable is a parse error), so this skips the projection.
fn match_row_count(g: &MemoryGraphStore, q: &str) -> usize {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile failed for {q:?}: {e}"));
    match rt.run_query(&query, 0) {
        QueryResult::Raw(ir) => ir.rows.len(),
        other => panic!("expected Raw, got {other:?}"),
    }
}

fn one_int(rows: &[Vec<Value>]) -> i64 {
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 1);
    match &rows[0][0] {
        Value::Int(n) => *n,
        v => panic!("expected Int, got {v:?}"),
    }
}

fn cmp_numeric(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)),
        _ => None,
    }
}

fn triple(p: &str, v: &str, a: &str) -> (String, String, String) {
    (p.to_string(), v.to_string(), a.to_string())
}

/// Curated (pattern, var, attr) tuples that bind on fraud.json.
fn pattern_var_attr() -> impl Strategy<Value = (String, String, String)> {
    prop_oneof![
        Just(triple("MATCH (x: Account)", "x", "owner")),
        Just(triple("MATCH (x: Account)", "x", "isBlocked")),
        Just(triple("MATCH (x)", "x", "owner")),
        Just(triple("MATCH ()-[e:Transfer]->()", "e", "amount")),
    ]
}

fn pattern_var_numeric_attr() -> impl Strategy<Value = (String, String, String)> {
    prop_oneof![Just(triple("MATCH ()-[e:Transfer]->()", "e", "amount"))]
}

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
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// **Invariant** (§20.9 GR 2): `COUNT(*)` equals the row count of the
    /// underlying `MATCH P`.
    #[test]
    fn count_star_equals_match_cardinality(p in nonempty_pattern()) {
        let g = fraud_graph();
        let count_q = format!("{p} RETURN COUNT(*)");
        let count = one_int(&run_projected(&g, &count_q));
        let raw_count = match_row_count(&g, &p);
        prop_assert_eq!(count, raw_count as i64);
    }

    /// **Invariant** (§20.9 GR 5b): `COUNT(DISTINCT x.a) ≤ COUNT(x.a)`.
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
        prop_assert!(distinct <= total, "distinct={distinct} > total={total}");
    }

    /// **Invariant** (§22.14): `MIN(x.a) ≤ MAX(x.a)` for any non-empty
    /// numeric input.
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
        let ord = cmp_numeric(min, max).expect("min/max numerically comparable");
        prop_assert!(
            ord != std::cmp::Ordering::Greater,
            "min={min:?} > max={max:?}"
        );
    }

    /// **Invariant**: `MIN(x.a) ≤ AVG(x.a) ≤ MAX(x.a)` for non-empty
    /// numeric input. Catches AVG numeric-promotion bugs that MIN/MAX
    /// alone wouldn't surface.
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

        let min_le_avg = cmp_numeric(min, avg).expect("comparable");
        let avg_le_max = cmp_numeric(avg, max).expect("comparable");
        prop_assert!(min_le_avg != std::cmp::Ordering::Greater);
        prop_assert!(avg_le_max != std::cmp::Ordering::Greater);
    }
}
