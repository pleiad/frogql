//! Tests for `COALESCE(v1, v2, ..., vN)` (ISO/IEC 39075:2024 §20.7
//! `<case abbreviation>`).
//!
//! Specification (recursive equivalence, SR 1c-1d):
//! ```text
//! COALESCE(V1, V2)        ≡ CASE WHEN NOT V1 IS NULL THEN V1 ELSE V2 END
//! COALESCE(V1, ..., Vn)   ≡ CASE WHEN NOT V1 IS NULL THEN V1
//!                                ELSE COALESCE(V2, ..., Vn) END
//! ```
//!
//! gqlite implements this directly as a left-to-right scan in the
//! runtime: returns the first operand whose evaluation produces a
//! non-null `Success`. `Failure` (missing attribute, unbound variable)
//! is treated as null per the engine's existing 3VL convention.

use gqlrust::compile_query;
use gqlrust::compile_query_unchecked;
use gqlrust::model::graph::Graph;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::syntax::expr::Expr;

/// Three users: Alice has both name and email, Bob has only name,
/// Carol has neither but exists. Designed for testing COALESCE
/// fall-through.
fn graph_with_optional_email() -> Graph {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Alice", "email": "alice@a.com"}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Bob"}},
        {"id": "u3", "labels": ["User"], "props": {}}
      ],
      "edges": []
    }"#;
    Graph::from_json_str(json).unwrap()
}

fn run_projected(g: &Graph, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    }
}

// =====================================================================
// Parser
// =====================================================================

#[test]
fn parser_coalesce_two_args_produces_coalesce_expr() {
    let q = compile_query_unchecked("MATCH (x) RETURN COALESCE(x.email, x.name)").unwrap();
    let items = q.returns.unwrap();
    let expr = match &items[0] {
        gqlrust::syntax::query::ReturnItem::Expr { expr, .. } => expr,
        _ => panic!("expected Expr return item"),
    };
    match expr {
        Expr::Coalesce(args) => assert_eq!(args.len(), 2),
        other => panic!("expected Expr::Coalesce, got {other:?}"),
    }
}

#[test]
fn parser_coalesce_lowercase_works() {
    let q = compile_query_unchecked("MATCH (x) RETURN coalesce(x.a, x.b, x.c)").unwrap();
    let items = q.returns.unwrap();
    if let gqlrust::syntax::query::ReturnItem::Expr {
        expr: Expr::Coalesce(args),
        ..
    } = &items[0]
    {
        assert_eq!(args.len(), 3);
    } else {
        panic!("expected Coalesce");
    }
}

#[test]
fn parser_coalesce_one_arg_is_error() {
    // Per ISO §20.7: `COALESCE ( V1 { , V2 }... )` requires the
    // repetition to fire at least once → minimum 2 operands.
    let r = compile_query_unchecked("MATCH (x) RETURN COALESCE(x.a)");
    let err = r.expect_err("single-arg COALESCE must be a parse error");
    assert!(
        err.contains("at least two arguments"),
        "error must mention min-2-args rule, got: {err}"
    );
}

#[test]
fn parser_coalesce_zero_args_is_error() {
    let r = compile_query_unchecked("MATCH (x) RETURN COALESCE()");
    assert!(r.is_err());
}

#[test]
fn parser_coalesce_keeps_lowercase_property_name_unaffected() {
    // `coalesce` should remain a usable property name when not
    // followed by `(`. The lexer's soft-keyword rule (Count/Sum/...)
    // applies here too.
    let q = compile_query_unchecked("MATCH (x) RETURN x.coalesce");
    assert!(q.is_ok(), "got {q:?}");
}

// =====================================================================
// Runtime — first-non-null semantics
// =====================================================================

