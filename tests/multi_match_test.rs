//! Tests for the structural multi-MATCH refactor (ISO §14.3-14.4).
//!
//! Constructs `Query` values manually with `len(matches) > 1` to exercise
//! the multi-MATCH path without going through the parser. Without this,
//! the central claim of the refactor — that `Query::collapsed_pattern()`
//! is sound for any chain of Simple match statements via `PathPattern::Join`
//! — would be unverified by example tests.

use std::path::Path;

use gqlrust::elaborate::elaborate_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::parser;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::syntax::path_pattern::PathPattern;
use gqlrust::syntax::query::{MatchStatement, Query};
use gqlrust::typing::checker::Typechecker;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::variable_type::VariableType;

fn fraud_graph() -> MemoryGraphStore {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    MemoryGraphStore::from_file(&p).unwrap()
}

fn multi_match_query(patterns: &[&str]) -> Query {
    let matches: Vec<MatchStatement> = patterns
        .iter()
        .map(|s| MatchStatement::Simple {
            pattern: parser::parse(s).expect("parse failed"),
        })
        .collect();
    elaborate_query(Query {
        matches,
        group_by: None,
        returns: None,
        distinct: false,
        order_by: None,
        limit: None,
    })
}

#[test]
fn collapse_single_match_is_identity() {
    let q = multi_match_query(&["(x)"]);
    assert!(matches!(q.collapsed_pattern(), PathPattern::Node(_)));
}

#[test]
fn collapse_two_matches_is_join() {
    let q = multi_match_query(&["(x)", "(y)"]);
    let PathPattern::Join(left, right) = q.collapsed_pattern() else {
        panic!("expected Join");
    };
    assert!(matches!(*left, PathPattern::Node(_)));
    assert!(matches!(*right, PathPattern::Node(_)));
}

/// Left-assoc fold: `Join(Join(p1, p2), p3)` is what `pattern_extract`
/// flattens elsewhere in the runtime.
#[test]
fn collapse_three_matches_is_left_associative_join() {
    let q = multi_match_query(&["(x)", "(y)", "(z)"]);
    let PathPattern::Join(left, right_outer) = q.collapsed_pattern() else {
        panic!("expected outer Join");
    };
    assert!(matches!(*right_outer, PathPattern::Node(_)));
    assert!(matches!(*left, PathPattern::Join(_, _)));
}

#[test]
fn runtime_disjoint_multi_match_matches_comma_join() {
    let g = fraud_graph();
    let runtime = Runtime::new(&g);

    let multi = multi_match_query(&["(x)", "(y)"]);
    let multi_rows = runtime.run(&multi.collapsed_pattern()).rows.len();

    let single = multi_match_query(&["(x), (y)"]);
    let single_rows = runtime.run(&single.collapsed_pattern()).rows.len();

    assert_eq!(multi_rows, single_rows);
    assert_eq!(multi_rows, 25, "fraud.json has 5 nodes; cartesian 5×5 = 25");
}

/// `MATCH (x: Account) MATCH (x)` binds `x` once across both matches.
/// Cartesian would be 4×5=20; natural join is 4.
#[test]
fn runtime_shared_var_multi_match_is_natural_join() {
    let g = fraud_graph();
    let runtime = Runtime::new(&g);
    let q = multi_match_query(&["(x: Account)", "(x)"]);
    let rows = runtime.run(&q.collapsed_pattern()).rows.len();
    assert_eq!(rows, 4);
}

#[test]
fn typecheck_accepts_shared_var_across_matches() {
    let q = multi_match_query(&["(x: Account)", "(x)"]);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    assert!(r.ok, "errors: {:?}", tc.errors);
    assert!(tc.errors.is_empty());
}

// ===== Meet semantics on shared variables across matches =====
//
// `Typechecker::check_query` walks `q.collapsed_pattern()`. The Concat/Join
// arm calls `TypeEnvironment::meet` on the two sub-environments, which for
// every shared variable computes `VariableType::meet`. These tests pin the
// shape of the resulting env, not just the absence of errors — they are the
// thing the prof asked for.

fn extract_label(t: &VariableType) -> Option<&LabelType> {
    match t {
        VariableType::Node(desc) => Some(&desc.label),
        _ => None,
    }
}

/// Compatible labels collapse to the more specific one. `(x: Account)`
/// meeting `(x)` (Star label) refines `x` to `Label("Account")`.
#[test]
fn meet_shared_var_takes_label_intersection() {
    let q = multi_match_query(&["(x: Account)", "(x)"]);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    assert!(r.ok, "errors: {:?}", tc.errors);

    let x_ty = r.env.get("x").expect("x must be bound");
    let label = extract_label(x_ty).expect("x must be a Node");
    assert!(
        matches!(label, LabelType::Label(s) if s == "Account"),
        "x label should be Account after meet, got {label:?}"
    );
}

/// Disjoint variables across matches are unaffected by the meet. Each
/// keeps its own type; `meet` only touches keys present in both envs.
#[test]
fn meet_disjoint_vars_each_keep_own_type() {
    let q = multi_match_query(&["(x: Account)", "(y: Person)"]);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    assert!(r.ok, "errors: {:?}", tc.errors);

    let x_label = extract_label(r.env.get("x").unwrap()).unwrap();
    let y_label = extract_label(r.env.get("y").unwrap()).unwrap();
    assert!(matches!(x_label, LabelType::Label(s) if s == "Account"));
    assert!(matches!(y_label, LabelType::Label(s) if s == "Person"));
}

