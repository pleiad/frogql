//! Tests for `ORDER BY` (ISO/IEC 39075:2024 §14.9 + §16.17). Covers
//! parser surface, sort directions, multi-key tuple comparison,
//! NULLS FIRST/LAST overrides (Feature GA03), the implementation
//! default (NULLS LAST regardless of direction per §16.17 SR 6), and
//! interactions with RETURN, DISTINCT, LIMIT, and queries that have
//! no RETURN at all.

use gqlrust::compile_query;
use gqlrust::compile_query_unchecked;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::syntax::query::{NullsOrder, SortDir};

/// Five users with varying `age` properties; one is missing `age`
/// entirely so the engine's implicit-null path kicks in. Designed for
/// testing every direction × null-ordering combination.
fn graph_with_users() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Alice", "age": 30}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Bob",   "age": 25}},
        {"id": "u3", "labels": ["User"], "props": {"name": "Carol", "age": 40}},
        {"id": "u4", "labels": ["User"], "props": {"name": "Dave",  "age": 25}},
        {"id": "u5", "labels": ["User"], "props": {"name": "Eve"}}
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
        _ => panic!("expected projected result for {q:?}"),
    }
}

// =====================================================================
// Parser surface (no runtime needed)
// =====================================================================

#[test]
fn parser_order_by_default_is_asc_no_nulls() {
    let q = compile_query_unchecked("MATCH (x) RETURN x.name ORDER BY x.name").unwrap();
    let specs = q.order_by.expect("order_by must be parsed");
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].dir, SortDir::Asc);
    assert_eq!(specs[0].nulls, None);
}

#[test]
fn parser_order_by_desc() {
    let q = compile_query_unchecked("MATCH (x) RETURN x.name ORDER BY x.name DESC").unwrap();
    let specs = q.order_by.unwrap();
    assert_eq!(specs[0].dir, SortDir::Desc);
}

#[test]
fn parser_order_by_long_forms() {
    let q1 = compile_query_unchecked("MATCH (x) RETURN x.name ORDER BY x.name ASCENDING").unwrap();
    let q2 = compile_query_unchecked("MATCH (x) RETURN x.name ORDER BY x.name DESCENDING").unwrap();
    assert_eq!(q1.order_by.unwrap()[0].dir, SortDir::Asc);
    assert_eq!(q2.order_by.unwrap()[0].dir, SortDir::Desc);
}

#[test]
fn parser_order_by_multi_key_with_mixed_directions() {
    let q =
        compile_query_unchecked("MATCH (x) RETURN x.name ORDER BY x.age DESC, x.name ASC").unwrap();
    let specs = q.order_by.unwrap();
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].dir, SortDir::Desc);
    assert_eq!(specs[1].dir, SortDir::Asc);
}

#[test]
fn parser_order_by_nulls_first_and_last() {
    let q1 = compile_query_unchecked("MATCH (x) RETURN x.name ORDER BY x.age NULLS FIRST").unwrap();
    let q2 =
        compile_query_unchecked("MATCH (x) RETURN x.name ORDER BY x.age DESC NULLS LAST").unwrap();
    assert_eq!(q1.order_by.unwrap()[0].nulls, Some(NullsOrder::First));
    let s2 = q2.order_by.unwrap();
    assert_eq!(s2[0].dir, SortDir::Desc);
    assert_eq!(s2[0].nulls, Some(NullsOrder::Last));
}

#[test]
fn parser_order_by_with_limit() {
    let q = compile_query_unchecked("MATCH (x) RETURN x.name ORDER BY x.age DESC LIMIT 3").unwrap();
    assert!(q.order_by.is_some());
    assert_eq!(q.limit, Some(3));
}

#[test]
fn parser_order_by_without_return_is_legal() {
    // ISO §14.9 allows ORDER BY as a standalone statement (no RETURN).
    let q = compile_query_unchecked("MATCH (x) ORDER BY x.age").unwrap();
    assert!(q.order_by.is_some());
    assert!(q.returns.is_none());
}

#[test]
fn parser_order_by_after_limit_is_error() {
    // §14.9 SR: ORDER BY must come BEFORE LIMIT. Reversing parses as a
    // trailing token after LIMIT, which the top-level parser rejects.
    let r = compile_query_unchecked("MATCH (x) RETURN x.name LIMIT 5 ORDER BY x.name");
    assert!(r.is_err(), "expected error, got {r:?}");
}

#[test]
fn parser_lowercase_order_by_works() {
    let q = compile_query_unchecked("MATCH (x) RETURN x.name order by x.name desc").unwrap();
    let specs = q.order_by.unwrap();
    assert_eq!(specs[0].dir, SortDir::Desc);
}

#[test]
fn parser_order_alone_without_by_does_not_consume_keyword() {
    // `order` should remain a usable identifier when not followed by
    // `by`. `(x: order)` would be a label name; we test the property
    // path that depends on `Name(\"order\")` being preserved.
    let q = compile_query_unchecked("MATCH (x) WHERE x.order = 1 RETURN x.name");
    assert!(q.is_ok(), "got: {q:?}");
}

