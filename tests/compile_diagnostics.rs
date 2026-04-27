//! Lib-level contract for `compile_query_with_diagnostics` and the
//! backward-compat `compile_query` wrapper.

use gqlrust::{
    compile_query, compile_query_unchecked, compile_query_with_diagnostics, CompileError,
};

#[test]
fn ok_clean_query_has_no_warnings() {
    let r = compile_query_with_diagnostics("MATCH (m:Movie) RETURN m.title")
        .expect("clean query should compile");
    assert!(r.warnings.is_empty(), "got: {:?}", r.warnings);
}

#[test]
fn ok_non_boolean_filter_produces_warning() {
    // `(x WHERE 1)` — filter has type Z, not B.
    let r = compile_query_with_diagnostics("(x WHERE 1)")
        .expect("query should compile (warnings, not errors)");
    assert!(
        r.warnings.iter().any(|w| w.contains("not a boolean")),
        "got: {:?}",
        r.warnings
    );
}

#[test]
fn err_parse_on_malformed_input() {
    let err = compile_query_with_diagnostics("MATCH (((").expect_err("malformed input should fail");
    assert!(matches!(err, CompileError::Parse(_)), "got: {:?}", err);
}

#[test]
fn err_type_on_unbound_variable() {
    let err = compile_query_with_diagnostics("MATCH (y) WHERE x.foo = 1 RETURN y.bar")
        .expect_err("unbound variable should fail");
    match err {
        CompileError::Type(es) => {
            assert!(
                es.iter()
                    .any(|e| e.contains("not found") && e.contains('x')),
                "got: {:?}",
                es
            );
        }
        other => panic!("expected Type error, got: {:?}", other),
    }
}

#[test]
fn compile_query_wraps_parse_error_with_prefix() {
    let err = compile_query("MATCH (((").unwrap_err();
    assert!(err.starts_with("Parse error: "), "got: {:?}", err);
}

#[test]
fn compile_query_wraps_type_error_with_prefix() {
    let err = compile_query("MATCH (y) WHERE x.foo = 1 RETURN y.bar").unwrap_err();
    assert!(err.starts_with("Type error: "), "got: {:?}", err);
}

#[test]
fn compile_query_unchecked_accepts_unbound_variable() {
    let _ = compile_query_unchecked("MATCH (y) WHERE x.foo = 1 RETURN y.bar")
        .expect("unchecked path should accept unbound variable");
}
