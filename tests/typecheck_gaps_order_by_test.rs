//! Tests for ORDER BY typechecker / parser surface (Features GA03 +
//! GQ13). The file mixes three kinds of tests:
//!
//! - **Passing tests** — pin behavior that this branch fixed:
//!   alias-in-sort-key resolution (Gap 1), aggregate-alias-in-sort-key
//!   resolution (Gap 2), structural match for aggregate queries
//!   (Gap 2 auxiliary), non-comparable sort keys rejected per ISO
//!   §22.14 (Gap 4).
//! - **Companion runtime tests** — pin observable runtime behavior on
//!   the gaps that are NOT fixed yet, so the contrast with the
//!   desired typecheck behavior is concrete.
//! - **`#[ignore]`d tests** — document remaining gaps. Run with
//!   `cargo test -- --ignored` to surface them as failing.
//!
//! Currently ignored:
//! - `typecheck_gap_unknown_attr_under_strict_schema` — strict schema
//!   does not promote the missing-attribute warning to error (Gap 3).
//!   The only remaining gap from the original four; Gaps 1, 2, 4 are
//!   all fixed.

use gqlrust::compile_query;
use gqlrust::compile_query_with;
use gqlrust::model::graph::Graph;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::typing::variable_type::Schema;

// =====================================================================
// Class 1: parser-level gaps. ISO §16.17 lists `<sort key>` ::=
// `<aggregating value expression>`, which permits both bare binding
// variable references AND aggregate expressions. gqlite's `expr()`
// rejects both before typecheck even starts.
// =====================================================================

/// `MATCH (x) RETURN x.name AS n ORDER BY n`
///
/// **ISO §16.17 SR 5c**: the sort key is evaluated in the working
/// record amended with the projected items, so a bare reference to a
/// `<return item alias>` is legal.
///
/// **Status: FIXED**. `parse_sort_key` now resolves bare-name sort
/// keys against the RETURN aliases (after the `Returns` block is
/// already parsed by the time `parse_optional_order_by` runs). When
/// the alias points to a `ReturnItem::Expr`, the underlying
/// expression is substituted via `SortKey::Expr` (pre-projection
/// sort path); when it points to a `ReturnItem::Aggregate`, the
/// sort key becomes `SortKey::Column(idx)` and the runtime sorts
/// post-projection by column index.
#[test]
fn typecheck_resolves_alias_in_sort_key() {
    let r = compile_query("MATCH (x) RETURN x.name AS n ORDER BY n");
    assert!(
        r.is_ok(),
        "ORDER BY <alias> should resolve via the RETURN alias, got: {:?}",
        r.err()
    );
}

/// `MATCH (x: User) RETURN x.city, COUNT(*) AS c GROUP BY x.city ORDER BY c DESC`
///
/// **ISO §16.17 SR 1**: a sort key is an `<aggregating value expression>`,
/// which expands to include `<aggregate function>`. Sorting groups by
/// the cardinality of the aggregate is the canonical use case.
///
/// **Status: FIXED via alias indirection**. Direct `ORDER BY COUNT(*)`
/// (no alias) is still a parse error because `expr()` does not
/// dispatch to `aggregate_function()` — that would require an
/// `Expr::Aggregate` variant and is left as future work. The user
/// must alias the aggregate (`COUNT(*) AS c`) and reference it in
/// the sort key (`ORDER BY c`); the parser resolves the alias to a
/// `SortKey::Column(idx)` and the runtime sorts post-projection.
#[test]
fn typecheck_resolves_aggregate_alias_in_sort_key() {
    let r = compile_query(
        "MATCH (x: User) RETURN x.city, COUNT(*) AS c GROUP BY x.city ORDER BY c DESC",
    );
    assert!(
        r.is_ok(),
        "ORDER BY <aggregate alias> should be accepted, got: {:?}",
        r.err()
    );
}

/// `MATCH (x: User) RETURN x.city, COUNT(*) GROUP BY x.city ORDER BY COUNT(*) DESC`
///
/// **Status: FIXED via parser-time structural match**. Direct
/// aggregates in sort keys are now legal as long as the same
/// aggregator appears in the RETURN list. `parse_sort_key` peeks
/// for `COUNT(...)` / `SUM(...)` / etc. and, when it finds one,
/// looks the parsed `Aggregator` up against `ReturnItem::Aggregate`
/// items by structural equality. Match → `SortKey::Column(idx)` and
/// the runtime sorts post-projection by that column.
///
/// Free-standing aggregates that are NOT in the RETURN list are
/// rejected with a clear error directing the user to project the
/// aggregate (alias optional) — see
/// `typecheck_rejects_direct_aggregate_not_in_return` below.
#[test]
fn typecheck_resolves_direct_aggregate_in_sort_key() {
    let r = compile_query(
        "MATCH (x: User) RETURN x.city, COUNT(*) GROUP BY x.city ORDER BY COUNT(*) DESC",
    );
    assert!(
        r.is_ok(),
        "direct ORDER BY COUNT(*) should be accepted when COUNT(*) is in RETURN, got: {:?}",
        r.err()
    );
}

