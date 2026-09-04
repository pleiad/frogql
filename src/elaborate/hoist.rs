//! Filter hoisting: place every predicate where the standard says it is
//! evaluated, rather than where it was written (issue #99).
//!
//! An inline descriptor lowers to a filter on the element it decorates:
//! `(m:N {k: n.k})` becomes `(m:N) WHERE m.k = n.k`. That is the right
//! *shape*, but not the right *scope*. ISO/IEC 39075:2024 §16.4 GR 9
//! evaluates a graph pattern's element filters over the combined multi-path
//! binding — the product of every operand's candidate paths — and §22.6
//! widens the search space to that whole binding when a referenced variable
//! is not declared in the local path pattern. So `n` is visible from `m`'s
//! filter even though `n` belongs to a sibling operand. Likewise §14.3 makes
//! a query a linear sequence of statements whose working table flows from
//! one to the next, so a later `MATCH` sees what an earlier one bound
//! (§16.3 SR 25).
//!
//! Evaluating such a filter against its own operand's rows finds the
//! variable unbound, which the runtime reports as a failure and swallows as
//! a dropped row — a silently empty result. This pass moves each filter up
//! to the smallest enclosing pattern that binds everything it references,
//! which restores the standard's scope in a single rewrite that both the
//! typechecker and the runtime then see:
//!
//! ```text
//!   Join(n, Filter(m, m.k = n.k))  ==>  Filter(Join(n, m), m.k = n.k)
//! ```
//!
//! When no subpattern of one clause can host a filter, the pass merges that
//! clause into the run of preceding simple clauses — `MATCH A MATCH B` and
//! `MATCH A, B` are the same natural join, which is why the runtime already
//! collapses the chain itself when nothing is optional.
//!
//! Three boundaries are deliberately opaque, because a filter that crosses
//! them means something different on the other side:
//!
//! - a **union arm** (`Union`) — a predicate on one alternative must not
//!   apply to the other;
//! - a **repetition** (`Repeat`, `Questioned`) — it constrains each
//!   application, not the whole walk;
//! - a **selected path** (`Selected`, ISO §16.6) — selection ranks the paths
//!   its own pattern produces, and the prefix's isolation rules (SR 5–8)
//!   keep interior variables from leaking either way.
//!
//! An `OPTIONAL MATCH` clause is opaque too: hoisting its predicate to the
//! chain would turn its left join into an inner one. Its correlation is
//! handled where it belongs — per outer row, in the runtime.
//!
//! A filter this pass cannot place is left exactly where it was, so a
//! genuine reference to a variable nothing binds still reaches the
//! typechecker as an error instead of quietly disappearing.

use std::collections::{BTreeSet, HashSet};

use crate::syntax::expr::Expr;
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::query::{MatchStatement, Query};

/// Rewrite each clause so its filters sit at the smallest subpattern that
/// binds them, merging consecutive simple clauses when a filter reaches
/// across the boundary between them.
pub fn hoist_query(q: Query) -> Query {
    let mut out: Vec<MatchStatement> = Vec::with_capacity(q.matches.len());

    for m in q.matches {
        let optional = m.is_optional();
        let (mut pattern, mut pending) = hoist_pattern(match m {
            MatchStatement::Simple { pattern } | MatchStatement::Optional { pattern } => pattern,
        });

        // A filter no subpattern of this clause can host may still be
        // hostable by this clause joined with the ones before it. The join
        // is only available for a simple clause following simple clauses:
        // absorbing an optional clause would drop its null-extended rows,
        // and absorbing across a `Selected` operand would erase the prefix
        // boundary the runtime keeps isolated.
        if !optional {
            while !pending.is_empty() {
                let can_merge = matches!(out.last(), Some(MatchStatement::Simple { pattern: prev })
                    if !prev.has_selected() && !pattern.has_selected());
                if !can_merge {
                    break;
                }
                let prev = match out.pop() {
                    Some(MatchStatement::Simple { pattern }) => pattern,
                    _ => unreachable!("guarded by can_merge"),
                };
                let joined = PathPattern::Join(Box::new(prev), Box::new(pattern));
                (pattern, pending) = place(joined, pending);
            }
        }

        // Whatever is still unplaceable goes back where the parser put it,
        // so the typechecker reports it rather than the runtime dropping
        // rows for a predicate nobody evaluated.
        pattern = pending
            .into_iter()
            .fold(pattern, |acc, e| PathPattern::Filter(Box::new(acc), e));

        out.push(if optional {
            MatchStatement::Optional { pattern }
        } else {
            MatchStatement::Simple { pattern }
        });
    }

    Query { matches: out, ..q }
}