// =====================================================================
// Runtime — single-key sort
// =====================================================================

#[test]
fn runtime_asc_default_orders_ascending_with_nulls_last() {
    // Ages: Alice=30, Bob=25, Carol=40, Dave=25, Eve=null. ASC default
    // → 25, 25, 30, 40, null. Bob and Dave are peers; under pdqsort
    // their relative order is implementation-dependent (§16.17 GR 1k
    // / US006). The test asserts only the age column to stay valid
    // regardless of which peer comes first.
    let g = graph_with_users();
    let rows = run_projected(&g, "MATCH (x: User) RETURN x.name, x.age ORDER BY x.age");
    assert_eq!(rows.len(), 5);
    let ages: Vec<&Value> = rows.iter().map(|r| &r[1]).collect();
    assert_eq!(
        ages,
        vec![
            &Value::Int(25),
            &Value::Int(25),
            &Value::Int(30),
            &Value::Int(40),
            &Value::Null, // NULLS LAST is gqlite's IS001 default
        ]
    );
}

#[test]
fn runtime_desc_orders_descending_with_nulls_last_default() {
    // Per ISO §16.17 SR 6 the default null ordering does NOT depend on
    // ASC/DESC. gqlite picks NULLS LAST always, so DESC still puts the
    // null at the end (different from PostgreSQL's NULLS FIRST default
    // for DESC — see bitácora 05 for the rationale).
    let g = graph_with_users();
    let rows = run_projected(
        &g,
        "MATCH (x: User) RETURN x.name, x.age ORDER BY x.age DESC",
    );
    let ages: Vec<&Value> = rows.iter().map(|r| &r[1]).collect();
    assert_eq!(
        ages,
        vec![
            &Value::Int(40),
            &Value::Int(30),
            &Value::Int(25),
            &Value::Int(25),
            &Value::Null,
        ]
    );
}

#[test]
fn runtime_explicit_nulls_first_overrides_default() {
    let g = graph_with_users();
    let rows = run_projected(
        &g,
        "MATCH (x: User) RETURN x.name, x.age ORDER BY x.age NULLS FIRST",
    );
    assert_eq!(rows[0][1], Value::Null);
}

#[test]
fn runtime_explicit_nulls_last_matches_default() {
    let g = graph_with_users();
    let rows = run_projected(
        &g,
        "MATCH (x: User) RETURN x.name, x.age ORDER BY x.age NULLS LAST",
    );
    assert_eq!(rows[rows.len() - 1][1], Value::Null);
}

#[test]
fn runtime_string_ordering_is_lexicographic() {
    let g = graph_with_users();
    let rows = run_projected(&g, "MATCH (x: User) RETURN x.name ORDER BY x.name");
    let names: Vec<String> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            v => panic!("expected str, got {v:?}"),
        })
        .collect();
    assert_eq!(names, vec!["Alice", "Bob", "Carol", "Dave", "Eve"]);
}

// =====================================================================
// Runtime — multi-key sort
// =====================================================================

#[test]
fn runtime_multi_key_lexicographic() {
    // Sort by age DESC, then name ASC. Bob (25) and Dave (25) are tied
    // on age, so name resolves the tie: Bob before Dave.
    let g = graph_with_users();
    let rows = run_projected(
        &g,
        "MATCH (x: User) RETURN x.name, x.age ORDER BY x.age DESC, x.name ASC",
    );
    let names: Vec<String> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            v => panic!("expected str, got {v:?}"),
        })
        .collect();
    // Carol(40), Alice(30), Bob(25), Dave(25), Eve(null).
    assert_eq!(names, vec!["Carol", "Alice", "Bob", "Dave", "Eve"]);
}

#[test]
fn runtime_multi_key_resolves_ties_in_order() {
    // age ASC, then name DESC: ties on 25 broken by name DESC.
    let g = graph_with_users();
    let rows = run_projected(
        &g,
        "MATCH (x: User) RETURN x.name ORDER BY x.age ASC, x.name DESC",
    );
    let names: Vec<String> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            v => panic!("expected str, got {v:?}"),
        })
        .collect();
    // 25→Dave,Bob (DESC), 30→Alice, 40→Carol, null→Eve (NULLS LAST).
    assert_eq!(names, vec!["Dave", "Bob", "Alice", "Carol", "Eve"]);
}

// =====================================================================
// Runtime — interaction with LIMIT, DISTINCT, no-RETURN
// =====================================================================

#[test]
fn runtime_order_by_with_limit_yields_top_n() {
    let g = graph_with_users();
    let rows = run_projected(
        &g,
        "MATCH (x: User) RETURN x.name ORDER BY x.age DESC LIMIT 2",
    );
    assert_eq!(rows.len(), 2);
    let names: Vec<String> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            v => panic!("expected str, got {v:?}"),
        })
        .collect();
    assert_eq!(names, vec!["Carol", "Alice"]);
}

