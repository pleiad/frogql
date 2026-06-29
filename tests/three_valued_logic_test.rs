//! SQL three-valued logic (3VL) for the boolean connectives `AND` / `OR` /
//! `NOT`, the `null AS T` cast pass-through, and the cast-error empties-the-row
//! behavior. Boolean-connective scope only — comparison/equality operators
//! (`=`, `<>`, `<`, ...) keep their pre-existing behavior here and are handled
//! in a follow-up PR; the guards at the bottom pin that pre-existing behavior.

use frogql::elaborate;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::parser;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::typing::checker::{TypecheckResult, Typechecker};

// ---------------------------------------------------------------------------
// Harnesses
// ---------------------------------------------------------------------------

fn run(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = frogql::compile_query(q).expect("compile failed");
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected rows for {q:?}, got {other:?}"),
    }
}

/// One bare node so a scalar `RETURN <expr>` produces exactly one row.
fn one_node() -> MemoryGraphStore {
    let json = r#"{"nodes":[{"id":"n1","labels":["N"],"props":{"s":"a"}}],"edges":[]}"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// Single-cell scalar result of `RETURN (<expr>) AS b` over one node.
fn scalar(expr: &str) -> Value {
    let g = one_node();
    let rows = run(&g, &format!("MATCH (x:N) RETURN ({expr}) AS b"));
    assert_eq!(rows.len(), 1, "expected one row for {expr:?}");
    rows.into_iter().next().unwrap().into_iter().next().unwrap()
}

/// Parse, elaborate, and typecheck a full query under the permissive star
/// schema. Returns the typecheck result plus errors/warnings.
fn check_full_query(query: &str) -> (TypecheckResult, Vec<String>, Vec<String>) {
    let q = parser::parse_query(query).expect("parse failed");
    let q = elaborate::elaborate_query(q);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    (r, tc.errors.clone(), tc.warnings.clone())
}

// ---------------------------------------------------------------------------
// Fix 2 — 3VL truth tables for OR / AND / NOT (literal null operand)
// ---------------------------------------------------------------------------

#[test]
fn test_or_truth_table() {
    assert_eq!(scalar("true OR null"), Value::Bool(true)); // short-circuit
    assert_eq!(scalar("null OR true"), Value::Bool(true));
    assert_eq!(scalar("false OR null"), Value::Null);
    assert_eq!(scalar("null OR false"), Value::Null);
    assert_eq!(scalar("null OR null"), Value::Null);
    assert_eq!(scalar("true OR false"), Value::Bool(true));
    assert_eq!(scalar("false OR false"), Value::Bool(false));
}

#[test]
fn test_and_truth_table() {
    assert_eq!(scalar("false AND null"), Value::Bool(false)); // short-circuit
    assert_eq!(scalar("null AND false"), Value::Bool(false));
    assert_eq!(scalar("true AND null"), Value::Null);
    assert_eq!(scalar("null AND true"), Value::Null);
    assert_eq!(scalar("null AND null"), Value::Null);
    assert_eq!(scalar("true AND true"), Value::Bool(true));
    assert_eq!(scalar("true AND false"), Value::Bool(false));
}

#[test]
fn test_not_null() {
    assert_eq!(scalar("NOT null"), Value::Null);
    assert_eq!(scalar("NOT true"), Value::Bool(false));
    assert_eq!(scalar("NOT false"), Value::Bool(true));
}

// ---------------------------------------------------------------------------
// Fix 2 — a missing/null property surfaces as the 3VL unknown (the engine
// reads a missing attribute as a Failure, treated as null by the connectives).
// ---------------------------------------------------------------------------

/// Three nodes: active=true, active=false, and one with `active` absent.
fn active_graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id":"a","labels":["N"],"props":{"name":"A","active":true}},
        {"id":"b","labels":["N"],"props":{"name":"B","active":false}},
        {"id":"c","labels":["N"],"props":{"name":"C"}}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

#[test]
fn test_missing_property_is_unknown_in_or() {
    // false OR x.active: true -> true, false -> false, missing -> null.
    let g = active_graph();
    let mut rows = run(&g, "MATCH (x:N) RETURN x.name, (false OR x.active) AS b");
    rows.sort_by(|l, r| format!("{:?}", l[0]).cmp(&format!("{:?}", r[0])));
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("A".into()), Value::Bool(true)],
            vec![Value::Str("B".into()), Value::Bool(false)],
            vec![Value::Str("C".into()), Value::Null],
        ]
    );
}

#[test]
fn test_missing_property_short_circuits_in_and() {
    // false AND x.active = false for every row, even the missing one.
    let g = active_graph();
    let rows = run(&g, "MATCH (x:N) RETURN (false AND x.active) AS b");
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(row, &vec![Value::Bool(false)]);
    }
}

/// Names returned by a query, sorted.
fn names(g: &MemoryGraphStore, q: &str) -> Vec<String> {
    let mut ns: Vec<String> = run(g, q)
        .into_iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            other => panic!("expected Str name, got {other:?}"),
        })
        .collect();
    ns.sort();
    ns
}