#[test]
fn runtime_returns_first_non_null_value() {
    // Alice has both email and name; COALESCE picks email (first arg).
    // Bob has only name; COALESCE falls through to name.
    // Carol has neither; COALESCE returns the literal "fallback".
    let g = graph_with_optional_email();
    let mut rows = run_projected(
        &g,
        "MATCH (x: User) RETURN x.name AS who, COALESCE(x.email, x.name, 'fallback') AS contact",
    );
    rows.sort_by(|a, b| match (&a[0], &b[0]) {
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        // Carol has no name; sort her last.
        (Value::Str(_), Value::Null) => std::cmp::Ordering::Less,
        (Value::Null, Value::Str(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("Alice".into()), Value::Str("alice@a.com".into())],
            vec![Value::Str("Bob".into()), Value::Str("Bob".into())],
            vec![Value::Null, Value::Str("fallback".into())],
        ]
    );
}

#[test]
fn runtime_returns_null_when_all_args_are_null() {
    let g = graph_with_optional_email();
    let rows = run_projected(
        &g,
        "MATCH (x: User) WHERE x.name = 'Alice' \
         RETURN COALESCE(x.foo, x.bar, x.baz) AS missing",
    );
    assert_eq!(rows, vec![vec![Value::Null]]);
}

#[test]
fn runtime_explicit_null_literal_is_skipped() {
    // ISO §20.7 SR 1d: nulls (any source — `null` literal, missing
    // attr, unbound var) are skipped. The non-null literal that
    // follows wins.
    let g = graph_with_optional_email();
    let rows = run_projected(
        &g,
        "MATCH (x: User) WHERE x.name = 'Bob' \
         RETURN COALESCE(null, x.email, 42) AS pick",
    );
    assert_eq!(rows, vec![vec![Value::Int(42)]]);
}

#[test]
fn runtime_short_circuits_on_first_non_null() {
    // `x.name` is non-null for every User → COALESCE returns it
    // before evaluating subsequent args. We can't observe the
    // evaluation order directly, but we can check the result type.
    let g = graph_with_optional_email();
    let rows = run_projected(
        &g,
        "MATCH (x: User) WHERE x.name = 'Alice' \
         RETURN COALESCE(x.name, 999) AS first",
    );
    // If short-circuited, the value is the string "Alice" not 999.
    assert_eq!(rows, vec![vec![Value::Str("Alice".into())]]);
}

#[test]
fn runtime_in_where_predicate() {
    // COALESCE in a WHERE expression: filter users whose effective
    // contact is alice@a.com, where contact = email OR name fallback.
    let g = graph_with_optional_email();
    let rows = run_projected(
        &g,
        "MATCH (x: User) \
         WHERE COALESCE(x.email, x.name) = 'alice@a.com' \
         RETURN x.name",
    );
    assert_eq!(rows, vec![vec![Value::Str("Alice".into())]]);
}

#[test]
fn runtime_inside_order_by_via_alias_substitution() {
    // Alias resolution from the ORDER BY work picks up COALESCE just
    // like any other Expr. Sort users by their effective contact
    // (email if present, else name). Result order:
    //   Bob       → "Bob"
    //   Alice     → "alice@a.com"
    //   Carol     → null (NULLS LAST default)
    let g = graph_with_optional_email();
    let rows = run_projected(
        &g,
        "MATCH (x: User) \
         RETURN x.name, COALESCE(x.email, x.name) AS contact \
         ORDER BY contact",
    );
    let contacts: Vec<&Value> = rows.iter().map(|r| &r[1]).collect();
    assert_eq!(
        contacts,
        vec![
            &Value::Str("Bob".into()),
            &Value::Str("alice@a.com".into()),
            &Value::Null,
        ]
    );
}

#[test]
fn runtime_int_promotion_in_coalesce_does_not_crash() {
    // Mixed Int/Float in COALESCE: each arg evaluated independently;
    // we just pick the first non-null. No promotion needed (the
    // result keeps the type of whichever arg won).
    let json = r#"{
      "nodes": [
        {"id": "n1", "labels": ["X"], "props": {"a": 7, "b": 2.5}},
        {"id": "n2", "labels": ["X"], "props": {"b": 2.5}},
        {"id": "n3", "labels": ["X"], "props": {}}
      ],
      "edges": []
    }"#;
    let g = Graph::from_json_str(json).unwrap();
    let rt = Runtime::new(&g);
    let q = compile_query("MATCH (x: X) RETURN COALESCE(x.a, x.b, 0) AS pick").unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    assert_eq!(rows.len(), 3);
    // n1 has a=7 → Int(7); n2 lacks a, has b=2.5 → Float(2.5);
    // n3 lacks both → Int(0). Order may be any; assert content.
    let mut got: Vec<Value> = rows.into_iter().map(|r| r[0].clone()).collect();
    got.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    let mut want = vec![Value::Int(7), Value::Float(2.5), Value::Int(0)];
    want.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(got, want);
}
