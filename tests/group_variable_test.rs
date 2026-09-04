//! ISO/IEC 39075:2024 §4.11.5 + §22.7 — group degree of reference.
//!
//! A variable declared inside a quantified path primary and referenced from
//! outside it is a *group* reference: it denotes the whole list of elements
//! bound over the repetitions, and an expression over it evaluates
//! **element-wise**, yielding a LIST (§22.7). Feature GQ17 is what allows the
//! expression to be richer than a bare variable reference, so `x.p` over a
//! group of two edges is `[1, 2]`.
//!
//! Issue #90: froGQL's typechecker already models this (`VariableType::Group(t)
//! .get_attribute(a)` is `group<T>`), but the runtime returns a `Failure` for
//! `x.p`, which `WHERE` swallows as a dropped row and `collect_aggregate_values`
//! swallows as null-elimination. Both silent.
//!
//! The chain below is the smallest graph with groups of more than one element:
//!
//! ```text
//!   (a) -[e1:R {p:1}]-> (b) -[e2:R {p:2}]-> (c)
//! ```
//!
//! Under `(s)-[x:R]->{1,2}(t)` it produces exactly three matches — two
//! one-edge groups and one two-edge group — so a per-row list result and a
//! per-row reduction are distinguishable from a cross-row aggregate.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