/// Hoist within one pattern. Returns the rewritten pattern plus the filters
/// that reference variables no subpattern of it binds.
fn hoist_pattern(p: PathPattern) -> (PathPattern, Vec<Expr>) {
    match p {
        PathPattern::Filter(inner, expr) => {
            let (inner, mut pending) = hoist_pattern(*inner);
            pending.push(expr);
            place(inner, pending)
        }

        PathPattern::Join(a, b) => {
            let (a, mut pending) = hoist_pattern(*a);
            let (b, pb) = hoist_pattern(*b);
            pending.extend(pb);
            place(PathPattern::Join(Box::new(a), Box::new(b)), pending)
        }

        PathPattern::Concat(a, b) => {
            let (a, mut pending) = hoist_pattern(*a);
            let (b, pb) = hoist_pattern(*b);
            pending.extend(pb);
            place(PathPattern::Concat(Box::new(a), Box::new(b)), pending)
        }

        // Transparent to scope, and it binds one more name: a filter over
        // the path variable can be hosted right here.
        PathPattern::Named { var, pattern } => {
            let (inner, pending) = hoist_pattern(*pattern);
            place(
                PathPattern::Named {
                    var,
                    pattern: Box::new(inner),
                },
                pending,
            )
        }

        // Opaque boundaries: hoist *inside* them, but nothing escapes. An
        // unplaceable filter is restored at the boundary's own root, which
        // is where it already was.
        PathPattern::Union(a, b) => {
            let a = sealed(*a);
            let b = sealed(*b);
            (PathPattern::Union(Box::new(a), Box::new(b)), Vec::new())
        }
        PathPattern::Repeat { pattern, lb, ub } => (
            PathPattern::Repeat {
                pattern: Box::new(sealed(*pattern)),
                lb,
                ub,
            },
            Vec::new(),
        ),
        PathPattern::Questioned(inner) => (
            PathPattern::Questioned(Box::new(sealed(*inner))),
            Vec::new(),
        ),
        PathPattern::Selected { prefix, pattern } => (
            PathPattern::Selected {
                prefix,
                pattern: Box::new(sealed(*pattern)),
            },
            Vec::new(),
        ),

        leaf => (leaf, Vec::new()),
    }
}

/// Hoist inside a subtree that filters may not leave, re-attaching anything
/// unplaceable at its root.
fn sealed(p: PathPattern) -> PathPattern {
    let (inner, pending) = hoist_pattern(p);
    pending
        .into_iter()
        .fold(inner, |acc, e| PathPattern::Filter(Box::new(acc), e))
}

/// Attach every pending filter that `node` binds all the variables of,
/// innermost-first; hand the rest back to the caller to place higher up.
fn place(node: PathPattern, pending: Vec<Expr>) -> (PathPattern, Vec<Expr>) {
    if pending.is_empty() {
        return (node, pending);
    }
    let bound = declared_vars(&node);
    let (here, rest): (Vec<Expr>, Vec<Expr>) = pending
        .into_iter()
        .partition(|e| referenced(e).is_subset(&bound));
    let node = here
        .into_iter()
        .fold(node, |acc, e| PathPattern::Filter(Box::new(acc), e));
    (node, rest)
}

fn referenced(e: &Expr) -> HashSet<String> {
    let mut acc = BTreeSet::new();
    e.referenced_vars(&mut acc);
    acc.into_iter().collect()
}

/// Every name the pattern binds: its element variables plus any path
/// variable declared inside it.
fn declared_vars(p: &PathPattern) -> HashSet<String> {
    let mut set = p.freevars();
    collect_path_vars(p, &mut set);
    set
}

fn collect_path_vars(p: &PathPattern, acc: &mut HashSet<String>) {
    match p {
        PathPattern::Named { var, pattern } => {
            acc.insert(var.clone());
            collect_path_vars(pattern, acc);
        }
        PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
            collect_path_vars(a, acc);
            collect_path_vars(b, acc);
        }
        PathPattern::Filter(inner, _)
        | PathPattern::Questioned(inner)
        | PathPattern::Repeat { pattern: inner, .. }
        | PathPattern::Selected { pattern: inner, .. } => collect_path_vars(inner, acc),
        _ => {}
    }
}
