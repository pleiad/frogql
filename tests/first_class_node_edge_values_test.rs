//! Tests for first-class node / edge reference values.
//!
//! ISO/IEC 39075:2024:
//! - §14.11 SR 8a: bare `<binding variable reference>` is a legal RETURN item.
//! - §20.12 SR 9: its declared type is the bound variable's value type.
//! - §4.4.4: reference values are equal iff same referent (same id);
//!   not orderable without Feature GA04.
//! - §22.14 CR 1: ordering operations require comparable value types.

use std::collections::BTreeMap;

use gqlrust::compile_query;
use gqlrust::compile_query_unchecked;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::syntax::expr::Expr;

fn graph_users_pets() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Alice"}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Bob"}},
        {"id": "p1", "labels": ["Pet"],  "props": {"name": "Rex"}}
      ],
      "edges": [
        {"id": "e1", "labels": ["OWNS"], "props": {},
         "endpoints": ["u1", "p1"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run_projected(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected for {q:?}"),
    }
}

// =====================================================================
// Parser
// =====================================================================

#[test]
fn parser_bare_name_in_return_produces_expr_var() {
    let q = compile_query_unchecked("MATCH (n) RETURN n").unwrap();
    let items = q.returns.unwrap();
    let expr = match &items[0] {
        gqlrust::syntax::query::ReturnItem::Expr { expr, .. } => expr,
        _ => panic!("expected Expr return item"),
    };
    assert!(matches!(expr, Expr::Var(name) if name == "n"));
}

#[test]
fn parser_bare_name_in_where_produces_expr_var() {
    let q = compile_query_unchecked("MATCH (a), (b) WHERE a = b RETURN a, b").unwrap();
    // The WHERE expr is wrapped inside `PathPattern::Filter`. We just
    // verify the query parses; the runtime test pins the semantics.
    assert_eq!(q.matches.len(), 1);
}

#[test]
fn parser_bare_name_followed_by_dot_still_attr_lookup() {
    let q = compile_query_unchecked("MATCH (n) RETURN n.name").unwrap();
    let items = q.returns.unwrap();
    let expr = match &items[0] {
        gqlrust::syntax::query::ReturnItem::Expr { expr, .. } => expr,
        _ => panic!("expected Expr return item"),
    };
    assert!(matches!(expr, Expr::AttrLookup { .. }));
}

// =====================================================================
// Runtime: RETURN n
// =====================================================================

#[test]
fn runtime_return_bare_node_yields_value_node() {
    let g = graph_users_pets();
    let rows = run_projected(&g, "MATCH (u: User) WHERE u.name = 'Alice' RETURN u");
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0][0], Value::Node(_)));
}

#[test]
fn runtime_return_bare_edge_yields_value_edge() {
    let g = graph_users_pets();
    let rows = run_projected(&g, "MATCH (u: User)-[r:OWNS]->(p: Pet) RETURN r");
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0][0], Value::Edge(_)));
}

#[test]
fn runtime_return_node_and_attr_in_same_query() {
    let g = graph_users_pets();
    let rows = run_projected(
        &g,
        "MATCH (u: User) WHERE u.name = 'Alice' RETURN u, u.name",
    );
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0][0], Value::Node(_)));
    assert_eq!(rows[0][1], Value::Str("Alice".into()));
}

// =====================================================================
// Runtime: WHERE n = m / WHERE n <> m (ISO §4.4.4 identity)
// =====================================================================

#[test]
fn runtime_node_equality_by_identity() {
    let g = graph_users_pets();
    // Two patterns binding the same node id via shared variable: equal.
    let rows = run_projected(&g, "MATCH (u: User) WHERE u.name = 'Alice' RETURN u, u");
    assert_eq!(rows.len(), 1);
    if let (Value::Node(a), Value::Node(b)) = (&rows[0][0], &rows[0][1]) {
        assert_eq!(a, b, "same variable, same id");
    } else {
        panic!("expected two Value::Node");
    }
}

#[test]
fn runtime_node_inequality_filters_pairs() {
    let g = graph_users_pets();
    // Cross product of (User, User) excluding self-pairs. Two users
    // → 2 distinct unordered pairs → 2 ordered pairs after `<>`.
    let rt = Runtime::new(&g);
    let q = compile_query("MATCH (a: User), (b: User) WHERE a <> b RETURN a, b").unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    // 2 users × 2 users − 2 self-pairs = 2.
    assert_eq!(rows.len(), 2);
    for row in &rows {
        if let (Value::Node(a), Value::Node(b)) = (&row[0], &row[1]) {
            assert_ne!(a, b, "self-pair should have been filtered");
        } else {
            panic!("expected two Value::Node per row");
        }
    }
}

