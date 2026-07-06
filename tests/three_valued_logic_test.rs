//! SQL/GQL three-valued logic (3VL) end-to-end, per the froGQL amendment to
//! FPPC approved by M. Toro:
//!
//! - `cod` degrades to bottom for the short-circuit connectives OR/AND only
//!   when BOTH operands are bottom (a bottom/null operand beside a valid Bool
//!   stays Bool).
//! - `null AS T` -> null for any target; any *other* failed cast is an error.
//! - binary/unary operators follow classic 3VL, propagating nulls.
//!
//! Plus the FPPC `Ra` read-site rule: a missing property/field reads as `ok
//! null`, so 3VL sees a null value (lenient) while genuine errors stay Failure
//! (which empties the path).
//!
//! Error-vs-null at a connective (a type error beside a dominating boolean):
//! confirmed by both the FPPC oracle (DGG monotonicity, Thm 6.8) and the ISO
//! oracle (`UA004`). A genuine type error empties the path regardless of
//! essential/inessential position — we take the strict, portable subset:
//! suppressing an inessential error is *permitted* by ISO but never *required*,
//! and never relied upon. A *null* operand stays lenient (3VL); only a genuine
//! error empties.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn run(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).expect("compile failed");
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected rows for {q:?}, got {other:?}"),
    }
}

/// One bare node (`s = "a"`, a non-numeric string) so `RETURN <expr>` gives one row.
fn one_node() -> MemoryGraphStore {
    let json = r#"{"nodes":[{"id":"n1","labels":["N"],"props":{"s":"a"}}],"edges":[]}"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// Single-cell value of `RETURN (<expr>) AS b` over one node.
fn scalar(expr: &str) -> Value {
    let g = one_node();
    let rows = run(&g, &format!("MATCH (x:N) RETURN ({expr}) AS b"));
    assert_eq!(rows.len(), 1, "expected one row for {expr:?}, got {rows:?}");
    rows.into_iter().next().unwrap().into_iter().next().unwrap()
}

/// Three users; Carol has no `email` (missing property -> null).
fn users() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id":"u1","labels":["User"],"props":{"name":"Alice","email":"a@x.test"}},
        {"id":"u2","labels":["User"],"props":{"name":"Bob","email":"b@x.test"}},
        {"id":"u3","labels":["User"],"props":{"name":"Carol"}}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// active = true / false / missing.
fn active_graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id":"a","labels":["N"],"props":{"name":"A","active":true}},
        {"id":"b","labels":["N"],"props":{"name":"B","active":false}},
        {"id":"c","labels":["N"],"props":{"name":"C"}}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn names(g: &MemoryGraphStore, q: &str) -> Vec<String> {
    let mut ns: Vec<String> = run(g, q)
        .into_iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            other => panic!("expected Str name, got {other:?}"),
        })
        .collect();
    ns.sort();
    ns
}

// ---------------------------------------------------------------------------
// Logical connectives — classic 3VL truth tables (literal null)
// ---------------------------------------------------------------------------

#[test]
fn test_or_truth_table() {
    assert_eq!(scalar("true OR null"), Value::Bool(true));
    assert_eq!(scalar("null OR true"), Value::Bool(true));
    assert_eq!(scalar("false OR null"), Value::Null);
    assert_eq!(scalar("null OR false"), Value::Null);
    assert_eq!(scalar("null OR null"), Value::Null);
    assert_eq!(scalar("true OR false"), Value::Bool(true));
    assert_eq!(scalar("false OR false"), Value::Bool(false));
}

#[test]
fn test_and_truth_table() {
    assert_eq!(scalar("false AND null"), Value::Bool(false));
    assert_eq!(scalar("null AND false"), Value::Bool(false));
    assert_eq!(scalar("true AND null"), Value::Null);
    assert_eq!(scalar("null AND true"), Value::Null);
    assert_eq!(scalar("null AND null"), Value::Null);
    assert_eq!(scalar("true AND true"), Value::Bool(true));
    assert_eq!(scalar("true AND false"), Value::Bool(false));
}

#[test]
fn test_not_truth_table() {
    assert_eq!(scalar("NOT null"), Value::Null);
    assert_eq!(scalar("NOT true"), Value::Bool(false));
    assert_eq!(scalar("NOT false"), Value::Bool(true));
}

// ---------------------------------------------------------------------------
// Read-site nulls (FPPC Ra): a missing property is null, not an error — so it
// stays lenient under the connectives. This is the headline fix.
// ---------------------------------------------------------------------------

#[test]
fn test_missing_property_reads_as_null() {
    let g = users();
    let rows = run(&g, "MATCH (x:User) WHERE x.name = 'Carol' RETURN x.email");
    assert_eq!(rows, vec![vec![Value::Null]]);
}

