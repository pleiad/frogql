//! ISO §22.14 ordering comparisons: `<`, `<=`, `>`, `>=`.
//!
//! Ordering used to be restricted to numbers in two of the three places
//! that decide it, and not in the third. `cmp_values` — the pushed-down
//! path, and the one `ORDER BY` uses — ordered strings and booleans;
//! `BinOp::delta` and `eval_binop` rejected them. So
//!
//!     MATCH (a:Person), (b:Person) WHERE a.name < b.name
//!
//! the standard way to keep one of each unordered pair, typed as ⊥ and
//! returned **zero rows in silence**, while `ORDER BY a.name` right
//! beside it sorted the same values fine.
//!
//! What is pinned here: the orderable types agree across the three
//! paths, a pair with no common domain is still rejected, and the
//! composite types ISO makes equality-comparable-only stay that way.

use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::typing::inference::infer_simple_schema;
use frogql::typing::variable_type::Schema;

fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["P"], "props": {"name": "ada", "n": 1, "f": 1.5, "b": false, "xs": [1]}},
        {"id": "b", "labels": ["P"], "props": {"name": "bob", "n": 2, "f": 0.5, "b": true, "xs": [2]}}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn rows_with(schema: &Schema, q: &str) -> Vec<Vec<Value>> {
    let g = graph();
    let rt = Runtime::new(&g);
    let query =
        frogql::compile_query_with(schema, q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
    match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("expected a projection, got {other:?}"),
    }
}

fn schema() -> Schema {
    infer_simple_schema(&graph())
}

/// The case from the report: a string comparison between two bound
/// variables. One row of the two-element self-join survives.
#[test]
fn strings_order_against_each_other() {
    let r = rows_with(
        &schema(),
        "MATCH (a:P), (b:P) WHERE a.name < b.name RETURN a.name, b.name",
    );
    assert_eq!(r.len(), 1, "exactly one of the two orderings holds");
    assert_eq!(r[0][0], Value::Str("ada".into()));
    assert_eq!(r[0][1], Value::Str("bob".into()));
}

/// Against a literal, and through `>=` as well, so the fix is not a
/// special case of one operator.
#[test]
fn strings_order_against_a_literal() {
    let s = schema();
    assert_eq!(
        rows_with(&s, "MATCH (a:P) WHERE a.name < 'b' RETURN a.name").len(),
        1
    );
    assert_eq!(
        rows_with(&s, "MATCH (a:P) WHERE a.name >= 'b' RETURN a.name").len(),
        1
    );
}

/// Booleans are ordered too (`false < true`), which ISO §22.14 says and
/// `cmp_values` has always implemented.
#[test]
fn booleans_order() {
    let r = rows_with(&schema(), "MATCH (a:P) WHERE a.b > false RETURN a.name");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0][0], Value::Str("bob".into()));
}

/// Numbers still compare across the int/float split, the same widening
/// `cmp_values` applies.
#[test]
fn numbers_still_widen() {
    let r = rows_with(&schema(), "MATCH (a:P) WHERE a.n > a.f RETURN a.name");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0][0], Value::Str("bob".into()));
}

/// Widening the domain must not make everything comparable: two types
/// with no common domain are still a type error, reported rather than
/// silently `false`.
#[test]
fn a_pair_with_no_common_domain_is_rejected() {
    let s = schema();
    for q in [
        "MATCH (a:P) WHERE a.name < a.n RETURN a.name",
        "MATCH (a:P) WHERE 1 < 'a' RETURN a.name",
    ] {
        let r = frogql::compile_query_with_diagnostics_with(&s, q)
            .unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
        assert!(
            r.warnings.iter().any(|w| w.contains("is not defined")),
            "`{q}` must report an undefined comparison, got {:?}",
            r.warnings
        );
    }
}

/// ISO §4.4.4: lists and records are equality-comparable, not
/// ordering-comparable. Widening ordering to strings must not have
/// widened it to composites.
#[test]
fn composites_are_not_ordered() {
    let s = schema();
    let r = frogql::compile_query_with_diagnostics_with(
        &s,
        "MATCH (a:P) WHERE a.xs < a.xs RETURN a.name",
    )
    .expect("compiles");
    assert!(
        r.warnings.iter().any(|w| w.contains("is not defined")),
        "a list comparison must be reported, got {:?}",
        r.warnings
    );
}

/// Ordering propagates null like every other comparison: unknown, which
/// drops the row rather than raising.
#[test]
fn null_propagates_through_ordering() {
    let r = rows_with(
        &schema(),
        "MATCH (a:P) RETURN a.name, a.name < null AS c ORDER BY a.name",
    );
    assert_eq!(r.len(), 2);
    assert_eq!(r[0][1], Value::Null);
}

/// The pushed-down and residual paths must agree. A predicate the
/// optimizer can fold into the index and the same predicate it cannot
/// have to keep or drop the same rows — that they disagreed is what made
/// the original bug invisible.
#[test]
fn the_two_comparison_paths_agree() {
    let s = schema();
    // `a.name < 'b'` is a pushdown candidate (var vs literal); the
    // var-vs-var form below is not.
    let pushed = rows_with(&s, "MATCH (a:P) WHERE a.name < 'b' RETURN a.name");
    let residual = rows_with(
        &s,
        "MATCH (a:P), (b:P) WHERE b.name = 'bob' AND a.name < b.name RETURN a.name",
    );
    assert_eq!(pushed, residual);
}
