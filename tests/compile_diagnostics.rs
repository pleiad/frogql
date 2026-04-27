//! Tests for `compile_query_with_diagnostics`.
//!
//! These also cover `compile_query` (which is now a thin wrapper) — drift
//! between the two would surface here. The intent is to lock the lib-level
//! contract that the REPL depends on:
//!
//! - parse failures → `CompileError::Parse`
//! - typecheck failures → `CompileError::Type`
//! - typecheck warnings on a successful compile → `CompileResult.warnings`
//! - the legacy `compile_query` flat-string form preserves "Parse error: …"
//!   / "Type error: …" prefixes.

use gqlrust::{
    compile_query, compile_query_unchecked, compile_query_with_diagnostics,
    CompileError,
};

// -----------------------------------------------------------------------
// Successful compiles
// -----------------------------------------------------------------------

#[test]
fn ok_clean_query_has_no_warnings() {
    let r = compile_query_with_diagnostics("MATCH (m:Movie) RETURN m.title")
        .expect("clean query should compile");
    assert!(r.warnings.is_empty(), "expected no warnings, got: {:?}", r.warnings);
}

#[test]
fn ok_non_boolean_filter_produces_warning() {
    // `(x WHERE 1)` — the filter expression has type Z, not B.
    // Under the default Schema::star() the typechecker accepts the
    // query (returns Ok) but warns that the filter isn't boolean.
    //
    // (Typo'd attributes don't trigger a warning under Schema::star()
    // because gradual typing keeps them as Star — that case requires
    // a real Closed property type, which only appears with an explicit
    // schema. See fppc's test_bad_attribute for the analogous
    // accepted-without-warning case.)
    let r = compile_query_with_diagnostics("(x WHERE 1)")
        .expect("query should compile (warnings, not errors)");
    assert!(
        !r.warnings.is_empty(),
        "expected a warning for non-boolean filter, got none"
    );
    assert!(
        r.warnings.iter().any(|w| w.contains("not a boolean")),
        "expected 'not a boolean' in warning, got: {:?}",
        r.warnings
    );
}

// -----------------------------------------------------------------------
// Parse failures
// -----------------------------------------------------------------------

#[test]
fn err_parse_on_malformed_input() {
    let err = compile_query_with_diagnostics("MATCH (((")
        .expect_err("malformed input should fail");
    assert!(
        matches!(err, CompileError::Parse(_)),
        "expected Parse error, got: {:?}",
        err
    );
}

// -----------------------------------------------------------------------
// Typecheck failures
// -----------------------------------------------------------------------

#[test]
fn err_type_on_unbound_variable() {
    // `y` is bound by the pattern; `x` is not. WHERE references
    // unbound `x.foo` — typechecker rejects.
    let err = compile_query_with_diagnostics(
        "MATCH (y) WHERE x.foo = 1 RETURN y.bar",
    )
    .expect_err("unbound variable should fail");
    match err {
        CompileError::Type(es) => {
            assert!(!es.is_empty(), "expected at least one error message");
            assert!(
                es.iter().any(|e| e.contains("not found")),
                "expected 'not found' in error message, got: {:?}",
                es
            );
            assert!(
                es.iter().any(|e| e.contains('x')),
                "expected variable name 'x' in error message, got: {:?}",
                es
            );
        }
        other => panic!("expected Type error, got: {:?}", other),
    }
}

// -----------------------------------------------------------------------
// Backward-compat wrapper preserves message format
// -----------------------------------------------------------------------

#[test]
fn compile_query_wraps_parse_error_with_prefix() {
    let err = compile_query("MATCH (((").unwrap_err();
    assert!(
        err.starts_with("Parse error: "),
        "expected 'Parse error: …' prefix, got: {:?}",
        err
    );
}

#[test]
fn compile_query_wraps_type_error_with_prefix() {
    let err = compile_query("MATCH (y) WHERE x.foo = 1 RETURN y.bar").unwrap_err();
    assert!(
        err.starts_with("Type error: "),
        "expected 'Type error: …' prefix, got: {:?}",
        err
    );
}

// -----------------------------------------------------------------------
// Unchecked path is unaffected
// -----------------------------------------------------------------------

#[test]
fn compile_query_unchecked_accepts_unbound_variable() {
    // Without typechecking the query goes straight through. (At runtime
    // it would yield no results; we don't run it here.)
    let _ = compile_query_unchecked("MATCH (y) WHERE x.foo = 1 RETURN y.bar")
        .expect("unchecked path should accept unbound variable");
}