#[test]
fn test_missing_or_true_keeps_all_rows() {
    // `status` is absent on every user -> null -> `null OR true = true` -> all
    // rows. This is the canonical headline case (`WHERE x.status OR true`).
    let g = users();
    assert_eq!(
        names(&g, "MATCH (x:User) WHERE x.status OR true RETURN x.name"),
        vec!["Alice", "Bob", "Carol"],
    );
}

#[test]
fn test_bool_property_or_true_keeps_all_rows() {
    // `active` is a real boolean, present (true/false) or missing (-> null);
    // `x.active OR true` is true for all three.
    let g = active_graph();
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.active OR true RETURN x.name"),
        vec!["A", "B", "C"],
    );
}

#[test]
fn test_nonbool_value_beside_true_still_empties() {
    // A *present* non-bool value (a string) in boolean position is a type
    // error, not a null. It empties even beside a dominating `true` (ISO
    // permits suppressing this inessential error but never requires it — we
    // don't). Only the missing (null) row survives via `null OR true`.
    let g = users(); // Alice/Bob email = string; Carol email missing (null)
    assert_eq!(
        names(&g, "MATCH (x:User) WHERE x.email OR true RETURN x.name"),
        vec!["Carol"],
    );
}

#[test]
fn test_missing_property_is_unknown_in_or() {
    // `false OR x.active`: active=true -> true, false -> false, missing -> null.
    let g = active_graph();
    let mut rows = run(&g, "MATCH (x:N) RETURN x.name, (false OR x.active) AS b");
    rows.sort_by(|l, r| format!("{:?}", l[0]).cmp(&format!("{:?}", r[0])));
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("A".into()), Value::Bool(true)],
            vec![Value::Str("B".into()), Value::Bool(false)],
            vec![Value::Str("C".into()), Value::Null],
        ]
    );
}

#[test]
fn test_missing_and_false_short_circuits() {
    // false AND x.active = false on every row (false is absorbing).
    let g = active_graph();
    let rows = run(&g, "MATCH (x:N) RETURN (false AND x.active) AS b");
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r == &vec![Value::Bool(false)]));
}

// ---------------------------------------------------------------------------
// Comparison / equality — 3VL (null propagates)
// ---------------------------------------------------------------------------

#[test]
fn test_null_comparisons_are_null() {
    assert_eq!(scalar("null = null"), Value::Null);
    assert_eq!(scalar("null = 5"), Value::Null);
    assert_eq!(scalar("5 = null"), Value::Null);
    assert_eq!(scalar("null <> null"), Value::Null);
    assert_eq!(scalar("null < 5"), Value::Null);
    assert_eq!(scalar("5 > null"), Value::Null);
}

#[test]
fn test_non_null_comparisons_unaffected() {
    assert_eq!(scalar("1 = 1"), Value::Bool(true));
    assert_eq!(scalar("1 = 2"), Value::Bool(false));
    assert_eq!(scalar("2 > 1"), Value::Bool(true));
    assert_eq!(scalar("'a' = 'a'"), Value::Bool(true));
}

#[test]
fn test_eq_null_drops_rows_but_is_null_keeps() {
    // `= null` is never true -> empty; `IS NULL` is the way to match.
    let g = users();
    assert!(run(&g, "MATCH (x:User) WHERE x.email = null RETURN x.name").is_empty());
    assert!(run(&g, "MATCH (x:User) WHERE x.email <> null RETURN x.name").is_empty());
    assert_eq!(
        names(&g, "MATCH (x:User) WHERE x.email IS NULL RETURN x.name"),
        vec!["Carol"],
    );
    assert_eq!(
        names(&g, "MATCH (x:User) WHERE x.email IS NOT NULL RETURN x.name"),
        vec!["Alice", "Bob"],
    );
}

#[test]
fn test_where_not_drops_unknown() {
    // NOT (unknown) = unknown -> Carol (missing email) is dropped, not kept.
    let g = users();
    assert_eq!(
        names(
            &g,
            "MATCH (x:User) WHERE NOT (x.email = 'nobody@x.test') RETURN x.name",
        ),
        vec!["Alice", "Bob"],
    );
}

// ---------------------------------------------------------------------------
// Arithmetic — 3VL null propagation
// ---------------------------------------------------------------------------

#[test]
fn test_arithmetic_null_propagates() {
    assert_eq!(scalar("null + 1"), Value::Null);
    assert_eq!(scalar("1 + null"), Value::Null);
    assert_eq!(scalar("null - null"), Value::Null);
    assert_eq!(scalar("null * 3"), Value::Null);
    assert_eq!(scalar("10 / null"), Value::Null);
    assert_eq!(scalar("-null"), Value::Null);
}

// ---------------------------------------------------------------------------
// IN — 3VL membership
// ---------------------------------------------------------------------------