#[test]
fn runtime_node_eq_falsy_for_distinct_ids() {
    let g = graph_users_pets();
    let rt = Runtime::new(&g);
    let q = compile_query("MATCH (a: User), (b: User) WHERE a = b RETURN a").unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    // Only self-pairs match → 2 rows (Alice = Alice, Bob = Bob).
    assert_eq!(rows.len(), 2);
}

// =====================================================================
// Runtime: IS NULL semantics for OPTIONAL
// =====================================================================

#[test]
fn runtime_bare_node_is_null_when_optional_did_not_match() {
    let g = graph_users_pets();
    // Bob has no pet. OPTIONAL leaves p as PathValue::Nothing → Value::Null.
    let rt = Runtime::new(&g);
    let q = compile_query(
        "MATCH (u: User {name: 'Bob'}) OPTIONAL MATCH (u)-[:OWNS]->(p: Pet) \
         RETURN u.name, p IS NULL AS missing",
    )
    .unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Bool(true));
}

#[test]
fn runtime_bare_node_not_null_when_optional_matched() {
    let g = graph_users_pets();
    let rows = run_projected(
        &g,
        "MATCH (u: User {name: 'Alice'}) OPTIONAL MATCH (u)-[:OWNS]->(p: Pet) \
         RETURN u.name, p IS NULL AS missing",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Bool(false));
}

// =====================================================================
// Typechecker: ORDER BY on a bare node is rejected (§22.14 CR 1)
// =====================================================================

#[test]
fn typecheck_order_by_bare_node_is_rejected() {
    // ISO §4.4.4 says reference values are equality-comparable, NOT
    // ordering-comparable without GA04. The ORDER BY check from
    // §22.14 should reject — ditto for §16.17 SR 1.
    let r = compile_query("MATCH (n: User) RETURN n ORDER BY n");
    let err = r.expect_err("ORDER BY on a node must be rejected");
    assert!(
        err.contains("§22.14") || err.contains("not a comparable value type"),
        "error must cite §22.14, got: {err}"
    );
}

#[test]
fn typecheck_unbound_variable_in_expression_is_error() {
    let r = compile_query("MATCH (n) RETURN unbound_var");
    let err = r.expect_err("unbound variable in expression must error");
    assert!(
        err.contains("not found"),
        "error must mention 'not found', got: {err}"
    );
}

// =====================================================================
// Combined with COALESCE (sibling feature from earlier branch)
// =====================================================================

#[test]
fn runtime_coalesce_picks_node_over_null() {
    let g = graph_users_pets();
    // `(p IS NULL ? fallback : p)` style: COALESCE on the optional-or-not
    // node returns either the node or the literal fallback. With Alice's
    // OWNS edge bound, p is a Value::Node and survives the first slot.
    let rt = Runtime::new(&g);
    let q = compile_query(
        "MATCH (u: User {name: 'Alice'}) OPTIONAL MATCH (u)-[:OWNS]->(p: Pet) \
         RETURN COALESCE(p, 'no-pet') AS picked",
    )
    .unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0][0], Value::Node(_)));
}

#[test]
fn runtime_coalesce_falls_through_when_optional_missing() {
    let g = graph_users_pets();
    let rows = run_projected(
        &g,
        "MATCH (u: User {name: 'Bob'}) OPTIONAL MATCH (u)-[:OWNS]->(p: Pet) \
         RETURN COALESCE(p, 'no-pet') AS picked",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Str("no-pet".into()));
}

// =====================================================================
// Smoke: SimpleType lattice handles new variants
// =====================================================================

#[test]
fn simple_type_lattice_node_meet_and_union() {
    use gqlrust::typing::simple_type::SimpleType;
    assert_eq!(
        SimpleType::meet(&SimpleType::Node, &SimpleType::Node),
        SimpleType::Node
    );
    assert_eq!(
        SimpleType::meet(&SimpleType::Node, &SimpleType::Edge),
        SimpleType::Zero
    );
    // Union of distinct ref-value types is a Union.
    let u = SimpleType::union(&SimpleType::Node, &SimpleType::Edge);
    assert!(matches!(u, SimpleType::Union(_, _)));
    // Star absorbs (top-most).
    assert_eq!(
        SimpleType::meet(&SimpleType::Node, &SimpleType::Star),
        SimpleType::Node
    );
}

