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
use crate::syntax::query::{MatchStatement, Query};

pub fn elaborate_query(q: Query) -> Query {
    let fresh = FreshVars::new(&q);
    let matches = q
        .matches
        .into_iter()
        .map(|m| match m {
            MatchStatement::Simple { prefix, pattern } => MatchStatement::Simple {
                prefix,
                pattern: elaborate_pattern(pattern, &fresh),
            },
            MatchStatement::Optional { prefix, pattern } => MatchStatement::Optional {
                prefix,
                pattern: elaborate_pattern(pattern, &fresh),
            },
        })
        .collect();
    Query { matches, ..q }
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
        PathPattern::Filter(p, e) => PathPattern::Filter(Box::new(elaborate_pattern(*p, fresh)), e),
        PathPattern::Repeat { pattern, lb, ub } => PathPattern::Repeat {
            pattern: Box::new(elaborate_pattern(*pattern, fresh)),
            lb,
            ub,
        },
        PathPattern::Questioned(p) => {
            PathPattern::Questioned(Box::new(elaborate_pattern(*p, fresh)))
        }
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
        PathPattern::Repeat { pattern, .. } | PathPattern::Questioned(pattern) => {
            visit(pattern, set)
        }
    }
}

// Keep the Value import visible so rustc doesn't warn once phase-1 literals
// (list/record constants) land here.
#[allow(dead_code)]
fn _use_value(_v: Value) {}