#[test]
fn runtime_order_by_with_distinct() {
    // Distinct ages, ascending. Two rows with age 25 collapse to one.
    let g = graph_with_users();
    let rows = run_projected(&g, "MATCH (x: User) RETURN DISTINCT x.age ORDER BY x.age");
    assert_eq!(rows.len(), 4);
    let ages: Vec<&Value> = rows.iter().map(|r| &r[0]).collect();
    assert_eq!(
        ages,
        vec![
            &Value::Int(25),
            &Value::Int(30),
            &Value::Int(40),
            &Value::Null,
        ]
    );
}

#[test]
fn runtime_order_by_without_return_sorts_raw_table() {
    // ISO §14.9 standalone form: ORDER BY can sort the raw binding
    // table when no RETURN is present. Verifies that the row count is
    // preserved and the limit (if any) is applied after sorting.
    let g = graph_with_users();
    let rt = Runtime::new(&g);
    let q = compile_query("MATCH (x: User) ORDER BY x.age LIMIT 3").unwrap();
    match rt.run_query(&q, 0) {
        QueryResult::Raw(ir) => assert_eq!(ir.rows.len(), 3),
        _ => panic!("expected Raw result for query without RETURN"),
    }
}

#[test]
fn runtime_order_by_deterministic_across_runs() {
    // Two users with age=25 (Bob, Dave). Per ISO §16.17 GR 1k / US006
    // the relative order of peers is implementation-dependent — gqlite
    // uses pdqsort (`sort_unstable_by`), which does not preserve input
    // order for peer rows but IS deterministic for a given input
    // vector. Two runs of the same query against the same graph must
    // therefore produce byte-identical output.
    let g = graph_with_users();
    let rows1 = run_projected(&g, "MATCH (x: User) RETURN x.name, x.age ORDER BY x.age");
    let rows2 = run_projected(&g, "MATCH (x: User) RETURN x.name, x.age ORDER BY x.age");
    assert_eq!(rows1, rows2, "sort must be deterministic across runs");
}

// =====================================================================
// Edge cases
// =====================================================================

#[test]
fn runtime_order_by_on_empty_input_is_noop() {
    let g = graph_with_users();
    let rows = run_projected(
        &g,
        "MATCH (x: User) WHERE x.name = 'Nobody' RETURN x.name ORDER BY x.name",
    );
    assert!(rows.is_empty());
}

#[test]
fn runtime_order_by_treats_failure_as_null() {
    // `x.nonexistent` raises ExprResult::Failure for every row → all
    // rows have null in this position → ORDER BY does not change the
    // order beyond the secondary key. Exercise: with a secondary key
    // present, the failure key acts as Equal across all rows.
    let g = graph_with_users();
    let rows = run_projected(
        &g,
        "MATCH (x: User) RETURN x.name ORDER BY x.nonexistent ASC, x.name ASC",
    );
    let names: Vec<String> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            v => panic!("expected str, got {v:?}"),
        })
        .collect();
    // Primary key is null for all; secondary key (name ASC) decides.
    assert_eq!(names, vec!["Alice", "Bob", "Carol", "Dave", "Eve"]);
}

#[test]
fn runtime_order_by_cross_kind_falls_back_to_equal() {
    // Mixed Int/Str in the same column — `value_cmp` returns None, our
    // total-order fallback is Equal, so input order is preserved
    // (stable sort). This is the implementation choice for ISO US007.
    let json = r#"{
      "nodes": [
        {"id": "n1", "labels": ["X"], "props": {"v": 1}},
        {"id": "n2", "labels": ["X"], "props": {"v": "hello"}},
        {"id": "n3", "labels": ["X"], "props": {"v": 2}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let rows = run_projected(&g, "MATCH (x: X) RETURN x.v ORDER BY x.v");
    // All three rows preserved; Int vs Str returns None from value_cmp,
    // so they are treated as Equal and the stable sort keeps original
    // input order. We pin the row count, not exact order — the latter
    // depends on json node iteration order.
    assert_eq!(rows.len(), 3);
}

#[test]
fn runtime_order_by_int_float_promote() {
    // Cross-numeric (Int vs Float) DOES compare via value_cmp's
    // promotion rules. 2.5 sorts between 2 and 3.
    let json = r#"{
      "nodes": [
        {"id": "n1", "labels": ["X"], "props": {"v": 3}},
        {"id": "n2", "labels": ["X"], "props": {"v": 2.5}},
        {"id": "n3", "labels": ["X"], "props": {"v": 2}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let rows = run_projected(&g, "MATCH (x: X) RETURN x.v ORDER BY x.v");
    assert_eq!(
        rows,
        vec![
            vec![Value::Int(2)],
            vec![Value::Float(2.5)],
            vec![Value::Int(3)],
        ]
    );
}