#[test]
fn test_in_three_valued() {
    assert_eq!(scalar("2 IN [1, 2, 3]"), Value::Bool(true));
    assert_eq!(scalar("9 IN [1, 2, 3]"), Value::Bool(false));
    // null on the left -> unknown
    assert_eq!(scalar("null IN [1, 2, 3]"), Value::Null);
    // not found, but the list carries a null -> unknown
    assert_eq!(scalar("9 IN [1, null, 3]"), Value::Null);
    // found -> true even if the list also carries a null
    assert_eq!(scalar("1 IN [1, null, 3]"), Value::Bool(true));
    // right side null -> unknown
    assert_eq!(scalar("1 IN null"), Value::Null);
}

// ---------------------------------------------------------------------------
// Cast (`as`) — null passes; genuine failures are errors, not null
// ---------------------------------------------------------------------------

#[test]
fn test_null_cast_passes_through() {
    assert_eq!(scalar("null AS INTEGER"), Value::Null);
    assert_eq!(scalar("null AS FLOAT"), Value::Null);
    assert_eq!(scalar("null AS STRING"), Value::Null);
    assert_eq!(scalar("null AS BOOL"), Value::Null);
}

#[test]
fn test_cast_convert_valid() {
    // CAST() conversion still works for real (non-null) values.
    let n = r#"{"nodes":[{"id":"n1","labels":["N"],"props":{"s":"7"}}],"edges":[]}"#;
    let g = MemoryGraphStore::from_json_str(n).unwrap();
    assert_eq!(
        run(&g, "MATCH (x:N) RETURN CAST(x.s AS INTEGER) AS i"),
        vec![vec![Value::Int(7)]],
    );
}

#[test]
fn test_null_field_access_and_missing_key() {
    // Record with an `addr` sub-record missing `zip`; and one node with no addr.
    let json = r#"{
      "nodes": [
        {"id":"a","labels":["N"],"props":{"name":"A","addr":{"city":"X"}}},
        {"id":"b","labels":["N"],"props":{"name":"B"}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    // missing sub-key -> null; missing base record -> null.field -> null.
    let mut rows = run(&g, "MATCH (x:N) RETURN x.name, x.addr.zip AS z");
    rows.sort_by(|l, r| format!("{:?}", l[0]).cmp(&format!("{:?}", r[0])));
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("A".into()), Value::Null],
            vec![Value::Str("B".into()), Value::Null],
        ]
    );
}

#[test]
fn test_null_base_field_access_is_null_not_error() {
    // Field access on a NULL base (`x.addr.zip` where node B has no `addr`, so
    // `x.addr` is null) must yield null (lenient), NOT a Failure. A projection
    // can't distinguish the two (both render as a null cell), so we test it in a
    // connective where null and error diverge: `null.field OR true` keeps the
    // row (null OR true = true); a Failure would empty it.
    let json = r#"{"nodes":[{"id":"b","labels":["N"],"props":{"name":"B"}}],"edges":[]}"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.addr.zip OR true RETURN x.name"),
        vec!["B"],
    );
    // Contrast: field access on a non-record, non-null base is a genuine error,
    // so `x.name.zip OR true` (name is a string) empties.
    assert!(run(&g, "MATCH (x:N) WHERE x.name.zip OR true RETURN x.name").is_empty());
}

// ---------------------------------------------------------------------------
// Error vs null at a connective. A genuine type error (failed cast / non-bool
// value) empties the path regardless of essential/inessential position — the
// strict, portable subset (ISO UA004 permits suppressing an inessential error
// but never requires it; FPPC DGG Thm 6.8 confirms static `B` imposes no
// runtime success obligation). A *null* operand stays lenient.
// ---------------------------------------------------------------------------

#[test]
fn test_failed_cast_beside_true_empties() {
    // `'a' AS INTEGER` is a genuine cast error (NOT null): inessential beside
    // `OR true`, but we still empty (we never rely on suppression).
    let g = one_node(); // s = "a"
    assert!(run(&g, "MATCH (x:N) WHERE (x.s AS INTEGER) OR true RETURN x.s").is_empty());
}

#[test]
fn test_nonbool_literal_error_vs_null_operand() {
    // `1` cannot be a boolean operand: `1 OR true` -> error -> empties.
    let g = one_node();
    assert!(run(&g, "MATCH (x:N) WHERE 1 OR true RETURN x.s").is_empty());
    // Contrast: a *null* operand is lenient — `null OR true` keeps the row.
    assert_eq!(
        run(&g, "MATCH (x:N) WHERE null OR true RETURN x.s").len(),
        1
    );
}