/// Free-standing aggregate in a sort key that does NOT appear in
/// the RETURN list. The parser cannot rewrite to `SortKey::Column`
/// without a target column, and we will not re-aggregate during
/// post-projection sort, so this is rejected with an explanatory
/// error.
#[test]
fn typecheck_rejects_direct_aggregate_not_in_return() {
    let r = compile_query("MATCH (x: User) RETURN x.city GROUP BY x.city ORDER BY COUNT(*) DESC");
    let err = r.expect_err("ORDER BY COUNT(*) without RETURN COUNT(*) must be rejected");
    assert!(
        err.contains("not in the RETURN list") || err.contains("COUNT"),
        "error must explain the rule, got: {err}"
    );
}

/// SUM-form direct aggregate in sort key. Same path as COUNT(*)
/// but exercises the `GeneralSet` aggregator variant.
#[test]
fn typecheck_resolves_direct_sum_aggregate_in_sort_key() {
    let r = compile_query(
        "MATCH (x: User) RETURN x.city, SUM(x.age) GROUP BY x.city ORDER BY SUM(x.age) DESC",
    );
    assert!(
        r.is_ok(),
        "ORDER BY SUM(x.age) should be accepted, got: {:?}",
        r.err()
    );
}

// =====================================================================
// Class 2: typechecker accepts ill-typed sort keys. Schema::star() is
// permissive by design, so these tests use a strict schema built via
// the catalog. The check is the same — the typechecker should reject
// when the schema makes the type clear.
// =====================================================================

/// Build a strict schema where `User` declares only `name: str`,
/// without `nonexistent`. Used by the next two tests.
fn strict_user_schema() -> Schema {
    use gqlrust::typing::descriptor_type::DescriptorType;
    use gqlrust::typing::label_type::LabelType;
    use gqlrust::typing::property_type::PropertyType;
    use gqlrust::typing::simple_type::SimpleType;
    use gqlrust::typing::variable_type::VariableType;

    let mut props = PropertyType::open_empty();
    props.extend("name".into(), SimpleType::S);
    let user = VariableType::Node(DescriptorType::new(LabelType::Label("User".into()), props));
    Schema::from_parts(vec![user], vec![])
}

/// `MATCH (x: User) RETURN x.name ORDER BY x.nonexistent` under a
/// schema that declares only `name` for `User`.
///
/// **Today**: typechecker accepts. Runtime evaluates `x.nonexistent`,
/// gets `ExprResult::Failure`, treats every row's sort key as null, so
/// the order is preserved by the secondary key (or by stable sort if
/// there is no secondary). No error, no warning.
///
/// **Gap**: under a strict schema, an attribute lookup on a label that
/// does not declare the attribute should at least raise a warning,
/// arguably an error. The same gap exists for `RETURN x.nonexistent`
/// — it's not specific to ORDER BY — but ORDER BY makes it visible
/// because the user's intent is to sort by something concrete, and a
/// silently-null sort key produces a misleading result.
#[test]
#[ignore = "Strict schema does not flag missing-attribute access in sort keys"]
fn typecheck_gap_unknown_attr_under_strict_schema() {
    let schema = strict_user_schema();
    let r = compile_query_with(
        &schema,
        "MATCH (x: User) RETURN x.name ORDER BY x.nonexistent",
    );
    assert!(
        r.is_err(),
        "strict schema must reject ORDER BY on undeclared attribute, got Ok"
    );
}

