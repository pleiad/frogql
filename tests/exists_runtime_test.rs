//! Phase 3: runtime evaluation of `EXISTS` and `NOT EXISTS` for
//! uncorrelated subqueries (the body shares no variable with the
//! outer assignment). The body runs once with `limit=1`; the result
//! is memoized on the body's heap address so subsequent rows of the
//! outer table reuse the verdict.
//!
//! Correlated bodies (which share variables with the outer scope)
//! still fail with a clear "not yet implemented" message. Phase 4
//! will replace that with a semi/anti-join evaluator.
//!
//! All queries here use the permissive `compile_query` path (Star
//! schema) so the optimiser cannot pre-fold the predicates — every
//! verdict comes from the runtime.

use std::path::Path;

use gqlrust::compile_query;
use gqlrust::model::graph::Graph;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

fn fraud_graph() -> Graph {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    Graph::from_file(&p).unwrap()
}

fn run_projected(g: &Graph, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected result for {q:?}"),
    }
}

fn run_raw_count(g: &Graph, q: &str) -> usize {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Raw(ir) => ir.rows.len(),
        _ => panic!("expected raw result for {q:?}"),
    }
}

#[test]
fn exists_with_present_label_keeps_all_rows() {
    // fraud.json has Account nodes and at least one Person node, so
    // EXISTS { (:Person) } is true under the runtime, and the outer
    // MATCH (x:Account) returns every Account.
    let g = fraud_graph();
    let count = run_raw_count(&g, "MATCH (x:Account) WHERE EXISTS { (:Person) }");
    let expected = run_raw_count(&g, "MATCH (x:Account)");
    assert!(expected > 0, "fraud.json should have Account nodes");
    assert_eq!(count, expected);
}

#[test]
fn exists_with_absent_label_filters_all_rows() {
    // No node carries the `Phantom` label. EXISTS evaluates to false
    // and every Account row drops out of the result table. Star
    // schema means the optimiser does not pre-fold this predicate,
    // so the verdict comes from the runtime.
    let g = fraud_graph();
    let count = run_raw_count(&g, "MATCH (x:Account) WHERE EXISTS { (:Phantom) }");
    assert_eq!(count, 0);
}

#[test]
fn not_exists_with_absent_label_keeps_all_rows() {
    let g = fraud_graph();
    let count = run_raw_count(&g, "MATCH (x:Account) WHERE NOT EXISTS { (:Phantom) }");
    let expected = run_raw_count(&g, "MATCH (x:Account)");
    assert!(expected > 0);
    assert_eq!(count, expected);
}

#[test]
fn not_exists_with_present_label_filters_all_rows() {
    let g = fraud_graph();
    let count = run_raw_count(&g, "MATCH (x:Account) WHERE NOT EXISTS { (:Person) }");
    assert_eq!(count, 0);
}

#[test]
fn exists_in_return_projects_bool() {
    // Returns one row per Account, each with a single Bool column —
    // the same value for every row because the body is uncorrelated.
    let g = fraud_graph();
    let rows = run_projected(
        &g,
        "MATCH (x:Account) RETURN EXISTS { (:Person) } AS has_person",
    );
    assert!(!rows.is_empty());
    for r in &rows {
        assert_eq!(r.len(), 1);
        assert_eq!(r[0], Value::Bool(true));
    }
}

#[test]
fn exists_with_inner_where_runs() {
    // The inner WHERE ties the body to a property that exists on
    // some Account nodes. Star schema, so the optimiser does not
    // touch the predicate; the runtime runs the body once.
    let g = fraud_graph();
    let count = run_raw_count(
        &g,
        "MATCH (x:Account) WHERE EXISTS { (a:Account) WHERE a.isBlocked = true }",
    );
    let expected = run_raw_count(&g, "MATCH (x:Account)");
    // At least one fraud.json Account has isBlocked=true (Mike).
    assert_eq!(count, expected);
}

#[test]
fn correlated_exists_fails_clearly() {
    // The body references `x` which is bound by the outer match. The
    // Phase 3 evaluator cannot run this case yet; the runtime maps
    // the failure onto a dropped row (the standard convention for
    // failed expressions on a Bool predicate is "filter out").
    let g = fraud_graph();
    let count = run_raw_count(
        &g,
        "MATCH (x:Account) WHERE EXISTS { (x)-[:Transfer]->(y) }",
    );
    // Every row's predicate fails with the "not yet implemented"
    // message, so every row is filtered out. This documents the
    // current state and will flip when Phase 4 lands.
    assert_eq!(count, 0);
}

#[test]
fn nested_uncorrelated_exists() {
    // Outer EXISTS body contains another (uncorrelated) EXISTS. The
    // inner runs, then the outer runs. Both evaluate to true because
    // Person and Account both exist in the graph.
    let g = fraud_graph();
    let count = run_raw_count(
        &g,
        "MATCH (x:Account) WHERE EXISTS { \
           MATCH (a:Account) WHERE EXISTS { (:Person) } \
         }",
    );
    let expected = run_raw_count(&g, "MATCH (x:Account)");
    assert_eq!(count, expected);
}
