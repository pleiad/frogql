//! Runtime evaluation of `EXISTS` and `NOT EXISTS`.
//!
//! Two regimes share the cache on `Runtime`:
//!   - Uncorrelated bodies — single `limit=1` run, cached as a bool.
//!   - Correlated bodies — single full run, projected onto the
//!     correlation variables, cached as a `HashSet`. Per outer row
//!     the predicate is one O(1) hash probe (semi/anti-join).
//!
//! All queries here use the permissive `compile_query` path (Star
//! schema) so the optimiser cannot pre-fold the predicates — every
//! verdict comes from the runtime.

use std::path::Path;

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

fn fraud_graph() -> MemoryGraphStore {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    MemoryGraphStore::from_file(&p).unwrap()
}

fn run_projected(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected result for {q:?}"),
    }
}

fn run_raw_count(g: &MemoryGraphStore, q: &str) -> usize {
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
fn correlated_exists_keeps_outer_rows_with_match() {
    // fraud.json wires every Account into a 4-cycle of Transfer
    // edges (a1 → p1 → p2 → a2 → a1), so every Account has at least
    // one outgoing Transfer. EXISTS evaluates to true on every
    // outer row.
    let g = fraud_graph();
    let count = run_raw_count(
        &g,
        "MATCH (x:Account) WHERE EXISTS { (x)-[:Transfer]->(y) }",
    );
    let total = run_raw_count(&g, "MATCH (x:Account)");
    assert_eq!(count, total);
}

#[test]
fn correlated_not_exists_drops_outer_rows_with_match() {
    // Symmetric: every Account has an outgoing Transfer, so
    // NOT EXISTS evaluates to false everywhere.
    let g = fraud_graph();
    let count = run_raw_count(
        &g,
        "MATCH (x:Account) WHERE NOT EXISTS { (x)-[:Transfer]->(y) }",
    );
    assert_eq!(count, 0);
}

#[test]
fn correlated_exists_partitions_outer_rows() {
    // Only `a1` has an outgoing `Foo` edge in fraud.json, so the
    // semi-join keeps exactly one Account row. The matching anti-
    // join keeps the other three.
    let g = fraud_graph();
    let with_foo = run_projected(
        &g,
        "MATCH (x:Account) WHERE EXISTS { (x)-[:Foo]->(y) } RETURN x.owner",
    );
    let without_foo = run_projected(
        &g,
        "MATCH (x:Account) WHERE NOT EXISTS { (x)-[:Foo]->(y) } RETURN x.owner",
    );
    assert_eq!(with_foo.len(), 1);
    assert_eq!(without_foo.len(), 3);
    assert_eq!(with_foo[0][0], Value::Str("Aretha".into()));
    let mut owners: Vec<&Value> = without_foo.iter().map(|r| &r[0]).collect();
    owners.sort_by_key(|v| match v {
        Value::Str(s) => s.clone(),
        _ => String::new(),
    });
    assert_eq!(
        owners,
        vec![
            &Value::Str("Jay".into()),
            &Value::Str("Mike".into()),
            &Value::Str("Scott".into()),
        ]
    );
}

#[test]
fn correlated_exists_with_inner_where() {
    // Body filters on a property of the correlated variable. Only
    // accounts that send transfers to a Person-labelled target
    // qualify — that's just `a1 -> d1` via Foo, but Transfer edges
    // never end at d1, so this should return zero accounts.
    let g = fraud_graph();
    let count = run_raw_count(
        &g,
        "MATCH (x:Account) WHERE EXISTS { (x)-[:Transfer]->(y:Person) }",
    );
    assert_eq!(count, 0);
}

#[test]
fn exists_cache_does_not_leak_across_run_query_calls() {
    // Regression: the per-Runtime exists_cache is keyed by the body's
    // heap address, which is only unique while the AST is alive. A
    // long-lived Runtime (REPL, benches, embedded callers) reuses
    // freed addresses across queries, so a stale entry from a prior
    // body would silently satisfy the next EXISTS probe. The runtime
    // clears the cache at the top of every public entry; this test
    // exercises a single Runtime across two queries whose bodies
    // collide in correlation shape (`[x]`) but disagree in body
    // content. Each verdict must reflect its own body, not the prior
    // one.
    let g = fraud_graph();
    let rt = Runtime::new(&g);

    // First query: every Account has an outgoing Transfer, so the
    // semi-join keeps every row. Populates the cache for body
    // `(x)-[:Transfer]->(y)` with the full Account set.
    let q1 = compile_query("MATCH (x:Account) WHERE EXISTS { (x)-[:Transfer]->(y) }").unwrap();
    let n1 = match rt.run_query(&q1, 0) {
        QueryResult::Raw(ir) => ir.rows.len(),
        _ => panic!("raw expected"),
    };
    let total = run_raw_count(&g, "MATCH (x:Account)");
    assert_eq!(n1, total);

    // Second query, same Runtime: Transfer edges never end at a
    // Person, so the semi-join must keep zero rows. If the cache
    // leaked from q1 (heap-address collision on the body Box), the
    // prior full-Account set would wrongly satisfy this probe and
    // bring back all four Accounts.
    let q2 =
        compile_query("MATCH (x:Account) WHERE EXISTS { (x)-[:Transfer]->(y:Person) }").unwrap();
    let n2 = match rt.run_query(&q2, 0) {
        QueryResult::Raw(ir) => ir.rows.len(),
        _ => panic!("raw expected"),
    };
    assert_eq!(n2, 0);
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