// These WHERE-clause cases FAIL under the pre-fix behavior: `x.status` is a
// missing attribute, which the engine reads as a Failure. The old code routed
// the connective through the implicit argument-cast wrapper, which propagated
// that Failure (`get_bool` -> false) and dropped EVERY row. Correct 3VL keeps
// them all: `unknown OR true = true`.

#[test]
fn test_missing_or_true_returns_all_rows() {
    let g = active_graph(); // no node has a `status` property
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.status OR true RETURN x.name"),
        vec!["A", "B", "C"],
    );
}

#[test]
fn test_true_or_missing_returns_all_rows() {
    let g = active_graph();
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE true OR x.status RETURN x.name"),
        vec!["A", "B", "C"],
    );
}

#[test]
fn test_missing_or_true_in_projection_is_true() {
    // Same divergence in a projection: pre-fix yielded a Null cell, correct
    // 3VL yields true.
    let g = active_graph();
    let rows = run(&g, "MATCH (x:N) RETURN (x.status OR true) AS b");
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(row, &vec![Value::Bool(true)]);
    }
}

// ---------------------------------------------------------------------------
// Fix 3 / Fix 4 — cast (`as`) null pass-through and invalid-cast handling
// ---------------------------------------------------------------------------

#[test]
fn test_null_cast_passes_through() {
    // `null AS T` (bare AS = assert) is null regardless of target.
    assert_eq!(scalar("null AS INTEGER"), Value::Null);
    assert_eq!(scalar("null AS FLOAT"), Value::Null);
    assert_eq!(scalar("null AS STRING"), Value::Null);
}

#[test]
fn test_invalid_cast_in_where_empties_result() {
    // Case A: an impossible cast in a WHERE filter drops the row (no crash).
    let g = one_node(); // x.s = "a"
    let rows = run(&g, "MATCH (x:N) WHERE CAST(x.s AS INTEGER) > 0 RETURN x.s");
    assert!(rows.is_empty(), "expected no rows, got {rows:?}");
}

#[test]
fn test_invalid_cast_in_return_is_null_cell() {
    // Case B (Option R): an impossible cast in a projection yields a null
    // cell; the row is kept. (No FPPCTypeError channel in this PR.)
    let g = one_node(); // x.s = "a"
    let rows = run(&g, "MATCH (x:N) RETURN CAST(x.s AS INTEGER) AS i");
    assert_eq!(rows, vec![vec![Value::Null]]);
}

// ---------------------------------------------------------------------------
// Fix 1 — typechecker short-circuit override for AND / OR
// ---------------------------------------------------------------------------

fn or_warns(query: &str) -> bool {
    let (_, _, warnings) = check_full_query(query);
    warnings
        .iter()
        .any(|w| w.contains("Or") && w.contains("is not defined"))
}

#[test]
fn test_typecheck_or_with_one_bottom_operand_is_bool() {
    // `1 OR true`: Integer meets ⊥ against Bool, but the other side is a
    // valid Bool, so under the short-circuit override the result is Bool and
    // the OR itself produces no "is not defined" warning (A1 leniency).
    let (r, errs, _) = check_full_query("MATCH (x) WHERE 1 OR true RETURN x");
    assert!(r.ok, "expected ok, errors={errs:?}");
    assert!(!or_warns("MATCH (x) WHERE 1 OR true RETURN x"));
}

#[test]
fn test_typecheck_or_with_both_bottom_operands_degrades() {
    // `1 OR 'a'`: both operands meet ⊥ against Bool -> degrade to ⊥ + warn.
    assert!(
        or_warns("MATCH (x) WHERE 1 OR 'a' RETURN x"),
        "expected an 'Or ... is not defined' warning"
    );
}

#[test]
fn test_typecheck_arithmetic_mismatch_still_degrades() {
    // Regression: non-short-circuit ops keep the default cod degradation.
    let (_, _, warnings) = check_full_query("MATCH (x) RETURN 1 + true");
    assert!(
        warnings.iter().any(|w| w.contains("is not defined")),
        "expected a degradation warning for 1 + true, got: {warnings:?}"
    );
}

// ---------------------------------------------------------------------------
// No-regression guards — comparison/equality stay at pre-existing behavior.
// These pin the current (non-3VL) results; the comparison-3VL follow-up PR
// is expected to flip the null-comparison ones.
// ---------------------------------------------------------------------------

#[test]
fn test_guard_eq_null_literal_drops_rows() {
    // Pre-existing (and ISO-aligned): `= null` never matches -> empty.
    let g = active_graph();
    let rows = run(&g, "MATCH (x:N) WHERE x.active = null RETURN x.name");
    assert!(rows.is_empty(), "expected no rows, got {rows:?}");
}

#[test]
fn test_guard_null_eq_null_is_true_pre_existing() {
    // PRE-EXISTING WART (to be fixed in the comparison-3VL follow-up PR):
    // structural equality makes `null = null` true rather than null.
    assert_eq!(scalar("null = null"), Value::Bool(true));
}

#[test]
fn test_guard_basic_comparison_unaffected() {
    assert_eq!(scalar("1 = 1"), Value::Bool(true));
    assert_eq!(scalar("1 = 2"), Value::Bool(false));
    assert_eq!(scalar("2 > 1"), Value::Bool(true));
}