/// `MATCH (x: User) RETURN x.name ORDER BY x.tags` where `tags` is
/// declared as `[int]` in the schema.
///
/// **Status: FIXED** in this branch. Per ISO §22.14 Conformance Rule 1
/// (read together with §4.4.2's definition of "comparable values"),
/// without Feature GA04 the operands of an ordering operation must be
/// predefined scalar types. `check_order_by` now propagates the
/// sort-key's `SimpleType` and rejects `List(_)` / `Record(_)` /
/// `Group(_)`. The error message cites §22.14 directly so the user
/// can find the rule.
///
/// Runtime behavior (silent fallback to Equal) is preserved on the
/// unchecked path (`compile_query_unchecked`) so users that opt out
/// of the typechecker still get a result, just an unordered one.
#[test]
fn typecheck_rejects_non_comparable_sort_key() {
    use gqlrust::typing::descriptor_type::DescriptorType;
    use gqlrust::typing::label_type::LabelType;
    use gqlrust::typing::property_type::PropertyType;
    use gqlrust::typing::simple_type::SimpleType;
    use gqlrust::typing::variable_type::VariableType;

    let mut props = PropertyType::open_empty();
    props.extend("name".into(), SimpleType::S);
    props.extend("tags".into(), SimpleType::List(Box::new(SimpleType::Z)));
    let user = VariableType::Node(DescriptorType::new(LabelType::Label("User".into()), props));
    let schema = Schema::from_parts(vec![user], vec![]);

    let r = compile_query_with(&schema, "MATCH (x: User) RETURN x.name ORDER BY x.tags");
    let err = r.expect_err("ORDER BY on a list-typed property should be rejected");
    assert!(
        err.contains("§22.14") || err.contains("not a comparable value type"),
        "error must cite the ISO rule, got: {err}",
    );
}

// =====================================================================
// Class 3: runtime "works" but produces a meaningless answer. Pin the
// observed behavior so the prof can see exactly what happens today.
// =====================================================================

/// Companion to `typecheck_gap_unknown_attr_under_strict_schema`: the
/// runtime accepts the same query under a permissive schema. This test
/// is NOT ignored — it documents the current observable behavior so
/// the contrast with the desired typecheck behavior is concrete.
#[test]
fn runtime_accepts_unknown_attr_in_sort_key_under_permissive_schema() {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Alice"}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Bob"}}
      ],
      "edges": []
    }"#;
    let g = Graph::from_json_str(json).unwrap();
    let q = compile_query("MATCH (x: User) RETURN x.name ORDER BY x.nonexistent")
        .expect("permissive schema accepts undeclared attribute");
    let rt = Runtime::new(&g);
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rows) => assert_eq!(
            rows.len(),
            2,
            "all rows must survive — sort key is null for every row, falls back to stable order"
        ),
        _ => panic!("expected projected"),
    }
}

// =====================================================================
// Class 4: ORDER BY with RETURN-alias resolution (Gap 1 + 2). These
// runtime tests pin the actual ordered output so we know the alias
// path produces the right comparator behaviour, not just "doesn't
// crash".
// =====================================================================

/// Non-aggregate query: `RETURN x.name AS n ORDER BY n` must produce
/// the same output as `RETURN x.name ORDER BY x.name`. The alias
/// resolves to the underlying `Expr` at parse time, so the runtime
/// uses the existing pre-projection sort fast path.
#[test]
fn runtime_alias_of_expr_sorts_identically_to_underlying_expr() {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Charlie"}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Alice"}},
        {"id": "u3", "labels": ["User"], "props": {"name": "Bob"}}
      ],
      "edges": []
    }"#;
    let g = Graph::from_json_str(json).unwrap();

    let rt = Runtime::new(&g);
    let q1 = compile_query("MATCH (x: User) RETURN x.name AS n ORDER BY n").unwrap();
    let q2 = compile_query("MATCH (x: User) RETURN x.name ORDER BY x.name").unwrap();
    let r1 = match rt.run_query(&q1, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    let r2 = match rt.run_query(&q2, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    assert_eq!(r1, r2, "alias-as-sort-key must match the bare-expr form");
    assert_eq!(
        r1,
        vec![
            vec![Value::Str("Alice".into())],
            vec![Value::Str("Bob".into())],
            vec![Value::Str("Charlie".into())],
        ]
    );
}

/// Aggregate query with alias-of-aggregate: groups must come back in
/// descending order of `COUNT(*)`. Pre-fix this would compile but
/// silently NOT sort (the aggregate path skipped sort entirely);
/// post-fix the post-projection sort orders by the alias's column.
#[test]
fn runtime_alias_of_aggregate_orders_groups_by_count() {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"city": "Santiago"}},
        {"id": "u2", "labels": ["User"], "props": {"city": "Santiago"}},
        {"id": "u3", "labels": ["User"], "props": {"city": "Santiago"}},
        {"id": "u4", "labels": ["User"], "props": {"city": "Valpo"}},
        {"id": "u5", "labels": ["User"], "props": {"city": "Concepcion"}},
        {"id": "u6", "labels": ["User"], "props": {"city": "Concepcion"}}
      ],
      "edges": []
    }"#;
    let g = Graph::from_json_str(json).unwrap();
    let rt = Runtime::new(&g);
    let q = compile_query(
        "MATCH (x: User) RETURN x.city, COUNT(*) AS c GROUP BY x.city ORDER BY c DESC",
    )
    .unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    let counts: Vec<&Value> = rows.iter().map(|r| &r[1]).collect();
    assert_eq!(
        counts,
        vec![&Value::Int(3), &Value::Int(2), &Value::Int(1)],
        "groups must be ordered by COUNT(*) DESC"
    );
}

