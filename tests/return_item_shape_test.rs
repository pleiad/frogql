//! Characterization of every behaviour that depended on RETURN carrying a
//! *bare* aggregate as its own AST variant.
//!
//! `SUM(x)` and `SUM(x) + 0` used to parse into different `ReturnItem`
//! variants, and roughly twenty sites had to re-unify them (`is_aggregate()`,
//! `ReturnItem::Aggregate { .. } => true, Expr => expr.contains_agg()`). The
//! split was backward compatibility from when aggregates first moved into the
//! expression grammar, not a semantic distinction — and keeping the two arms
//! in sync is what #95 failed at.
//!
//! These tests pin what must survive collapsing the variant into
//! `ReturnItem::Expr { expr: Expr::Agg(..) }`. They passed before the
//! refactor and must pass after it; the ORDER BY matcher (which compared
//! `Aggregator`s structurally against RETURN items) is the sharp edge.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// Three people in two cities, so a grouped count has more than one group
/// and an ordering over it is observable.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["P"], "props": {"name": "ana",  "city": "north", "n": 3}},
        {"id": "b", "labels": ["P"], "props": {"name": "ben",  "city": "south", "n": 1}},
        {"id": "c", "labels": ["P"], "props": {"name": "cleo", "city": "south", "n": 2}}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run(q: &str) -> Vec<Vec<Value>> {
    let g = graph();
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile {q:?}: {e}"));
    match Runtime::new(&g).run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected result for {q:?}, got {other:?}"),
    }
}

// --- ORDER BY over a bare aggregate: the sharp edge ---

#[test]
fn order_by_a_direct_aggregate_matching_a_return_item() {
    // The sort key is written as the aggregate itself, not as an alias, and
    // must resolve to the projected column that holds it.
    let rows = run("MATCH (p: P) RETURN p.city AS city, COUNT(*) \
         GROUP BY p.city ORDER BY COUNT(*) DESC");
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("south".into()), Value::Int(2)],
            vec![Value::Str("north".into()), Value::Int(1)],
        ]
    );
}

#[test]
fn order_by_the_alias_of_a_bare_aggregate() {
    let rows = run("MATCH (p: P) RETURN p.city AS city, COUNT(*) AS c \
         GROUP BY p.city ORDER BY c ASC");
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("north".into()), Value::Int(1)],
            vec![Value::Str("south".into()), Value::Int(2)],
        ]
    );
}

#[test]
fn order_by_a_direct_aggregate_absent_from_return_is_an_error() {
    let err = compile_query(
        "MATCH (p: P) RETURN p.city AS city, COUNT(*) GROUP BY p.city ORDER BY SUM(p.n)",
    )
    .expect_err("a direct aggregate sort key not projected must be rejected");
    assert!(
        err.contains("not in the RETURN list"),
        "expected the direct-aggregate diagnostic, got: {err}"
    );
}

#[test]
fn order_by_a_bare_aggregate_is_typed_by_its_result() {
    // The sort key points at a projected aggregate whose result is a list,
    // which is not orderable. Typing it through the reducer's result type is
    // what catches this — laundering it through `Star` would not.
    assert!(
        compile_query(
            "MATCH (p: P) RETURN p.city AS city, COLLECT_LIST(p.n) AS ns \
             GROUP BY p.city ORDER BY ns"
        )
        .is_err(),
        "ORDER BY over a list-valued aggregate must be rejected"
    );
}

// --- Grouping: arity and keys ---

#[test]
fn bare_aggregate_is_not_a_grouping_key() {
    // With no explicit GROUP BY, the keys are the non-aggregate items. The
    // bare COUNT(*) must not become one, or every row would be its own group.
    let rows = run("MATCH (p: P) RETURN COUNT(*)");
    assert_eq!(rows, vec![vec![Value::Int(3)]]);
}

#[test]
fn bare_aggregate_mixed_with_a_plain_item_still_needs_group_by() {
    assert!(compile_query("MATCH (p: P) RETURN p.city, COUNT(*)").is_err());
}

#[test]
fn bare_aggregate_is_exempt_from_the_functional_dependency_check() {
    // `SUM(p.n)` is not determined by `p.city`, and must be exempt anyway
    // because it is reduced over the group rather than read off a row.
    let rows =
        run("MATCH (p: P) RETURN p.city AS city, SUM(p.n) AS total GROUP BY p.city ORDER BY city");
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("north".into()), Value::Int(3)],
            vec![Value::Str("south".into()), Value::Int(3)],
        ]
    );
}

// --- The two forms must agree, which is the whole point ---

#[test]
fn a_bare_aggregate_and_the_same_one_in_arithmetic_agree() {
    let bare = run("MATCH (p: P) RETURN SUM(p.n) AS total");
    let wrapped = run("MATCH (p: P) RETURN SUM(p.n) + 0 AS total");
    assert_eq!(bare, wrapped);
    assert_eq!(bare, vec![vec![Value::Int(6)]]);
}

#[test]
fn distinct_inside_a_bare_aggregate_survives() {
    // Two people share city "south"; DISTINCT collapses them.
    let rows = run("MATCH (p: P) RETURN COUNT(DISTINCT p.city) AS cities");
    assert_eq!(rows, vec![vec![Value::Int(2)]]);
}

#[test]
fn count_star_keeps_its_own_semantics() {
    // COUNT(*) is cardinality: no null elimination, no argument to evaluate.
    let rows = run("MATCH (p: P) RETURN COUNT(*) AS c");
    assert_eq!(rows, vec![vec![Value::Int(3)]]);
}

// --- A bare aggregate as the single item of a subquery body ---

#[test]
fn bare_aggregate_inside_a_value_subquery_body() {
    let rows = run("MATCH (p: P) RETURN p.name AS name, \
         VALUE { MATCH (q: P) WHERE q.city = p.city RETURN COUNT(*) } AS peers \
         ORDER BY name");
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("ana".into()), Value::Int(1)],
            vec![Value::Str("ben".into()), Value::Int(2)],
            vec![Value::Str("cleo".into()), Value::Int(2)],
        ]
    );
}

// --- An unaliased aggregate column keeps its positional identity ---

#[test]
fn an_unaliased_bare_aggregate_has_no_alias() {
    // The bindings name unaliased columns `col0`, `col1`, … off `alias()`
    // returning None. Projection order and arity must not shift.
    let rows = run("MATCH (p: P) RETURN p.city AS city, COUNT(*) GROUP BY p.city ORDER BY city");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].len(), 2);
}
