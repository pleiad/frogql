//! Scalar math functions (issue #97).
//!
//! The report that opened that issue is the reason these exist: a user
//! asked for `sqrt(n.lat)` and got `unexpected token LParen at position
//! 8`, a message pointing at a parenthesis rather than at the missing
//! function. Writing a geographic query then meant pre-computing
//! `cos(latitude)` outside the engine, repeating every subexpression for
//! want of `pow`, and taking the square root client-side.
//!
//! What is pinned here: the values, the type results, null propagation,
//! the non-numeric rejection, and that every one of these names is still
//! usable as a variable, label and property — which matters, because
//! `abs`, `round`, `sign` and `degrees` are exactly the words a schema
//! uses for columns.

use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"i": 9, "f": 2.25, "neg": -4, "s": "x"}},
        {"id": "b", "labels": ["N"], "props": {"i": 16, "f": 0.5, "neg": -1, "s": "y"}}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn rows(q: &str) -> Vec<Vec<Value>> {
    let g = graph();
    let rt = Runtime::new(&g);
    let query = frogql::compile_query(q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
    match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("expected a projection, got {other:?}"),
    }
}

/// One row, one column.
fn scalar(expr: &str) -> Value {
    let q = format!("MATCH (x:N) WHERE x.i = 9 RETURN {expr}");
    let r = rows(&q);
    assert_eq!(r.len(), 1, "expected one row from `{q}`");
    r[0][0].clone()
}

fn approx(v: Value, want: f64) {
    match v {
        Value::Float(x) => assert!((x - want).abs() < 1e-9, "expected ≈{want}, got {x}"),
        other => panic!("expected a float, got {other:?}"),
    }
}

#[test]
fn one_argument_functions_compute() {
    approx(scalar("SQRT(x.i)"), 3.0);
    approx(scalar("CEIL(x.f)"), 3.0);
    approx(scalar("FLOOR(x.f)"), 2.0);
    approx(scalar("ROUND(x.f)"), 2.0);
    approx(scalar("EXP(0.0)"), 1.0);
    approx(scalar("LN(1.0)"), 0.0);
    approx(scalar("SIN(0.0)"), 0.0);
    approx(scalar("COS(0.0)"), 1.0);
    approx(scalar("TAN(0.0)"), 0.0);
    approx(scalar("DEGREES(RADIANS(180.0))"), 180.0);
}

#[test]
fn two_argument_functions_compute() {
    approx(scalar("POW(x.i, 2)"), 81.0);
    // LOG takes the base first, the SQL order.
    approx(scalar("LOG(2.0, 8.0)"), 3.0);
}

/// `CEILING` and `POWER` are the SQL spellings; both canonicalise so the
/// runtime has one arm each.
#[test]
fn sql_spellings_are_accepted() {
    approx(scalar("CEILING(x.f)"), 3.0);
    approx(scalar("POWER(x.i, 2)"), 81.0);
}

/// Case does not matter, the same as every other soft keyword here.
#[test]
fn names_are_case_insensitive() {
    approx(scalar("sqrt(x.i)"), 3.0);
    approx(scalar("Sqrt(x.i)"), 3.0);
}

/// `ABS` and `SIGN` of an integer are integers — an exact answer should
/// not be widened to a float. Everything else is float-valued, `FLOOR`
/// and friends included, which is what ISO says of `FLOOR` and what keeps
/// the three rounding functions consistent with each other.
#[test]
fn abs_and_sign_preserve_an_integer() {
    assert_eq!(scalar("ABS(x.neg)"), Value::Int(4));
    assert_eq!(scalar("SIGN(x.neg)"), Value::Int(-1));
    assert_eq!(scalar("SIGN(x.i)"), Value::Int(1));
    assert_eq!(scalar("SIGN(0)"), Value::Int(0));
    // The float forms stay floats.
    approx(scalar("ABS(0.0 - 2.25)"), 2.25);
    approx(scalar("SIGN(0.0 - 2.25)"), -1.0);
    approx(scalar("FLOOR(x.i)"), 9.0);
}

