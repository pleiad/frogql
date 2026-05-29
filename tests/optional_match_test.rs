//! Tests for `OPTIONAL MATCH`. Covers parser, typechecker, and runtime
//! semantics of the left-outer-join formulation of the TOpt rule.
//!
//! The runtime semantics map directly onto `TypeEnvironment::outer_join`:
//! a row in the optional pattern that unifies with the accumulated row is
//! the "success" branch (regular natural join); otherwise the row is
//! preserved with the optional pattern's new variables set to `Nothing`,
//! which projects as `Value::Null` (the "unsuccess" branch).

use std::path::Path;

use gqlrust::compile_query;
use gqlrust::compile_query_unchecked;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::syntax::query::MatchStatement;

fn fraud_graph() -> MemoryGraphStore {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    MemoryGraphStore::from_file(&p).unwrap()
}

/// Tiny graph with two users; only one has a Pet relationship. Used to
/// exercise the unsuccess branch on a query whose optional cannot match
/// for some accumulated rows.
fn graph_with_optional_pet() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Alice"}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Bob"}},
        {"id": "pet1", "labels": ["Pet"], "props": {"name": "Rex"}}
      ],
      "edges": [
        {"id": "e1", "labels": ["OWNS"], "props": {},
         "endpoints": ["u1", "pet1"], "directionality": "->"}
      ]
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

fn run_raw_count(g: &MemoryGraphStore, q: &str) -> usize {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Raw(ir) => ir.rows.len(),
        _ => panic!("expected raw result for {q:?}"),
    }
}

// =====================================================================
// Parser
// =====================================================================

#[test]
fn parser_optional_match_produces_optional_variant() {
    let q = compile_query_unchecked("OPTIONAL MATCH (x)").unwrap();
    assert_eq!(q.matches.len(), 1);
    assert!(matches!(&q.matches[0], MatchStatement::Optional { .. }));
}

#[test]
fn parser_match_then_optional_preserves_order() {
    let q = compile_query_unchecked("MATCH (x) OPTIONAL MATCH (y)").unwrap();
    assert_eq!(q.matches.len(), 2);
    assert!(matches!(&q.matches[0], MatchStatement::Simple { .. }));
    assert!(matches!(&q.matches[1], MatchStatement::Optional { .. }));
}

#[test]
fn parser_optional_match_with_per_clause_where() {
    let q = compile_query_unchecked("MATCH (x: User) OPTIONAL MATCH (y: Pet) WHERE y.name = 'Rex'")
        .unwrap();
    assert_eq!(q.matches.len(), 2);
    assert!(matches!(&q.matches[1], MatchStatement::Optional { .. }));
}

#[test]
fn parser_three_match_chain_with_two_optionals() {
    let q = compile_query_unchecked("MATCH (x) OPTIONAL MATCH (y) OPTIONAL MATCH (z)").unwrap();
    assert_eq!(q.matches.len(), 3);
    assert!(matches!(&q.matches[0], MatchStatement::Simple { .. }));
    assert!(matches!(&q.matches[1], MatchStatement::Optional { .. }));
    assert!(matches!(&q.matches[2], MatchStatement::Optional { .. }));
}

#[test]
fn parser_optional_without_match_keyword_is_error() {
    let r = compile_query_unchecked("OPTIONAL (x)");
    assert!(r.is_err(), "expected parse error, got {r:?}");
}

#[test]
fn display_roundtrips_through_optional() {
    let q = compile_query_unchecked("MATCH (x: User) OPTIONAL MATCH (y: Pet)").unwrap();
    assert!(q.to_string().contains("OPTIONAL MATCH"));
}

#[test]
fn has_any_optional_pins_chain_classification() {
    let only_simple = compile_query_unchecked("MATCH (x) MATCH (y)").unwrap();
    assert!(!only_simple.has_any_optional());

    let with_opt = compile_query_unchecked("MATCH (x) OPTIONAL MATCH (y)").unwrap();
    assert!(with_opt.has_any_optional());

    let leading_opt = compile_query_unchecked("OPTIONAL MATCH (x)").unwrap();
    assert!(leading_opt.has_any_optional());
}

// =====================================================================
// Typechecker — pin the env shape produced by TOpt + outer_join
// =====================================================================

#[test]
fn typecheck_optional_introduces_null_branch_for_new_var() {
    // y appears only in the OPTIONAL → its type is Null ⊔ T (a Union).
    // x is in the leading MATCH → its type is unchanged.
    use gqlrust::elaborate::elaborate_query;
    use gqlrust::parser::parse_query;
    use gqlrust::typing::checker::Typechecker;
    use gqlrust::typing::variable_type::{Schema, VariableType};

    let q = elaborate_query(parse_query("MATCH (x) OPTIONAL MATCH (y)").unwrap());
    let mut tc = Typechecker::new(Schema::star());
    let r = tc.check_query(&q);
    assert!(r.ok, "errors: {:?}", tc.errors);

    let x_ty = r.env.get("x").expect("x bound");
    assert!(
        !matches!(x_ty, VariableType::Union(_, _)),
        "x should not gain Null: {x_ty}"
    );

    let y_ty = r.env.get("y").expect("y bound");
    let is_null_union = matches!(
        y_ty,
        VariableType::Union(a, b)
            if matches!(a.as_ref(), VariableType::Null) || matches!(b.as_ref(), VariableType::Null)
    );
    assert!(is_null_union, "y should be Null ⊔ T, got {y_ty}");
}