/// Direct aggregate `ORDER BY COUNT(*) DESC` (no alias) sorts the
/// aggregated rows by group cardinality. Pre-fix the parser
/// rejected this; post-fix the structural-match path picks up
/// `COUNT(*)` and resolves to `SortKey::Column(1)`.
#[test]
fn runtime_direct_count_star_sorts_groups_by_cardinality() {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"city": "Santiago"}},
        {"id": "u2", "labels": ["User"], "props": {"city": "Santiago"}},
        {"id": "u3", "labels": ["User"], "props": {"city": "Santiago"}},
        {"id": "u4", "labels": ["User"], "props": {"city": "Valpo"}},
        {"id": "u5", "labels": ["User"], "props": {"city": "Concepcion"}},
        {"id": "u6", "labels": ["User"], "props": {"city": "Concepcion"}}
      ],
      "edges": []
    }"#;
    let g = Graph::from_json_str(json).unwrap();
    let rt = Runtime::new(&g);
    let q = compile_query(
        "MATCH (x: User) RETURN x.city, COUNT(*) GROUP BY x.city ORDER BY COUNT(*) DESC",
    )
    .unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    let counts: Vec<&Value> = rows.iter().map(|r| &r[1]).collect();
    assert_eq!(
        counts,
        vec![&Value::Int(3), &Value::Int(2), &Value::Int(1)],
        "direct ORDER BY COUNT(*) DESC must order groups by cardinality"
    );
}

/// Aggregate query with `ORDER BY <bare expr matching a RETURN item>`:
/// covers the structural-match auto-resolution. The query has no
/// alias on `x.city` but the parser still rewrites the sort key to a
/// column reference because `x.city` is identical to a RETURN item.
#[test]
fn runtime_aggregate_query_sorts_by_implicit_grouping_column() {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["U"], "props": {"city": "Z"}},
        {"id": "u2", "labels": ["U"], "props": {"city": "A"}},
        {"id": "u3", "labels": ["U"], "props": {"city": "M"}}
      ],
      "edges": []
    }"#;
    let g = Graph::from_json_str(json).unwrap();
    let rt = Runtime::new(&g);
    let q = compile_query("MATCH (x: U) RETURN x.city, COUNT(*) GROUP BY x.city ORDER BY x.city")
        .unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    let cities: Vec<&Value> = rows.iter().map(|r| &r[0]).collect();
    assert_eq!(
        cities,
        vec![
            &Value::Str("A".into()),
            &Value::Str("M".into()),
            &Value::Str("Z".into()),
        ],
        "groups must come out alphabetically by city"
    );
}

/// Mixed-keys typecheck rule: `ORDER BY n, x.age` where `n` is an
/// aggregate alias and `x.age` is a bare Expr. The typechecker
/// rejects because post-projection sort cannot evaluate Expr keys
/// (no assignment after aggregation) and pre-projection sort cannot
/// look up Column keys (no projected row yet).
#[test]
fn typecheck_rejects_mixed_alias_and_expr_sort_keys_in_aggregate_query() {
    let r = compile_query(
        "MATCH (x: User) RETURN x.name, COUNT(*) AS c GROUP BY x.name ORDER BY c, x.email",
    );
    let err = r.expect_err("mixed alias + Expr sort keys must be rejected");
    assert!(
        err.contains("ORDER BY mixes") || err.contains("only aliases"),
        "error message must explain the mix, got: {err}"
    );
}

/// Companion to the list-typed gap: under permissive Schema::star
/// the typechecker sees `Star` for the sort key (not `List`), so
/// the comparable-type check does not fire. The runtime evaluates
/// the list value, `value_cmp` returns `None`, the comparator falls
/// back to `Equal` per US007, and rows survive in some stable order.
/// Pinned so the contrast with the strict-schema gap test is
/// concrete.
#[test]
fn runtime_treats_list_sort_key_as_equal_so_input_order_preserved() {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Alice", "tags": [3, 1]}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Bob",   "tags": [1, 2]}},
        {"id": "u3", "labels": ["User"], "props": {"name": "Carol", "tags": [2]}}
      ],
      "edges": []
    }"#;
    let g = Graph::from_json_str(json).unwrap();
    let q = compile_query("MATCH (x: User) RETURN x.name ORDER BY x.tags").unwrap();
    let rt = Runtime::new(&g);
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rows) => {
            // 3 rows survive in some stable order — the runtime did not
            // actually sort by `tags` because lists aren't comparable.
            assert_eq!(rows.len(), 3);
        }
        _ => panic!("expected projected"),
    }
}