/// Three-way chain converges via left-associative meet:
/// `meet(meet(Account, Star), Account) = Account`.
#[test]
fn meet_three_way_match_chain_converges() {
    let q = multi_match_query(&["(x: Account)", "(x)", "(x: Account)"]);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    assert!(r.ok, "errors: {:?}", tc.errors);

    let x_label = extract_label(r.env.get("x").unwrap()).unwrap();
    assert!(matches!(x_label, LabelType::Label(s) if s == "Account"));
}

/// Property filters across matches don't affect the env binding shape
/// (they get hoisted to `Filter` wrappers by elaborate, not into the
/// descriptor's PropertyType). What matters is that the meet on the
/// shared var still goes through cleanly without collapsing to Zero.
#[test]
fn meet_shared_var_with_property_filters_does_not_collapse() {
    let q = multi_match_query(&[
        "(x: Account {owner: 'Aretha'})",
        "(x: Account {isBlocked: false})",
    ]);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    assert!(r.ok, "errors: {:?}", tc.errors);

    let x_ty = r.env.get("x").expect("x must be bound");
    assert!(!x_ty.is_empty(), "meet must not collapse x to Zero");
    let label = extract_label(x_ty).expect("x must be a Node");
    assert!(matches!(label, LabelType::Label(s) if s == "Account"));
}

// ===== Runtime: shared-var natural join, beyond the 2-way case =====

/// Three-way shared `x` with one constraining clause: each MATCH that
/// further restricts `x` only filters rows. Final count is the number of
/// Account nodes (4), regardless of how many free `(x)` matches follow.
#[test]
fn runtime_three_way_shared_var_natural_join() {
    let g = fraud_graph();
    let runtime = Runtime::new(&g);
    let q = multi_match_query(&["(x: Account)", "(x)", "(x)"]);
    let rows = runtime.run(&q.collapsed_pattern()).rows.len();
    assert_eq!(rows, 4);
}

/// Constraining match comes second: `MATCH (x) MATCH (x: Account)` yields
/// the same row count as the reverse order. Natural join is commutative
/// over Simple matches, so order should not matter.
#[test]
fn runtime_constraining_match_order_independent() {
    let g = fraud_graph();
    let runtime = Runtime::new(&g);

    let forward = multi_match_query(&["(x: Account)", "(x)"]);
    let reverse = multi_match_query(&["(x)", "(x: Account)"]);

    let n_forward = runtime.run(&forward.collapsed_pattern()).rows.len();
    let n_reverse = runtime.run(&reverse.collapsed_pattern()).rows.len();
    assert_eq!(n_forward, n_reverse);
    assert_eq!(n_forward, 4);
}

// ===== End-to-end: multi-MATCH + shared var + RETURN =====
//
// By the time RETURN runs, the natural join on shared variables has
// already happened in the runtime (collapsed pattern → IntermediateResult
// → one binding per variable per row). RETURN just projects each row's
// Assignment, the same code path used for single-MATCH. These tests pin
// that the projected values are exactly what we expect — no double
// counting from the cartesian product, no NULL on shared bindings.

fn run_compiled(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = gqlrust::compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected Projected, got {other:?}"),
    }
}

/// Multi-MATCH with shared `x` projects each unified value once, not the
/// 4×5=20 cartesian. Returns the four Account owners.
#[test]
fn return_shared_var_projects_natural_join_values() {
    let g = fraud_graph();
    let rows = run_compiled(&g, "MATCH (x: Account) MATCH (x) RETURN x.owner");
    assert_eq!(rows.len(), 4);

    let mut owners: Vec<String> = rows
        .into_iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            v => panic!("expected string owner, got {v:?}"),
        })
        .collect();
    owners.sort();
    assert_eq!(owners, vec!["Aretha", "Jay", "Mike", "Scott"]);
}

/// Multi-MATCH binds two distinct variables, each from its own clause;
/// RETURN projects both. Row count is the cartesian (5×4=20 because all
/// nodes paired with each Account), values come from independent vars.
#[test]
fn return_disjoint_vars_projects_both() {
    let g = fraud_graph();
    let rows = run_compiled(&g, "MATCH (x) MATCH (y: Account) RETURN x.owner, y.owner");
    assert_eq!(rows.len(), 5 * 4);
    for row in &rows {
        assert_eq!(row.len(), 2);
        // Each row carries an x-owner and a y-owner; both are strings.
        assert!(matches!(&row[0], Value::Str(_)));
        assert!(matches!(&row[1], Value::Str(_)));
    }
}

/// `MATCH (x: Account) MATCH (x) RETURN x.owner` ≡ `MATCH (x: Account)
/// RETURN x.owner` in projected output. The second `MATCH (x)` is a
/// no-op for results — it only re-asserts that x exists.
#[test]
fn return_redundant_match_does_not_alter_projection() {
    let g = fraud_graph();
    let with_redundant = run_compiled(&g, "MATCH (x: Account) MATCH (x) RETURN x.owner");
    let without = run_compiled(&g, "MATCH (x: Account) RETURN x.owner");
    let mut a: Vec<_> = with_redundant
        .into_iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            _ => panic!(),
        })
        .collect();
    let mut b: Vec<_> = without
        .into_iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            _ => panic!(),
        })
        .collect();
    a.sort();
    b.sort();
    assert_eq!(a, b);
}

/// `optimize_query` is private; round-trip via `compile_query_unchecked`
/// which calls it. After optimization, `matches.len() == 1`.
#[test]
fn optimize_query_collapses_multi_match_to_single_simple() {
    let q = multi_match_query(&["(x)", "(y)", "(z)"]);
    assert_eq!(q.matches.len(), 3);

    let input = format!("{}", q.collapsed_pattern());
    let optimized = gqlrust::compile_query_unchecked(&input).unwrap();

    assert_eq!(optimized.matches.len(), 1);
    assert!(matches!(
        &optimized.matches[0],
        MatchStatement::Simple { .. }
    ));
}
