//! Phase 2: optimiser folds of EXISTS / NOT EXISTS predicates whose
//! body is statically empty.
//!
//! `EXISTS L` collapses to `false` and `NOT EXISTS L` collapses to
//! `true` when the typechecker proves the body unsatisfiable.
//! Today emptiness is detected via refinement against an active
//! graph type (a label or property the schema rejects). The label
//! algebra does not currently normalise `A \\& !A` to bottom on its
//! own, so a schema is required to surface those cases.
//!
//! Bodies that are non-empty stay as their original `Expr::Exists` /
//! `Expr::NotExists` variants so the runtime evaluates them.

use gqlrust::model::value::Value;
use gqlrust::syntax::expr::Expr;
use gqlrust::syntax::path_pattern::PathPattern;
use gqlrust::syntax::query::{MatchStatement, ReturnItem};
use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::variable_type::{Schema, VariableType};

/// Active graph type with a single node entry `(:Account)` and no
/// edges. Anything outside that label refines to bottom.
fn account_only_schema() -> Schema {
    Schema::from_parts(
        vec![VariableType::Node(DescriptorType::new(
            LabelType::Label("Account".into()),
            PropertyType::closed_empty(),
        ))],
        vec![],
    )
}

/// Pull the WHERE expression out of the first match clause of a
/// post-optimiser query. Panics if the structure does not match,
/// which is also the assertion we want for a regression.
fn where_expr_with(schema: &Schema, input: &str) -> Expr {
    let q = gqlrust::compile_query_with(schema, input).expect("compile failed");
    let pat = match &q.matches[0] {
        MatchStatement::Simple { pattern, .. } | MatchStatement::Optional { pattern, .. } => {
            pattern
        }
    };
    match pat {
        PathPattern::Filter(_, e) => e.clone(),
        other => panic!("expected MATCH ... WHERE filter, got {other:?}"),
    }
}

fn where_expr(input: &str) -> Expr {
    let q = gqlrust::compile_query(input).expect("compile failed");
    let pat = match &q.matches[0] {
        MatchStatement::Simple { pattern, .. } | MatchStatement::Optional { pattern, .. } => {
            pattern
        }
    };
    match pat {
        PathPattern::Filter(_, e) => e.clone(),
        other => panic!("expected MATCH ... WHERE filter, got {other:?}"),
    }
}

#[test]
fn exists_against_schema_without_label_folds_to_false() {
    // Active graph type has only `Account` nodes; the body asks for a
    // node whose label is `Person`. Refinement against the schema
    // collapses the descriptor and the predicate folds.
    let e = where_expr_with(
        &account_only_schema(),
        "MATCH (x) WHERE EXISTS { (y: Person) } RETURN x.owner",
    );
    assert_eq!(e, Expr::Const(Value::Bool(false)));
}

#[test]
fn not_exists_against_schema_without_label_folds_to_true() {
    let e = where_expr_with(
        &account_only_schema(),
        "MATCH (x) WHERE NOT EXISTS { (y: Person) } RETURN x.owner",
    );
    assert_eq!(e, Expr::Const(Value::Bool(true)));
}

#[test]
fn exists_with_unknown_property_folds_to_false() {
    // Schema's `Account` node is a closed record with only
    // `owner` / `isBlocked`. A body that demands `nope int` cannot be
    // satisfied — the property type meets to bottom.
    let schema = Schema::from_parts(
        vec![VariableType::Node(DescriptorType::new(
            LabelType::Label("Account".into()),
            PropertyType::Closed(
                [
                    (
                        "owner".to_string(),
                        gqlrust::typing::simple_type::SimpleType::S,
                    ),
                    (
                        "isBlocked".to_string(),
                        gqlrust::typing::simple_type::SimpleType::B,
                    ),
                ]
                .into_iter()
                .collect(),
            ),
        ))],
        vec![],
    );
    let e = where_expr_with(
        &schema,
        "MATCH (x) WHERE EXISTS { (y: Account {nope int}) } RETURN x.owner",
    );
    assert_eq!(e, Expr::Const(Value::Bool(false)));
}

#[test]
fn exists_with_satisfiable_body_stays_unfolded() {
    // Body matches the schema; the typechecker cannot prove
    // emptiness and the predicate must survive into the runtime.
    let e = where_expr_with(
        &account_only_schema(),
        "MATCH (x) WHERE EXISTS { (y: Account) } RETURN x.owner",
    );
    assert!(
        matches!(e, Expr::Exists { .. }),
        "expected Expr::Exists, got {e:?}"
    );
}

#[test]
fn not_exists_with_satisfiable_body_stays_unfolded() {
    let e = where_expr_with(
        &account_only_schema(),
        "MATCH (x) WHERE NOT EXISTS { (y: Account) } RETURN x.owner",
    );
    assert!(
        matches!(e, Expr::NotExists { .. }),
        "expected Expr::NotExists, got {e:?}"
    );
}

#[test]
fn exists_under_star_schema_stays_unfolded() {
    // Star schema refines everything to star — no descriptor collapses
    // and no predicate folds. Documents the limit of the current pass.
    let e = where_expr("MATCH (x) WHERE EXISTS { (a)-[:KNOWS]->(b) } RETURN x.name");
    assert!(
        matches!(e, Expr::Exists { .. }),
        "expected Expr::Exists under Star, got {e:?}"
    );
}

#[test]
fn nested_inner_exists_folds_outer_stays() {
    // Inner body is empty against the schema; the folder rewrites the
    // inner predicate to `false`. The outer body, after the rewrite,
    // is `(a) WHERE false` — the typechecker does not propagate
    // boolean literal emptiness into pattern unsatisfiability today,
    // so the outer predicate keeps its `Expr::Exists` shape and gets
    // evaluated by the runtime.
    let e = where_expr_with(
        &account_only_schema(),
        "MATCH (x) WHERE EXISTS { \
           MATCH (a: Account) WHERE EXISTS { (z: Person) } \
         } RETURN x.owner",
    );
    let body = match e {
        Expr::Exists { body } => body,
        other => panic!("expected outer Expr::Exists, got {other:?}"),
    };
    let inner_pat = match &body.matches[0] {
        MatchStatement::Simple { pattern, .. } | MatchStatement::Optional { pattern, .. } => {
            pattern
        }
    };
    let inner_expr = match inner_pat {
        PathPattern::Filter(_, e) => e.clone(),
        other => panic!("expected MATCH ... WHERE inside outer EXISTS, got {other:?}"),
    };
    assert_eq!(
        inner_expr,
        Expr::Const(Value::Bool(false)),
        "inner EXISTS should have folded to false"
    );
}

#[test]
fn fold_runs_on_return_expression() {
    // EXISTS in RETURN, not just in WHERE. The walker reaches both.
    let q = gqlrust::compile_query_with(
        &account_only_schema(),
        "MATCH (x) RETURN EXISTS { (y: Person) } AS has_match",
    )
    .expect("compile failed");
    let returns = q.returns.expect("returns must be present");
    let ReturnItem::Expr { expr, .. } = &returns[0] else {
        panic!("expected expr return item, got {:?}", returns[0]);
    };
    assert_eq!(*expr, Expr::Const(Value::Bool(false)));
}
