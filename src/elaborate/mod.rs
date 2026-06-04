//! Elaboration: the phase between parse and optimize.
//!
//! Rewrites surface syntax into the core AST that the typechecker and runtime
//! expect. Distinct from the optimizer — elaboration is *semantic lowering*,
//! not a performance-preserving transform. Anything the ISO GQL standard
//! defines as syntactic sugar over the core semantics lives here.
//!
//! Currently implemented:
//! - Hoist `{name: expr}` value filters inside descriptors into WHERE.
//!   `(x:L {k: v})` becomes `(x:L) WHERE x.k = v`.
//!
//! Future home for: record/list literal normalization, path-binding sugar,
//! default-MATCH insertion, etc.

use std::cell::Cell;

use crate::model::value::Value;
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr};
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::query::{MatchStatement, Query, ReturnItem, SortKey};

pub fn elaborate_query(q: Query) -> Query {
    let fresh = FreshVars::new(&q);
    let matches: Vec<MatchStatement> = q
        .matches
        .into_iter()
        .map(|m| match m {
            MatchStatement::Simple { pattern } => MatchStatement::Simple {
                pattern: elaborate_pattern(pattern, &fresh),
            },
            MatchStatement::Optional { pattern } => MatchStatement::Optional {
                pattern: elaborate_pattern(pattern, &fresh),
            },
        })
        .collect();
    // Elaborate subquery bodies that live inside RETURN items and ORDER BY
    // keys (VALUE / EXISTS) so their own descriptors' value filters lower.
    let returns = q.returns.map(|items| {
        items
            .into_iter()
            .map(|it| match it {
                ReturnItem::Expr { expr, alias } => ReturnItem::Expr {
                    expr: elaborate_expr(expr),
                    alias,
                },
                other => other,
            })
            .collect()
    });
    let order_by = q.order_by.map(|specs| {
        specs
            .into_iter()
            .map(|mut s| {
                if let SortKey::Expr(e) = s.key {
                    s.key = SortKey::Expr(elaborate_expr(e));
                }
                s
            })
            .collect()
    });
    // Resolve `GROUP BY <RETURN alias>` against the (already elaborated)
    // RETURN list. ISO restricts a grouping element to a binding-variable
    // reference; this lowers the common convenience form `... AS k ... GROUP
    // BY k` to the underlying expression so both the typechecker's functional
    // dependency check and the runtime see a key evaluable over the binding
    // table. A name that is a binding variable shadows any same-named alias.
    let group_by = q.group_by.map(|items| {
        let mut bound = std::collections::HashSet::new();
        for m in &matches {
            visit(m.pattern(), &mut bound);
        }
        items
            .into_iter()
            .map(|g| resolve_group_key(g, &bound, returns.as_deref()))
            .collect()
    });
    Query {
        matches,
        returns,
        group_by,
        order_by,
        ..q
    }
}

/// Lower one GROUP BY element. A bare `Expr::Var(name)` that is *not* a
/// binding variable but *is* a RETURN alias is replaced by the aliased
/// expression (already elaborated). Binding variables and spelled-out
/// expressions pass through (the latter still elaborated for subquery
/// bodies). Aggregate-aliased or unresolved names are left as `Var` so the
/// typechecker reports them.
fn resolve_group_key(
    g: Expr,
    bound: &std::collections::HashSet<String>,
    returns: Option<&[ReturnItem]>,
) -> Expr {
    if let Expr::Var(name) = &g {
        if bound.contains(name) {
            return g;
        }
        if let Some(items) = returns {
            if let Some(ReturnItem::Expr { expr, .. }) =
                items.iter().find(|it| it.alias() == Some(name.as_str()))
            {
                return expr.clone();
            }
        }
        return g;
    }
    elaborate_expr(g)
}

