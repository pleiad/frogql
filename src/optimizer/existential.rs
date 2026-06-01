//! Folding pass for `EXISTS` / `NOT EXISTS` predicates whose body is
//! statically empty.
//!
//! Mirrors the rewrite rules `ExistsEmpty` and `NotExistsEmpty` from the
//! type-system extension: when the typechecker proves the body
//! unsatisfiable, the predicate collapses to a literal — `false` for
//! `Exists`, `true` for `NotExists`. The pass walks every `Expr` in the
//! query (WHERE filters, GROUP BY, RETURN, and recursively the bodies
//! of nested existentials) and rewrites in place.
//!
//! The body's emptiness is decided by running the typechecker against
//! the supplied schema. Outer-scope correlation is not threaded into
//! the body for this pass, so we catch shape-driven emptiness (a label
//! absent from the schema, two property types that meet to bottom)
//! but miss emptiness that arises only after refining inner variables
//! against the outer environment. That refinement-aware case is left
//! for a future pass.

use crate::model::value::Value;
use crate::syntax::expr::Expr;
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::query::{Aggregator, MatchStatement, Query, ReturnItem};
use crate::typing::checker::Typechecker;
use crate::typing::variable_type::Schema;

/// Walk every `Expr` reachable from `q` and fold empty existentials.
/// Recurses into the body of each existential so deeply-nested
/// predicates are folded bottom-up; an inner constant cannot itself
/// drive the outer body to empty (the typechecker only tracks pattern
/// shape, not literal-Boolean propagation), but the recursion still
/// shrinks the AST.
pub fn fold_empty_existentials(q: &mut Query, schema: &Schema) {
    for m in &mut q.matches {
        let pattern = match m {
            MatchStatement::Simple { pattern, .. } | MatchStatement::Optional { pattern, .. } => {
                pattern
            }
        };
        fold_in_pattern(pattern, schema);
    }
    if let Some(group_by) = &mut q.group_by {
        for e in group_by {
            fold_in_expr(e, schema);
        }
    }
    if let Some(returns) = &mut q.returns {
        for item in returns {
            match item {
                ReturnItem::Expr { expr, .. } => fold_in_expr(expr, schema),
                ReturnItem::Aggregate { agg, .. } => match agg {
                    Aggregator::CountStar => {}
                    Aggregator::GeneralSet { expr, .. } => fold_in_expr(expr, schema),
                },
            }
        }
    }
}

fn fold_in_pattern(p: &mut PathPattern, schema: &Schema) {
    match p {
        PathPattern::Filter(inner, expr) => {
            fold_in_pattern(inner, schema);
            fold_in_expr(expr, schema);
        }
        PathPattern::Concat(p1, p2) | PathPattern::Union(p1, p2) | PathPattern::Join(p1, p2) => {
            fold_in_pattern(p1, schema);
            fold_in_pattern(p2, schema);
        }
        PathPattern::Repeat { pattern, .. } | PathPattern::Questioned(pattern) => {
            fold_in_pattern(pattern, schema);
        }
        PathPattern::Node(_)
        | PathPattern::EdgeRight(_)
        | PathPattern::EdgeLeft(_)
        | PathPattern::EdgeUndirected(_)
        | PathPattern::EdgeAnyDirection(_) => {}
    }
}

fn fold_in_expr(e: &mut Expr, schema: &Schema) {
    // Recurse into children first so any nested existentials are
    // folded before the parent decides on its own body.
    match e {
        Expr::Exists { body } | Expr::NotExists { body } => {
            fold_empty_existentials(body, schema);
        }
        Expr::Binop { left, right, .. } => {
            fold_in_expr(left, schema);
            fold_in_expr(right, schema);
        }
        Expr::Unop { operand, .. } | Expr::IsNull { operand, .. } => {
            fold_in_expr(operand, schema);
        }
        Expr::FieldAccess { base, .. } => {
            fold_in_expr(base, schema);
        }
        Expr::Coalesce(args) => {
            for a in args {
                fold_in_expr(a, schema);
            }
        }
        Expr::Const(_) | Expr::Var(_) | Expr::AttrLookup { .. } | Expr::Type(_) => {}
    }

    // Decide and rewrite. The two reads of `e` are sequential — the
    // immutable read in `match` ends before the assignment.
    let is_empty = match e {
        Expr::Exists { body } | Expr::NotExists { body } => {
            let mut tc = Typechecker::new(schema.clone());
            tc.check_query(body).empty
        }
        _ => return,
    };
    if is_empty {
        let value = matches!(e, Expr::NotExists { .. });
        *e = Expr::Const(Value::Bool(value));
    }
}