fn chain() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"name": "a"}},
        {"id": "b", "labels": ["N"], "props": {"name": "b"}},
        {"id": "c", "labels": ["N"], "props": {"name": "c"}}
      ],
      "edges": [
        {"id": "e1", "labels": ["R"], "props": {"p": 1},
         "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "e2", "labels": ["R"], "props": {"p": 2},
         "endpoints": ["b", "c"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile {q:?}: {e}"));
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected result for {q:?}, got {other:?}"),
    }
}

fn ints(xs: &[i64]) -> Value {
    Value::List(xs.iter().copied().map(Value::Int).collect())
}

/// Rows sorted by their debug rendering, so a test asserts on a set of rows
/// without depending on the runtime's enumeration order.
fn sorted(mut rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    rows.sort_by_key(|r| format!("{r:?}"));
    rows
}

// --- §22.7: an expression over a group evaluates element-wise to a list ---

#[test]
fn test_group_attribute_is_a_list_per_match() {
    // THE core of #90. Today every `xp` cell is `Value::Null`.
    let g = chain();
    let rows = sorted(run(
        &g,
        "MATCH (s: N)-[x: R]->{1,2}(t: N) RETURN s.name AS s, t.name AS t, x.p AS xp",
    ));
    assert_eq!(
        rows,
        sorted(vec![
            vec![Value::Str("a".into()), Value::Str("b".into()), ints(&[1])],
            vec![Value::Str("b".into()), Value::Str("c".into()), ints(&[2])],
            vec![
                Value::Str("a".into()),
                Value::Str("c".into()),
                ints(&[1, 2])
            ],
        ])
    );
}

#[test]
fn test_group_list_order_follows_the_walk() {
    // §22.7 appends per element in binding order, so the two-edge group is
    // `[1, 2]` (a→b then b→c), never `[2, 1]`.
    let g = chain();
    let rows = run(&g, "MATCH (s: N)-[x: R]->{2}(t: N) RETURN x.p AS xp");
    assert_eq!(rows, vec![vec![ints(&[1, 2])]]);
}

#[test]
fn test_quantifier_makes_a_group_even_at_one_one() {
    // The quantifier alone confers group degree, regardless of its bounds —
    // `{1,1}` is a group of one, so the result is `[1]`, not `1`.
    let g = chain();
    let rows = sorted(run(&g, "MATCH (s: N)-[x: R]->{1,1}(t: N) RETURN x.p AS xp"));
    assert_eq!(rows, sorted(vec![vec![ints(&[1])], vec![ints(&[2])]]));
}

#[test]
fn test_bare_group_variable_still_projects_the_element_list() {
    // Already works today; pinned so the fix does not regress it. The cells
    // are edge reference values, one per repetition.
    let g = chain();
    let rows = run(&g, "MATCH (s: N)-[x: R]->{2}(t: N) RETURN x AS x");
    assert_eq!(rows.len(), 1);
    match &rows[0][0] {
        Value::List(items) => {
            assert_eq!(items.len(), 2);
            assert!(items.iter().all(|v| matches!(v, Value::Edge(_))));
        }
        other => panic!("expected a list of edge references, got {other:?}"),
    }
}

#[test]
fn test_missing_attribute_is_null_per_element_not_a_dropped_row() {
    // Element-wise evaluation of an absent property gives a list of nulls
    // (the `ok null` rule applied per element), not a single null and not a
    // vanished row.
    let g = chain();
    let rows = run(&g, "MATCH (s: N)-[x: R]->{2}(t: N) RETURN x.nosuch AS xp");
    assert_eq!(
        rows,
        vec![vec![Value::List(vec![Value::Null, Value::Null])]]
    );
}

#[test]
fn test_nested_quantifier_nests_the_group() {
    // `(-[x]->{1,2}){1,2}` binds `x` to a group of groups, so the projection
    // is a list of lists — the shape `VariableType::Group(Group(_))` already
    // predicts.
    let g = chain();
    let rows = run(
        &g,
        "MATCH (s: N)((n: N)-[x: R]->{2}(m: N)){1,1}(t: N) RETURN x.p AS xp",
    );
    assert_eq!(rows, vec![vec![Value::List(vec![ints(&[1, 2])])]]);
}

// --- The singleton/group split of ISO NOTE 78 ---

#[test]
fn test_local_reference_inside_the_element_pattern_stays_singleton() {
    // Half of NOTE 78 that already works, pinned as a no-regression guard:
    // *within* the edge pattern, `x` has singleton degree, so `x.p > 1` is a
    // scalar comparison filtering each repetition. Only the b->c edge passes,
    // so the sole surviving match is that one hop. Projected via the node
    // names, so this asserts the singleton half alone and passes today.
    let g = chain();
    let rows = run(
        &g,
        "MATCH (s: N)-[x: R WHERE x.p > 1]->{1,2}(t: N) RETURN s.name AS s, t.name AS t",
    );
    assert_eq!(
        rows,
        vec![vec![Value::Str("b".into()), Value::Str("c".into())]]
    );
}

#[test]
fn test_group_reference_of_a_filtered_element_pattern() {
    // The same query read from outside: the group holds only the edges that
    // survived the local filter, so the element-wise projection is `[2]`.
    let g = chain();
    let rows = run(
        &g,
        "MATCH (s: N)-[x: R WHERE x.p > 1]->{1,2}(t: N) RETURN x.p AS xp",
    );
    assert_eq!(rows, vec![vec![ints(&[2])]]);
}

// --- Aggregates over a group reference (ISO NOTE 78's `SUM(E.P)`) ---

#[test]
fn test_sum_over_a_group_reduces_within_the_match() {
    // NOTE 78's `SUM(E.P)`: the argument is a group reference, so the sum
    // reduces the *group's* list inside each match. Three matches in, three
    // rows out — 1, 2, and 1+2. Today this collapses to a single `NULL` row.
    let g = chain();
    let rows = sorted(run(
        &g,
        "MATCH (s: N)-[x: R]->{1,2}(t: N) RETURN SUM(x.p) AS total",
    ));
    assert_eq!(
        rows,
        sorted(vec![
            vec![Value::Int(1)],
            vec![Value::Int(2)],
            vec![Value::Int(3)],
        ])
    );
}

#[test]
fn test_count_and_collect_over_a_group_reduce_within_the_match() {
    let g = chain();
    let rows = sorted(run(
        &g,
        "MATCH (s: N)-[x: R]->{1,2}(t: N) RETURN COUNT(x.p) AS n",
    ));
    assert_eq!(
        rows,
        sorted(vec![
            vec![Value::Int(1)],
            vec![Value::Int(1)],
            vec![Value::Int(2)],
        ])
    );

    let rows = sorted(run(
        &g,
        "MATCH (s: N)-[x: R]->{1,2}(t: N) RETURN COLLECT_LIST(x.p) AS ps",
    ));
    assert_eq!(
        rows,
        sorted(vec![
            vec![ints(&[1])],
            vec![ints(&[2])],
            vec![ints(&[1, 2])],
        ])
    );
}

#[test]
fn test_scalar_aggregate_still_reduces_across_rows() {
    // No-regression guard for the split above: with no quantifier, `x` is a
    // singleton, `SUM(x.p)` is the ordinary row aggregate, and the two edges
    // collapse into one row of 3.
    let g = chain();
    let rows = run(&g, "MATCH (s: N)-[x: R]->(t: N) RETURN SUM(x.p) AS total");
    assert_eq!(rows, vec![vec![Value::Int(3)]]);
}

// --- Issue #95: an element-wise aggregate beside any other RETURN item ---
//
// An element-wise aggregate reduces *within* a match, so a projection that
// contains one keeps one row per match — it does not collapse rows the way
// a row aggregate does. Two older code paths still assumed "aggregate ⇒
// collapses rows", and they were written before group variables existed:
// the typechecker's implicit-GROUP-BY rule, and `run_aggregated`'s handling
// of a bare `ReturnItem::Aggregate`.

#[test]
fn test_elementwise_aggregate_beside_another_item_needs_no_group_by() {
    // One row per match, each carrying its own group's sum. Requiring a
    // GROUP BY here would be wrong: nothing is being collapsed.
    let g = chain();
    let rows = sorted(run(
        &g,
        "MATCH (s: N)-[x: R]->{1,2}(t: N) RETURN s.name AS s, t.name AS t, SUM(x.p) AS m",
    ));
    assert_eq!(
        rows,
        sorted(vec![
            vec![
                Value::Str("a".into()),
                Value::Str("b".into()),
                Value::Int(1)
            ],
            vec![
                Value::Str("b".into()),
                Value::Str("c".into()),
                Value::Int(2)
            ],
            vec![
                Value::Str("a".into()),
                Value::Str("c".into()),
                Value::Int(3)
            ],
        ])
    );
}

#[test]
fn test_elementwise_aggregate_does_not_depend_on_arithmetic_around_it() {
    // These once took different projection paths — `SUM(x.p)` parsed to a
    // `ReturnItem::Aggregate` variant, `SUM(x.p) + 0` to a `ReturnItem::Expr`
    // — and answered `NULL` vs `3`, decided by nothing but the `+ 0`. Both
    // are plain expressions now, but the equality is worth keeping pinned.
    let g = chain();
    let bare = run(
        &g,
        "MATCH (s: N {name: 'a'})-[x: R]->{2}(t: N) RETURN t.name AS n, SUM(x.p) AS m GROUP BY t.name",
    );
    let wrapped = run(
        &g,
        "MATCH (s: N {name: 'a'})-[x: R]->{2}(t: N) RETURN t.name AS n, SUM(x.p) + 0 AS m GROUP BY t.name",
    );
    assert_eq!(bare, wrapped);
    assert_eq!(bare, vec![vec![Value::Str("c".into()), Value::Int(3)]]);
}

#[test]
fn test_path_length_and_group_sum_in_one_row() {
    // The query issue #95 was filed for: measure the path the engine found.
    // `path_length(p)` is not an aggregate and `SUM(x.p)` is element-wise,
    // so the two belong in one row with no grouping.
    let g = chain();
    let rows = run(
        &g,
        "MATCH p = SHORTEST 1 (s: N {name: 'a'})-[x: R]->*(t: N {name: 'c'}) \
         RETURN path_length(p) AS len, SUM(x.p) AS m",
    );
    assert_eq!(rows, vec![vec![Value::Int(2), Value::Int(3)]]);
}

#[test]
fn test_elementwise_count_counts_the_group_not_the_rows() {
    let g = chain();
    let rows = run(
        &g,
        "MATCH p = SHORTEST 1 (s: N {name: 'a'})-[x: R]->*(t: N {name: 'c'}) \
         RETURN path_length(p) AS len, COUNT(x) AS hops",
    );
    assert_eq!(rows, vec![vec![Value::Int(2), Value::Int(2)]]);
}

#[test]
fn test_row_aggregate_beside_another_item_still_needs_group_by() {
    // Non-regression: the implicit-grouping rule is unchanged for a real row
    // aggregate, which does collapse rows. `x` here is a singleton.
    assert!(
        frogql::compile_query("MATCH (s: N)-[x: R]->(t: N) RETURN t.name AS n, SUM(x.p) AS m")
            .is_err()
    );
    assert!(frogql::compile_query(
        "MATCH (s: N)-[x: R]->{1,2}(t: N) RETURN t.name AS n, COUNT(*) AS c"
    )
    .is_err());
}