/// Elaborate an expression, recursing into any subquery body (EXISTS /
/// NOT EXISTS / VALUE) via `elaborate_query` so the body's own descriptor
/// value filters are lowered. Identity for subquery-free expressions.
pub fn elaborate_expr(e: Expr) -> Expr {
    match e {
        Expr::Exists { body } => Expr::Exists {
            body: Box::new(elaborate_query(*body)),
        },
        Expr::NotExists { body } => Expr::NotExists {
            body: Box::new(elaborate_query(*body)),
        },
        Expr::ValueSubquery { body } => Expr::ValueSubquery {
            body: Box::new(elaborate_query(*body)),
        },
        Expr::Binop { op, left, right } => Expr::Binop {
            op,
            left: Box::new(elaborate_expr(*left)),
            right: Box::new(elaborate_expr(*right)),
        },
        Expr::Unop { op, operand } => Expr::Unop {
            op,
            operand: Box::new(elaborate_expr(*operand)),
        },
        Expr::IsNull { operand, negated } => Expr::IsNull {
            operand: Box::new(elaborate_expr(*operand)),
            negated,
        },
        Expr::FieldAccess { base, field } => Expr::FieldAccess {
            base: Box::new(elaborate_expr(*base)),
            field,
        },
        Expr::Coalesce(args) => Expr::Coalesce(args.into_iter().map(elaborate_expr).collect()),
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args.into_iter().map(elaborate_expr).collect(),
        },
        Expr::Record { fields } => Expr::Record {
            fields: fields
                .into_iter()
                .map(|(k, e)| (k, elaborate_expr(e)))
                .collect(),
        },
        Expr::Case {
            branches,
            else_expr,
        } => Expr::Case {
            branches: branches
                .into_iter()
                .map(|(c, v)| (elaborate_expr(c), elaborate_expr(v)))
                .collect(),
            else_expr: else_expr.map(|e| Box::new(elaborate_expr(*e))),
        },
        Expr::ListComprehension {
            var,
            source,
            filter,
            body,
        } => Expr::ListComprehension {
            var,
            source: Box::new(elaborate_expr(*source)),
            filter: filter.map(|f| Box::new(elaborate_expr(*f))),
            body: Box::new(elaborate_expr(*body)),
        },
        other => other,
    }
}

pub fn elaborate_pattern(p: PathPattern, fresh: &FreshVars) -> PathPattern {
    match p {
        PathPattern::Node(desc_opt) => lower_node_or_edge(desc_opt, PathPattern::Node, fresh),
        PathPattern::EdgeRight(desc_opt) => {
            lower_node_or_edge(desc_opt, PathPattern::EdgeRight, fresh)
        }
        PathPattern::EdgeLeft(desc_opt) => {
            lower_node_or_edge(desc_opt, PathPattern::EdgeLeft, fresh)
        }
        PathPattern::EdgeUndirected(desc_opt) => {
            lower_node_or_edge(desc_opt, PathPattern::EdgeUndirected, fresh)
        }
        PathPattern::EdgeAnyDirection(desc_opt) => {
            lower_node_or_edge(desc_opt, PathPattern::EdgeAnyDirection, fresh)
        }
        PathPattern::Concat(p1, p2) => PathPattern::Concat(
            Box::new(elaborate_pattern(*p1, fresh)),
            Box::new(elaborate_pattern(*p2, fresh)),
        ),
        PathPattern::Union(p1, p2) => PathPattern::Union(
            Box::new(elaborate_pattern(*p1, fresh)),
            Box::new(elaborate_pattern(*p2, fresh)),
        ),
        PathPattern::Join(p1, p2) => PathPattern::Join(
            Box::new(elaborate_pattern(*p1, fresh)),
            Box::new(elaborate_pattern(*p2, fresh)),
        ),
        PathPattern::Filter(p, e) => {
            PathPattern::Filter(Box::new(elaborate_pattern(*p, fresh)), elaborate_expr(e))
        }
        PathPattern::Repeat { pattern, lb, ub } => PathPattern::Repeat {
            pattern: Box::new(elaborate_pattern(*pattern, fresh)),
            lb,
            ub,
        },
        PathPattern::Questioned(p) => {
            PathPattern::Questioned(Box::new(elaborate_pattern(*p, fresh)))
        }
        PathPattern::Selected { prefix, pattern } => PathPattern::Selected {
            prefix,
            pattern: Box::new(elaborate_pattern(*pattern, fresh)),
        },
        PathPattern::Named { var, pattern } => PathPattern::Named {
            var,
            pattern: Box::new(elaborate_pattern(*pattern, fresh)),
        },
    }
}