#[test]
fn typecheck_leading_optional_makes_var_nullable() {
    // OPTIONAL MATCH (x) with empty Γ₁: unsuccess gives x ↦ Null,
    // success gives x ↦ T → join is Null ⊔ T.
    use gqlrust::elaborate::elaborate_query;
    use gqlrust::parser::parse_query;
    use gqlrust::typing::checker::Typechecker;
    use gqlrust::typing::variable_type::{Schema, VariableType};

    let q = elaborate_query(parse_query("OPTIONAL MATCH (x)").unwrap());
    let mut tc = Typechecker::new(Schema::star());
    let r = tc.check_query(&q);
    assert!(r.ok);

    let x_ty = r.env.get("x").expect("x bound");
    let has_null = matches!(
        x_ty,
        VariableType::Union(a, b)
            if matches!(a.as_ref(), VariableType::Null) || matches!(b.as_ref(), VariableType::Null)
    );
    assert!(has_null, "leading OPTIONAL must make x nullable: {x_ty}");
}

#[test]
#[should_panic(expected = "collapsed_pattern() is unsound")]
fn collapsed_pattern_panics_on_optional_in_debug() {
    let q = compile_query_unchecked("MATCH (x) OPTIONAL MATCH (y)").unwrap();
    let _ = q.collapsed_pattern();
}

// =====================================================================
// Runtime — pin the success / unsuccess split
// =====================================================================

/// Two users; only Alice owns a pet. Optional matches Alice's pet but
/// preserves Bob with `pet ↦ Null`. Cardinality must be 2 (one row per user).
#[test]
fn runtime_optional_preserves_unmatched_row_with_null() {
    let g = graph_with_optional_pet();
    let mut rows = run_projected(
        &g,
        "MATCH (u: User) OPTIONAL MATCH (u)-[:OWNS]->(p: Pet) RETURN u.name, p.name",
    );
    rows.sort_by(|a, b| match (&a[0], &b[0]) {
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    });
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("Alice".into()), Value::Str("Rex".into())],
            vec![Value::Str("Bob".into()), Value::Null],
        ]
    );
}

/// Optional pattern that NEVER matches (no edges at all). Every accumulated
/// row must survive with the new var bound to Null.
#[test]
fn runtime_optional_never_matches_keeps_all_rows() {
    let g = graph_with_optional_pet();
    let rows = run_projected(
        &g,
        "MATCH (u: User) OPTIONAL MATCH (u)-[:NoSuchLabel]->(p) RETURN u.name, p.name",
    );
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert_eq!(r[1], Value::Null, "p.name must be null when optional fails");
    }
}

/// Optional that ALWAYS matches degenerates to natural join. Equivalent to
/// the same query with `MATCH` instead of `OPTIONAL MATCH` — both must
/// produce the same row count.
#[test]
fn runtime_optional_always_matches_equals_natural_join() {
    let g = fraud_graph();
    let opt = run_raw_count(
        &g,
        "MATCH (a: Account) OPTIONAL MATCH (a)-[:Transfer]->(b: Account)",
    );
    let nat = run_raw_count(&g, "MATCH (a: Account) MATCH (a)-[:Transfer]->(b: Account)");
    // Every Account has at least one outgoing Transfer in fraud.json,
    // so the unsuccess branch fires zero times → counts must agree.
    assert_eq!(opt, nat);
}

/// fraud.json has exactly one Foo edge (a1 → d1). Three of the four Account
/// rows must fall into the unsuccess branch and emit `b ↦ Null`.
#[test]
fn runtime_optional_partial_match_splits_rows() {
    let g = fraud_graph();
    let mut owners: Vec<(String, Option<String>)> = run_projected(
        &g,
        "MATCH (a: Account) OPTIONAL MATCH (a)-[:Foo]->(b) RETURN a.owner, b.owner",
    )
    .into_iter()
    .map(|r| {
        let a_name = match &r[0] {
            Value::Str(s) => s.clone(),
            v => panic!("expected str for a.owner, got {v:?}"),
        };
        let b_name = match &r[1] {
            Value::Str(s) => Some(s.clone()),
            Value::Null => None,
            v => panic!("expected str|null for b.owner, got {v:?}"),
        };
        (a_name, b_name)
    })
    .collect();
    owners.sort();

    // a1 (Aretha) has Foo→d1 (Fred). The other three Accounts (Scott, Jay,
    // Mike) must fall into the unsuccess branch with b.owner = Null.
    let with_match: Vec<_> = owners.iter().filter(|(_, b)| b.is_some()).collect();
    let without: Vec<_> = owners.iter().filter(|(_, b)| b.is_none()).collect();
    assert_eq!(with_match.len(), 1);
    assert_eq!(with_match[0], &("Aretha".into(), Some("Fred".into())));
    assert_eq!(without.len(), 3);
}

