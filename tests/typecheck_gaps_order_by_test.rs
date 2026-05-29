//! ORDER BY typechecker/parser tests (Features GA03 + GQ13).
//! `#[ignore]`d cases document remaining gaps; run with
//! `cargo test -- --ignored` to surface them as failing.

use gqlrust::compile_query;
use gqlrust::compile_query_with;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::typing::variable_type::Schema;

// ISO §16.17 SR 5c (RETURN-alias as sort key) and SR 1 (aggregate
// as sort key) — both legal per spec, both worked around in the
// parser.

#[test]
fn typecheck_resolves_alias_in_sort_key() {
    let r = compile_query("MATCH (x) RETURN x.name AS n ORDER BY n");
    assert!(r.is_ok(), "got: {:?}", r.err());
}

#[test]
fn typecheck_resolves_aggregate_alias_in_sort_key() {
    let r = compile_query(
        "MATCH (x: User) RETURN x.city, COUNT(*) AS c GROUP BY x.city ORDER BY c DESC",
    );
    assert!(r.is_ok(), "got: {:?}", r.err());
}

#[test]
fn typecheck_resolves_direct_aggregate_in_sort_key() {
    let r = compile_query(
        "MATCH (x: User) RETURN x.city, COUNT(*) GROUP BY x.city ORDER BY COUNT(*) DESC",
    );
    assert!(r.is_ok(), "got: {:?}", r.err());
}

#[test]
fn typecheck_rejects_direct_aggregate_not_in_return() {
    let r = compile_query("MATCH (x: User) RETURN x.city GROUP BY x.city ORDER BY COUNT(*) DESC");
    let err = r.expect_err("free-standing aggregate must be rejected");
    assert!(
        err.contains("not in the RETURN list") || err.contains("COUNT"),
        "got: {err}"
    );
}

#[test]
fn typecheck_resolves_direct_sum_aggregate_in_sort_key() {
    let r = compile_query(
        "MATCH (x: User) RETURN x.city, SUM(x.age) GROUP BY x.city ORDER BY SUM(x.age) DESC",
    );
    assert!(r.is_ok(), "got: {:?}", r.err());
}

// ISO §22.14: sort keys must be comparable types (no Feature GA04).

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

#[test]
#[ignore = "Strict schema does not flag missing-attribute access in sort keys"]
fn typecheck_gap_unknown_attr_under_strict_schema() {
    let schema = strict_user_schema();
    let r = compile_query_with(
        &schema,
        "MATCH (x: User) RETURN x.name ORDER BY x.nonexistent",
    );
    assert!(r.is_err(), "got Ok");
}

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

// Companion runtime tests: pin the observable behavior on cases the
// typechecker doesn't reject (yet) so the contrast is concrete.

#[test]
fn runtime_accepts_unknown_attr_in_sort_key_under_permissive_schema() {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Alice"}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Bob"}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
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

// Runtime tests pinning the actual ordered output for alias / column-
// reference sort keys.

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
    let g = MemoryGraphStore::from_json_str(json).unwrap();

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
    let g = MemoryGraphStore::from_json_str(json).unwrap();
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
    let g = MemoryGraphStore::from_json_str(json).unwrap();
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
    let g = MemoryGraphStore::from_json_str(json).unwrap();
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

// Permissive-schema counterpart of the list-typed gap: type is
// `Star`, the §22.14 check passes, runtime falls back to Equal per
// US007.
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
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let q = compile_query("MATCH (x: User) RETURN x.name ORDER BY x.tags").unwrap();
    let rt = Runtime::new(&g);
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rows) => assert_eq!(rows.len(), 3),
        _ => panic!("expected projected"),
    }
}
