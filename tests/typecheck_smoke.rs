//! Smoke tests for the phase-1 typechecker.
//!
//! Not a full test suite — these confirm that the on-by-default typecheck
//! path produces the same plan as the opt-out path for a handful of
//! representative queries, and that the checker rejects at least one
//! obviously-broken query that would have compiled silently before the
//! migration. Failure-detection coverage intentionally narrow.

use gqlrust::{compile_query, compile_query_unchecked};

/// Each query: must typecheck successfully, and must produce the same
/// optimized plan via the checked and unchecked entry points.
fn assert_smoke_query(q: &str) {
    let checked =
        compile_query(q).unwrap_or_else(|e| panic!("compile_query failed for {:?}: {}", q, e));
    let unchecked = compile_query_unchecked(q)
        .unwrap_or_else(|e| panic!("compile_query_unchecked failed for {:?}: {}", q, e));

    assert_eq!(
        format!("{:?}", checked.collapsed_pattern()),
        format!("{:?}", unchecked.collapsed_pattern()),
        "checked and unchecked plans diverge for {:?}",
        q
    );
}

#[test]
fn smoke_node_pattern_with_property_return() {
    assert_smoke_query("MATCH (x: Person) RETURN x.name");
}

#[test]
fn smoke_two_hop_with_filter() {
    assert_smoke_query(
        "MATCH (x: Account)-[:Transfer]->(y: Account) \
         WHERE x.isBlocked = false RETURN y.id",
    );
}

#[test]
fn smoke_concat_three_nodes_pattern_only() {
    assert_smoke_query("(x: {a int})(y: {b bool})(z: {c str})");
}

#[test]
fn smoke_typecheck_rejects_unbound_variable() {
    // y is bound by the pattern; x is not. WHERE x.foo references the unbound
    // variable. The checked path should reject this; the unchecked path will
    // happily produce a plan (which would fail at runtime).
    let q = "MATCH (y) WHERE x.foo = 1 RETURN y.bar";

    let checked = compile_query(q);
    assert!(
        checked.is_err(),
        "expected typecheck error for unbound x in WHERE, got Ok"
    );
    let err = checked.unwrap_err();
    assert!(
        err.contains("not found"),
        "expected 'not found' in error message, got: {}",
        err
    );

    // Opt-out path still produces a plan — confirms the typechecker is the
    // gatekeeper, not the parser/elaborator/optimizer.
    let unchecked = compile_query_unchecked(q);
    assert!(
        unchecked.is_ok(),
        "compile_query_unchecked should not enforce typing"
    );
}
