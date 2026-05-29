//! Tests for `COALESCE` (ISO/IEC 39075:2024 §20.7).

use gqlrust::compile_query;
use gqlrust::compile_query_unchecked;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::syntax::expr::Expr;

fn graph_with_optional_email() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Alice", "email": "alice@a.com"}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Bob"}},
        {"id": "u3", "labels": ["User"], "props": {}}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run_projected(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    }
}

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
    let r = compile_query_unchecked("MATCH (x) RETURN COALESCE(x.a)");
    let err = r.expect_err("single-arg COALESCE must be a parse error");
    assert!(err.contains("at least two arguments"), "got: {err}");
}

#[test]
fn parser_coalesce_zero_args_is_error() {
    let r = compile_query_unchecked("MATCH (x) RETURN COALESCE()");
    assert!(r.is_err());
}

#[test]
fn parser_coalesce_keeps_lowercase_property_name_unaffected() {
    let q = compile_query_unchecked("MATCH (x) RETURN x.coalesce");
    assert!(q.is_ok(), "got {q:?}");
}

#[test]
fn runtime_returns_first_non_null_value() {
    let g = graph_with_optional_email();
    let mut rows = run_projected(
        &g,
        "MATCH (x: User) RETURN x.name AS who, COALESCE(x.email, x.name, 'fallback') AS contact",
    );
    rows.sort_by(|a, b| match (&a[0], &b[0]) {
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
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
    let g = graph_with_optional_email();
    let rows = run_projected(
        &g,
        "MATCH (x: User) WHERE x.name = 'Alice' \
         RETURN COALESCE(x.name, 999) AS first",
    );
    assert_eq!(rows, vec![vec![Value::Str("Alice".into())]]);
}

#[test]
fn runtime_in_where_predicate() {
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
    let json = r#"{
      "nodes": [
        {"id": "n1", "labels": ["X"], "props": {"a": 7, "b": 2.5}},
        {"id": "n2", "labels": ["X"], "props": {"b": 2.5}},
        {"id": "n3", "labels": ["X"], "props": {}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let rt = Runtime::new(&g);
    let q = compile_query("MATCH (x: X) RETURN COALESCE(x.a, x.b, 0) AS pick").unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    assert_eq!(rows.len(), 3);
    let mut got: Vec<Value> = rows.into_iter().map(|r| r[0].clone()).collect();
    got.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    let mut want = vec![Value::Int(7), Value::Float(2.5), Value::Int(0)];
    want.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(got, want);
}