#[test]
fn value_node_equality_is_by_id() {
    // Pin §4.4.4: two reference values equal iff same referent (id).
    assert_eq!(Value::Node(7), Value::Node(7));
    assert_ne!(Value::Node(7), Value::Node(8));
    // Cross-kind never equal (even with matching id).
    assert_ne!(Value::Node(7), Value::Edge(7));
    // Node and a Record with id=7 do NOT equal — distinct kinds.
    let mut r = BTreeMap::new();
    r.insert("id".to_string(), Value::Int(7));
    assert_ne!(Value::Node(7), Value::Record(r));
}

// =====================================================================
// Edge cases surfaced by the post-implementation survey
// =====================================================================

/// `-[r]->{1,2}` binds `r` to a `PathValue::Group` of the matched edges.
/// Projecting a repetition-group variable lifts the group to a
/// `Value::List` of edge reference values, in match order. Here the graph
/// has a single edge `u1 -[OWNS]-> p1` and `p1` has no out-edge, so the
/// `{1,2}` repetition yields exactly one 1-edge path: `r ↦ [OWNS]`.
#[test]
fn runtime_return_bare_repetition_var_yields_list_of_edges() {
    let g = graph_users_pets();
    let rt = Runtime::new(&g);
    let q = compile_query("MATCH ()-[r]->{1,2}() RETURN r").unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    assert_eq!(rows.len(), 1, "exactly one 1-edge path matches");
    assert_eq!(
        rows[0][0],
        Value::List(vec![Value::Edge(0)]),
        "a repetition-group variable projects as a List of edge references"
    );
}

/// `WHERE n = e` (node vs edge): per §4.4.4 reference values of
/// different static base types are NOT essentially comparable; the
/// comparison is false in 3VL → row drops.
#[test]
fn runtime_cross_kind_node_edge_comparison_drops_rows() {
    let g = graph_users_pets();
    let rt = Runtime::new(&g);
    let q = compile_query("MATCH (n: User), ()-[e:OWNS]->() WHERE n = e RETURN n").unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    assert!(rows.is_empty(), "node-vs-edge equality always false");
}

/// `ORDER BY n.scalar_attr` is the positive case complementing the
/// reject test for `ORDER BY n` — sorting by an attribute that
/// resolves to a scalar is fine even when the holder is a node.
#[test]
fn runtime_order_by_node_attribute_sorts_correctly() {
    let g = graph_users_pets();
    let rows = run_projected(&g, "MATCH (u: User) RETURN u.name ORDER BY u.name DESC");
    let names: Vec<String> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            v => panic!("expected str, got {v:?}"),
        })
        .collect();
    assert_eq!(names, vec!["Bob", "Alice"]);
}

/// `RETURN [n, m]` — list literal with bare-variable elements.
/// Today the list-literal parser only accepts constants, so this is
/// a parse error. Pinned so any future relaxation produces a
/// detectable test flip.
#[test]
fn parser_rejects_list_literal_with_bare_variables() {
    let r = compile_query("MATCH (n: User), (m: User) RETURN [n, m]");
    let err = r.expect_err("list literal with bare vars must be a parse error");
    assert!(
        err.contains("non-constant") || err.contains("list"),
        "error message should mention the constraint, got: {err}"
    );
}

/// ISO §14.11 SR 8a: `RETURN n` is shorthand for `RETURN n AS n`.
/// The wire-format key in the Python binding now honors this. The
/// AST itself does NOT rewrite (alias remains None); pinning the
/// underlying `Expr::Var("n")` shape is what the Python layer reads.
#[test]
fn return_bare_var_keeps_var_in_ast_for_implicit_alias() {
    let q = compile_query_unchecked("MATCH (n) RETURN n").unwrap();
    let items = q.returns.unwrap();
    assert_eq!(items.len(), 1);
    if let gqlrust::syntax::query::ReturnItem::Expr { expr, alias } = &items[0] {
        assert!(matches!(expr, Expr::Var(name) if name == "n"));
        assert!(alias.is_none(), "implicit alias is applied at projection");
    } else {
        panic!("expected Expr return item");
    }
}