/// `SIGN(-0.0)` is zero. `f64::signum` says `-1.0`, which is a fact about
/// the float representation and not about the sign of the number.
#[test]
fn sign_of_negative_zero_is_zero() {
    approx(scalar("SIGN(0.0 - 0.0)"), 0.0);
}

/// Null in, null out. A missing property reads as null, and `sqrt` of it
/// is unknown rather than wrong.
#[test]
fn null_propagates() {
    assert_eq!(scalar("SQRT(null)"), Value::Null);
    assert_eq!(scalar("POW(null, 2)"), Value::Null);
    assert_eq!(scalar("POW(2, null)"), Value::Null);
    assert_eq!(scalar("SQRT(x.missing)"), Value::Null);
}

/// A result outside the reals is null, the answer 3VL already gives a
/// division by zero — not a NaN this engine would then have to compare.
#[test]
fn a_non_real_result_is_null() {
    assert_eq!(scalar("SQRT(0.0 - 1.0)"), Value::Null);
    assert_eq!(scalar("LN(0.0)"), Value::Null);
}

/// A provably non-numeric argument is a type error, and null does not
/// rescue it: no value `str | NULL` admits is a number.
///
/// Checked against the graph's own inferred schema, because that is what
/// makes the argument's type provable — with no schema, `x.s` is `Star`
/// and the gradual rule lets it through on purpose.
#[test]
fn a_non_numeric_argument_is_a_type_error() {
    let g = graph();
    let schema = frogql::typing::inference::infer_simple_schema(&g);
    let e = frogql::compile_query_with(&schema, "MATCH (x:N) RETURN SQRT(x.s)")
        .expect_err("a string argument must be rejected");
    assert!(
        e.contains("SQRT") && e.contains("numeric"),
        "the message must name the function: {e}"
    );
    // And the numeric one still passes against the same schema, so the
    // rejection is about the argument and not about the schema.
    assert!(frogql::compile_query_with(&schema, "MATCH (x:N) RETURN SQRT(x.i)").is_ok());
}

/// Wrong arity is reported against the function, not against a
/// parenthesis. The message in the original report pointed at `(`, which
/// is what made the missing function hard to recognise.
#[test]
fn wrong_arity_names_the_function() {
    let e =
        frogql::compile_query("MATCH (x:N) RETURN ABS(1, 2)").expect_err("ABS takes one argument");
    assert!(e.contains("ABS"), "the message must name the function: {e}");
    let e = frogql::compile_query("MATCH (x:N) RETURN POW(2)").expect_err("POW takes two");
    assert!(e.contains("POW"), "the message must name the function: {e}");
}

/// Soft keywords: only the call form is special. `abs`, `round`, `sign`
/// and `degrees` are the words a real schema uses for columns, so they
/// have to keep working as variables, labels and property names.
#[test]
fn the_names_stay_usable_as_identifiers() {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["round"], "props": {"abs": 3, "sign": "s", "degrees": 90}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let rt = Runtime::new(&g);
    let q = frogql::compile_query(
        "MATCH (sqrt:round) WHERE sqrt.abs = 3 RETURN sqrt.degrees AS pow, ABS(sqrt.abs) AS a",
    )
    .expect("these names must still parse as identifiers");
    match rt.run_query(&q, 0) {
        QueryResult::Projected(r) => {
            assert_eq!(r.len(), 1);
            assert_eq!(r[0][0], Value::Int(90));
            assert_eq!(r[0][1], Value::Int(3));
        }
        other => panic!("expected a projection, got {other:?}"),
    }
}

/// They compose with arithmetic, aggregates and ORDER BY, which is the
/// whole point — the issue's example is a distance ranking.
#[test]
fn they_compose_with_the_rest_of_the_language() {
    let r = rows("MATCH (x:N) RETURN x.i, SQRT(POW(x.i, 2) + POW(x.f, 2)) AS d ORDER BY d DESC");
    assert_eq!(r.len(), 2);
    // 16 sorts above 9.
    assert_eq!(r[0][0], Value::Int(16));
    let r = rows("MATCH (x:N) RETURN SUM(ABS(x.neg)) AS total");
    assert_eq!(r[0][0], Value::Int(5));
}