#[test]
fn test_error_empties_essential_and_inessential_alike() {
    // We treat a type error identically whether the boolean makes it
    // inessential (`_ OR true`, `false AND _`) or essential (`_ OR false`,
    // `true AND _`): all empty. (ISO mandates the essential ones; the
    // inessential ones we take the strict/portable option.)
    let g = one_node(); // s = "a" (a non-numeric string)
    for q in [
        "WHERE (x.s AS INTEGER) OR true",   // inessential
        "WHERE (x.s AS INTEGER) OR false",  // essential
        "WHERE (x.s AS INTEGER) AND false", // inessential
        "WHERE (x.s AS INTEGER) AND true",  // essential
    ] {
        assert!(
            run(&g, &format!("MATCH (x:N) {q} RETURN x.s")).is_empty(),
            "expected empty for {q:?}",
        );
    }
}

#[test]
fn test_cast_error_in_projection_is_null_cell() {
    // In a projection the error surfaces as a null cell (row kept) — the engine
    // never crashes on a type error.
    let g = one_node();
    assert_eq!(
        run(&g, "MATCH (x:N) RETURN CAST(x.s AS INTEGER) AS i"),
        vec![vec![Value::Null]],
    );
}

// ---------------------------------------------------------------------------
// Soundness of the vacuity under-approximation. We relaxed the *runtime* (null
// propagation across all binops) but only the *typechecker* for OR/AND (the cod
// override). These tests guard the one thing that asymmetry could break: the
// typechecker declaring a query `guaranteed_empty` while the runtime returns
// rows. That must never happen (static-empty must imply runtime-empty).
// ---------------------------------------------------------------------------

fn ge_and_rows(g: &MemoryGraphStore, q: &str) -> (bool, usize) {
    let res = frogql::compile_query_with_diagnostics(q).expect("compile");
    let rows = match Runtime::new(g).run_query(&res.query, 0) {
        QueryResult::Projected(r) => r.len(),
        QueryResult::Raw(ir) => ir.rows.len(),
    };
    (res.guaranteed_empty, rows)
}

#[test]
fn test_static_empty_implies_runtime_empty() {
    // Every predicate here types to bottom (via literal/type mismatch across the
    // binops we changed: arithmetic, comparison, IN, AND/OR). Wherever the
    // checker says `guaranteed_empty`, the runtime must return zero rows.
    let g = active_graph();
    for q in [
        "MATCH (x:N) WHERE 1 RETURN x.name",       // non-bool predicate
        "MATCH (x:N) WHERE 1 AND 2 RETURN x.name", // Int AND Int (both bottom)
        "MATCH (x:N) WHERE 1 OR 2 RETURN x.name",  // Int OR Int (both bottom)
        "MATCH (x:N) WHERE 'a' = 1 RETURN x.name", // incompatible comparison
        "MATCH (x:N) WHERE 'a' < 1 RETURN x.name", // incompatible ordering
        "MATCH (x:N) WHERE x.name + 1 RETURN x.name", // arithmetic in bool position
        "MATCH (x:N) WHERE 5 IN 3 RETURN x.name",  // IN non-list
    ] {
        let (ge, rows) = ge_and_rows(&g, q);
        if ge {
            assert_eq!(
                rows, 0,
                "UNSOUND: {q:?} is guaranteed_empty but returned {rows} rows"
            );
        }
    }
}

#[test]
fn test_cod_override_keeps_vacuity_sound_under_closed_schema() {
    use frogql::typing::descriptor_type::DescriptorType;
    use frogql::typing::label_type::LabelType;
    use frogql::typing::property_type::PropertyType;
    use frogql::typing::simple_type::SimpleType;
    use frogql::typing::variable_type::{Schema, VariableType};
    use std::collections::BTreeMap;

    // Closed type N { n: Int } — so an unknown property `status` types to bottom.
    let mut props = BTreeMap::new();
    props.insert("n".to_string(), SimpleType::Z);
    let schema = Schema::from_parts(
        vec![VariableType::Node(DescriptorType::new(
            LabelType::Label("N".into()),
            PropertyType::Closed(props),
        ))],
        vec![],
    );

    // `x.status OR true`: `x.status` is bottom (unknown on a closed type). WITHOUT
    // the OR cod override this would prune as guaranteed_empty; the runtime reads
    // x.status as null, so `null OR true = true` keeps every row — that mismatch
    // would be unsound. The override types it Bool, so it is NOT pruned.
    let res = frogql::compile_query_with_diagnostics_with(
        &schema,
        "MATCH (x:N) WHERE x.status OR true RETURN x.name",
    )
    .expect("compile");
    assert!(
        !res.guaranteed_empty,
        "cod override must keep `bottom OR true` non-empty to stay sound",
    );
    let g = active_graph();
    let rows = match Runtime::new(&g).run_query(&res.query, 0) {
        QueryResult::Projected(r) => r.len(),
        _ => 0,
    };
    assert_eq!(rows, 3, "runtime keeps all rows: null OR true = true");
}