/// Two consecutive OPTIONALs. The first introduces a possibly-null var; the
/// second sees the partially-bound table and must still emit one row per
/// input even when its own pattern fails.
#[test]
fn runtime_two_optionals_each_preserves_input_cardinality() {
    let g = graph_with_optional_pet();
    let rows = run_projected(
        &g,
        "MATCH (u: User) \
         OPTIONAL MATCH (u)-[:OWNS]->(p: Pet) \
         OPTIONAL MATCH (u)-[:NoSuchLabel]->(z) \
         RETURN u.name, p.name, z.name",
    );
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert_eq!(r[2], Value::Null, "z.name must be null in every row");
    }
}

/// Shared variable across MATCH and OPTIONAL: the optional only matches
/// when the shared binding agrees. Bob has no pet, so his row goes into
/// the unsuccess branch even though pet1 exists in the graph.
#[test]
fn runtime_optional_shared_var_filters_to_compatible_extensions() {
    let g = graph_with_optional_pet();
    let rt = Runtime::new(&g);
    let q = compile_query(
        "MATCH (u: User {name: 'Bob'}) \
         OPTIONAL MATCH (u)-[:OWNS]->(p: Pet) \
         RETURN u.name, p.name",
    )
    .unwrap();
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => {
            assert_eq!(
                rs,
                vec![vec![Value::Str("Bob".into()), Value::Null]],
                "Bob exists, has no pet → one row with p.name = null",
            );
        }
        _ => panic!("expected projected"),
    }
}

/// `WHERE` inside an OPTIONAL filters BEFORE the outer-join, so a row that
/// would have matched but fails the predicate falls into unsuccess (not
/// dropped from the outer table).
#[test]
fn runtime_optional_where_runs_before_outer_join() {
    let g = graph_with_optional_pet();
    let rows = run_projected(
        &g,
        "MATCH (u: User) \
         OPTIONAL MATCH (u)-[:OWNS]->(p: Pet) WHERE p.name = 'Garfield' \
         RETURN u.name, p.name",
    );
    assert_eq!(rows.len(), 2, "outer table preserved despite filter");
    for r in &rows {
        assert_eq!(
            r[1],
            Value::Null,
            "no pet named Garfield exists → every row falls into unsuccess",
        );
    }
}

// =====================================================================
// Regression: LIMIT must be honored when the chain is a single OPTIONAL
// with no following match. ISO §14.3-14.4 SR4 allows OPTIONAL as the
// only statement; without RETURN the Raw path used to skip the
// truncation entirely because the for-loop never ran.
// =====================================================================

#[test]
fn runtime_single_optional_match_honors_limit_without_return() {
    let g = fraud_graph();
    let rt = Runtime::new(&g);
    // 5 nodes in fraud.json. With limit=2 the binding table must shrink.
    let q = compile_query("OPTIONAL MATCH (x) LIMIT 2").unwrap();
    match rt.run_query(&q, 0) {
        QueryResult::Raw(ir) => assert_eq!(
            ir.rows.len(),
            2,
            "single-OPTIONAL Raw path must respect LIMIT (was emitting all rows)",
        ),
        _ => panic!("expected Raw result for query without RETURN"),
    }
}

#[test]
fn runtime_single_simple_match_honors_limit_without_return() {
    // Same regression for a single Simple match: this path collapses to
    // run_path_pattern which honors limit, but pin it explicitly so the
    // single-match limit invariant is covered for both shapes.
    let g = fraud_graph();
    let rt = Runtime::new(&g);
    let q = compile_query("MATCH (x) LIMIT 2").unwrap();
    match rt.run_query(&q, 0) {
        QueryResult::Raw(ir) => assert_eq!(ir.rows.len(), 2),
        _ => panic!("expected Raw result"),
    }
}

#[test]
fn runtime_single_optional_match_caller_limit_also_honored() {
    // The runtime cap (passed via `Runtime::run_query(_, cap)`) must also
    // shrink a single-OPTIONAL Raw result. Combine_limits picks the
    // smaller of in-query LIMIT and caller cap; here only the caller cap
    // is set.
    let g = fraud_graph();
    let rt = Runtime::new(&g);
    let q = compile_query("OPTIONAL MATCH (x)").unwrap();
    match rt.run_query(&q, 3) {
        QueryResult::Raw(ir) => assert_eq!(ir.rows.len(), 3),
        _ => panic!("expected Raw result"),
    }
}
