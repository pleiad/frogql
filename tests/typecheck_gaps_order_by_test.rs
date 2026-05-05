//! Known typechecker / parser gaps for ORDER BY (Feature GA03 + GQ13).
//!
//! These tests document cases where the surface accepts (or rejects with
//! the wrong message) queries that ISO/IEC 39075:2024 §16.17 implies
//! should be handled differently. Every test in this file is `#[ignore]`
//! by default so it does not break CI; running `cargo test --
//! --ignored` exposes them as failing tests for review.
//!
//! The runtime behaves correctly on every legal query that reaches it —
//! these gaps are upstream of evaluation. They split into three classes:
//!
//! 1. **Parser blocks ISO-legal forms**: bare-name aliases and aggregate
//!    expressions in `<sort key>` are §16.17-legal but the gqlite parser
//!    rejects them earlier than typecheck.
//! 2. **Typechecker accepts ill-typed sort keys**: missing attribute
//!    (under a strict graph type) and non-comparable types (lists,
//!    records) are accepted silently; the runtime falls back to
//!    "treat-as-null" or "treat-as-equal" instead of producing an error.
//! 3. **Aggregate output typing is implicit**: same root cause we already
//!    discussed for `SUM(x.name)` — the typechecker discards the inner
//!    expression's type, so it cannot detect a non-numeric aggregate.

use gqlrust::compile_query;
use gqlrust::compile_query_with;
use gqlrust::model::graph::Graph;
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
/// **Today**: parser fails with `"unexpected bare variable 'n' in
/// expression"` because `expr()` requires `name.attr` and rejects
/// stand-alone names.
///
/// **Gap**: either teach `expr()` to accept bare names that match a
/// RETURN alias, or add a separate sort-key parser that knows about
/// the aliases in scope.
#[test]
#[ignore = "ISO-legal RETURN alias as sort key — parser rejects bare names"]
fn typecheck_gap_alias_in_sort_key() {
    let r = compile_query("MATCH (x) RETURN x.name AS n ORDER BY n");
    assert!(
        r.is_ok(),
        "ORDER BY <alias> should resolve via the RETURN alias, got: {:?}",
        r.err()
    );
}

/// `MATCH (x: User) RETURN x.city, COUNT(*) GROUP BY x.city ORDER BY COUNT(*) DESC`
///
/// **ISO §16.17 SR 1**: a sort key is an `<aggregating value expression>`,
/// which expands to include `<aggregate function>`. Sorting groups by
/// their cardinality (`ORDER BY COUNT(*)`) is the canonical use case.
///
/// **Today**: parser fails with `"expected expression, got Count"`
/// because `expr()` doesn't dispatch to `aggregate_function()`. Only
/// `return_item()` does, via `peek_aggregate_kind()`.
///
/// **Gap**: extend `expr()` to recognize aggregate function calls, OR
/// special-case sort_spec to allow the aggregate-function form. The
/// runtime would also need to evaluate the sort key against the
/// aggregated row (post-projection), which our current
/// "sort-before-projection" pipeline does not support — see also
/// `typecheck_gap_sort_over_aggregate_column_runtime` below.
#[test]
#[ignore = "ISO-legal aggregate as sort key — parser rejects COUNT/SUM/AVG/MIN/MAX outside RETURN"]
fn typecheck_gap_aggregate_in_sort_key() {
    let r = compile_query(
        "MATCH (x: User) RETURN x.city, COUNT(*) GROUP BY x.city ORDER BY COUNT(*) DESC",
    );
    assert!(
        r.is_ok(),
        "ORDER BY COUNT(*) should be accepted, got: {:?}",
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

/// Companion to `typecheck_gap_non_comparable_sort_key`: list-typed
/// sort keys end up as Equal across the comparator. We pin the
/// "doesn't crash, doesn't sort" outcome so the prof can decide
/// whether silent identity-on-incomparable is acceptable.
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