/// If the descriptor carries value filters, hoist them: assign a fresh variable
/// if needed, clear the filter list, wrap the pattern in a `Filter` node
/// containing `var.attr = expr AND ...`.
fn lower_node_or_edge<F>(desc_opt: Option<Descriptor>, ctor: F, fresh: &FreshVars) -> PathPattern
where
    F: FnOnce(Option<Descriptor>) -> PathPattern,
{
    match desc_opt {
        None => ctor(None),
        Some(mut desc) if desc.value_filters.is_empty() => {
            desc.value_filters.clear();
            ctor(Some(desc))
        }
        Some(mut desc) => {
            let filters = std::mem::take(&mut desc.value_filters);
            if desc.var.is_none() {
                desc.var = Some(fresh.next());
            }
            let var = desc.var.clone().unwrap();
            let cond = filters_to_expr(&var, filters);
            PathPattern::Filter(Box::new(ctor(Some(desc))), cond)
        }
    }
}

/// Build `var.a = e1 AND var.b = e2 AND ...` from a list of (attr, expr) pairs.
fn filters_to_expr(var: &str, filters: Vec<(String, Expr)>) -> Expr {
    let mut iter = filters.into_iter();
    let (first_attr, first_val) = iter.next().expect("at least one filter");
    let mut acc = eq(var, &first_attr, first_val);
    for (attr, val) in iter {
        acc = Expr::Binop {
            op: BinOp::And,
            left: Box::new(acc),
            right: Box::new(eq(var, &attr, val)),
        };
    }
    acc
}

fn eq(var: &str, attr: &str, value: Expr) -> Expr {
    Expr::Binop {
        op: BinOp::Eq,
        left: Box::new(Expr::AttrLookup {
            var: var.to_string(),
            attr: attr.to_string(),
        }),
        right: Box::new(value),
    }
}

/// Fresh variable generator that avoids collisions with any name already used in
/// the query. Names look like `_gqlite_elab_0`, `_gqlite_elab_1`, ...
pub struct FreshVars {
    counter: Cell<usize>,
    taken: std::collections::HashSet<String>,
}

impl FreshVars {
    pub fn new(q: &Query) -> Self {
        let mut taken = std::collections::HashSet::new();
        for m in &q.matches {
            visit(m.pattern(), &mut taken);
        }
        Self {
            counter: Cell::new(0),
            taken,
        }
    }

    pub fn next(&self) -> String {
        loop {
            let i = self.counter.get();
            self.counter.set(i + 1);
            let name = format!("_gqlite_elab_{i}");
            if !self.taken.contains(&name) {
                return name;
            }
        }
    }
}

fn visit(p: &PathPattern, set: &mut std::collections::HashSet<String>) {
    let push_desc = |d: &Descriptor, set: &mut std::collections::HashSet<String>| {
        if let Some(v) = &d.var {
            set.insert(v.clone());
        }
    };
    match p {
        PathPattern::Node(Some(d))
        | PathPattern::EdgeRight(Some(d))
        | PathPattern::EdgeLeft(Some(d))
        | PathPattern::EdgeUndirected(Some(d))
        | PathPattern::EdgeAnyDirection(Some(d)) => push_desc(d, set),
        PathPattern::Node(None)
        | PathPattern::EdgeRight(None)
        | PathPattern::EdgeLeft(None)
        | PathPattern::EdgeUndirected(None)
        | PathPattern::EdgeAnyDirection(None) => {}
        PathPattern::Concat(p1, p2) | PathPattern::Union(p1, p2) | PathPattern::Join(p1, p2) => {
            visit(p1, set);
            visit(p2, set);
        }
        PathPattern::Filter(p, _) => visit(p, set),
        PathPattern::Named { var, pattern } => {
            set.insert(var.clone());
            visit(pattern, set);
        }
        PathPattern::Repeat { pattern, .. }
        | PathPattern::Questioned(pattern)
        | PathPattern::Selected { pattern, .. } => visit(pattern, set),
    }
}

// Keep the Value import visible so rustc doesn't warn once phase-1 literals
// (list/record constants) land here.
#[allow(dead_code)]
fn _use_value(_v: Value) {}
