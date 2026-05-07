//! Unroll bounded `Repeat { lb, ub: Some(ub) }` patterns into a Union of
//! concatenated copies, when the explosion is small and the inner pattern
//! has no named variables.
//!
//! The runtime treats `Repeat` as a hash-join cross-product over the
//! inner pattern's first→last node map (see `Runtime::run_repetition_*`).
//! That fallback never invokes LTJ on the chain that contains the
//! repetition, so predicate pushdown, btree-driven range filters, and
//! worst-case-optimal join evaluation are all disabled across the
//! repetition boundary. For LDBC IC2-style patterns with
//! `~[:knows]~{1, 2}` between two anchored nodes, the post-repetition
//! chain therefore enumerates ~200 k rows in seconds even when the
//! anchored endpoints are point-lookups.
//!
//! Unrolling rewrites `(P){lb, ub}` into
//! `(P)^lb | (P)^{lb+1} | ... | (P)^ub`. Each arm is a flat concat that
//! the LTJ extractor decomposes to triples and runs as a worst-case-
//! optimal multi-way join. The runtime's `Union` evaluator dispatches
//! each arm independently, so the overall cost becomes a sum of LTJs
//! instead of a hash-join over the repetition.
//!
//! Bounds:
//!   - `ub - lb + 1 <= MAX_UNROLL` keeps the AST blow-up bounded.
//!   - `P.freevars().is_empty()` avoids the `Group` semantics that named
//!     edge / node variables inside a repetition produce — each arm
//!     would need a different Group-of-k binding shape, which the
//!     pattern-level unroll can't express.
//!   - `lb >= 1` skips the length-0 base case (one row per graph node);
//!     splicing that into a surrounding concat would require synthesising
//!     a Node descriptor and is out of scope for the unroll pass.
//!   - `ub` must be bounded (`Some(_)`); unbounded `{n,}` is a transitive
//!     closure and stays on the existing fallback.
//!
//! Disable with `GQLITE_DISABLE_REPEAT_UNROLL=1`.

use crate::syntax::path_pattern::PathPattern;

/// Max number of arms the union may grow to. Each arm is its own LTJ
/// run, so the cost is roughly linear in the arm count; 4 keeps the
/// `{1,4}` and `{2,5}` shapes practical without letting a misuse of
/// `{1,20}` quietly explode the AST.
const MAX_UNROLL: usize = 4;

pub fn optimize(p: PathPattern) -> PathPattern {
    if std::env::var("GQLITE_DISABLE_REPEAT_UNROLL").is_ok() {
        return p;
    }
    let unrolled = rewrite(p);
    // Lift any Union produced by unrolling out of any surrounding Concat
    // so each arm becomes a flat chain. Without this, `Concat(prefix,
    // Union(a, b))` keeps Union inside the LTJ decomposer's view, which
    // is one of the carve-outs that forces the runtime back onto pairwise
    // hash-join — and the hash-join evaluates each Concat side globally,
    // erasing the win we were trying to capture.
    distribute_union_over_concat(unrolled)
}

fn rewrite(p: PathPattern) -> PathPattern {
    match p {
        PathPattern::Concat(p1, p2) => {
            PathPattern::Concat(Box::new(rewrite(*p1)), Box::new(rewrite(*p2)))
        }
        PathPattern::Union(p1, p2) => {
            PathPattern::Union(Box::new(rewrite(*p1)), Box::new(rewrite(*p2)))
        }
        PathPattern::Join(p1, p2) => {
            PathPattern::Join(Box::new(rewrite(*p1)), Box::new(rewrite(*p2)))
        }
        PathPattern::Filter(inner, expr) => PathPattern::Filter(Box::new(rewrite(*inner)), expr),
        PathPattern::Questioned(inner) => PathPattern::Questioned(Box::new(rewrite(*inner))),
        PathPattern::Repeat {
            pattern,
            lb,
            ub: Some(ub),
        } if can_unroll(&pattern, lb, ub) => {
            // Recurse into the inner before unrolling so any nested
            // Repeats also collapse before we clone the AST k times.
            let inner = rewrite(*pattern);
            unroll_to_union(inner, lb, ub)
        }
        PathPattern::Repeat { pattern, lb, ub } => PathPattern::Repeat {
            pattern: Box::new(rewrite(*pattern)),
            lb,
            ub,
        },
        leaf => leaf,
    }
}

/// Distribute Concat over Union: `Concat(Union(a, b), q) ≡
/// Union(Concat(a, q), Concat(b, q))` (and symmetrically on the right).
/// Bounded by `MAX_UNROLL` already on the Repeat-introduced Unions, so
/// the AST blow-up from this pass is at most a constant factor of the
/// arms produced earlier. We only distribute over Concat — Filter,
/// Repeat, and Join are left intact (a top-level Filter wraps the
/// Union as expected; Filter-Union distribution would duplicate WHERE
/// evaluation across arms with no payoff).
fn distribute_union_over_concat(p: PathPattern) -> PathPattern {
    match p {
        PathPattern::Concat(p1, p2) => {
            let p1 = distribute_union_over_concat(*p1);
            let p2 = distribute_union_over_concat(*p2);
            match (p1, p2) {
                (PathPattern::Union(a, b), q) => {
                    let q1 = PathPattern::Concat(Box::new(*a), Box::new(q.clone()));
                    let q2 = PathPattern::Concat(Box::new(*b), Box::new(q));
                    PathPattern::Union(
                        Box::new(distribute_union_over_concat(q1)),
                        Box::new(distribute_union_over_concat(q2)),
                    )
                }
                (p, PathPattern::Union(a, b)) => {
                    let q1 = PathPattern::Concat(Box::new(p.clone()), Box::new(*a));
                    let q2 = PathPattern::Concat(Box::new(p), Box::new(*b));
                    PathPattern::Union(
                        Box::new(distribute_union_over_concat(q1)),
                        Box::new(distribute_union_over_concat(q2)),
                    )
                }
                (p1, p2) => PathPattern::Concat(Box::new(p1), Box::new(p2)),
            }
        }
        PathPattern::Union(p1, p2) => PathPattern::Union(
            Box::new(distribute_union_over_concat(*p1)),
            Box::new(distribute_union_over_concat(*p2)),
        ),
        PathPattern::Join(p1, p2) => PathPattern::Join(
            Box::new(distribute_union_over_concat(*p1)),
            Box::new(distribute_union_over_concat(*p2)),
        ),
        PathPattern::Filter(inner, expr) => {
            PathPattern::Filter(Box::new(distribute_union_over_concat(*inner)), expr)
        }
        PathPattern::Questioned(inner) => {
            PathPattern::Questioned(Box::new(distribute_union_over_concat(*inner)))
        }
        PathPattern::Repeat { pattern, lb, ub } => PathPattern::Repeat {
            pattern: Box::new(distribute_union_over_concat(*pattern)),
            lb,
            ub,
        },
        leaf => leaf,
    }
}

fn can_unroll(p: &PathPattern, lb: usize, ub: usize) -> bool {
    if lb == 0 || lb > ub {
        return false;
    }
    if ub - lb + 1 > MAX_UNROLL {
        return false;
    }
    if !p.freevars().is_empty() {
        return false;
    }
    // Single-edge inner only. The unroll inserts an anonymous `Node(None)`
    // between consecutive copies so the LTJ decomposer sees the strict
    // `Node Edge Node Edge ... Node` alternation that
    // `decompose_flat_chain` requires. For more complex inners (e.g.
    // `Node-Edge-Node` repeats) the join boundary insertion would be
    // ambiguous and could produce illegal `Node Node` adjacencies; bail
    // out and let the runtime keep its hash-join repetition.
    matches!(
        p,
        PathPattern::EdgeRight(_)
            | PathPattern::EdgeLeft(_)
            | PathPattern::EdgeUndirected(_)
            | PathPattern::EdgeAnyDirection(_)
    )
}

/// Build `(P)^lb | (P)^{lb+1} | ... | (P)^ub` as a left-leaning Union
/// of concats. For `lb == ub` this produces the single concat (no
/// Union wrapper) since the loop only runs once.
fn unroll_to_union(inner: PathPattern, lb: usize, ub: usize) -> PathPattern {
    let mut acc: Option<PathPattern> = None;
    for k in lb..=ub {
        let copy = repeat_concat(&inner, k);
        acc = Some(match acc {
            None => copy,
            Some(prev) => PathPattern::Union(Box::new(prev), Box::new(copy)),
        });
    }
    acc.expect("can_unroll guarantees lb <= ub so the range is non-empty")
}

/// `repeat_concat(P, k)` builds `Edge Node(None) Edge Node(None) ... Edge`
/// for an edge-only inner `P`. The anonymous nodes between copies are
/// what the LTJ flat-chain decomposer (`decompose_flat_chain`) needs to
/// see strict `Node Edge Node` alternation when this expression is
/// spliced inside a surrounding outer chain like `(a) ... (b)` —
/// without them, two consecutive edges break decomposition and the
/// runtime falls back to hash-join, which is what unrolling was meant
/// to escape.
fn repeat_concat(p: &PathPattern, k: usize) -> PathPattern {
    debug_assert!(k >= 1);
    if k == 1 {
        return p.clone();
    }
    let mut acc = p.clone();
    for _ in 1..k {
        acc = PathPattern::Concat(
            Box::new(p.clone()),
            Box::new(PathPattern::Concat(
                Box::new(PathPattern::Node(None)),
                Box::new(acc),
            )),
        );
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::descriptor::Descriptor;
    use crate::syntax::path_pattern::PathPattern;
    use crate::typing::descriptor_type::DescriptorType;
    use crate::typing::label_type::LabelType;
    use crate::typing::property_type::PropertyType;

    fn knows_edge() -> PathPattern {
        PathPattern::EdgeUndirected(Some(Descriptor {
            var: None,
            dtype: DescriptorType {
                label: LabelType::Label("knows".into()),
                props: PropertyType::Open(Default::default()),
            },
            value_filters: vec![],
            value_preds: vec![],
        }))
    }

    #[test]
    fn unrolls_one_to_two() {
        let p = PathPattern::Repeat {
            pattern: Box::new(knows_edge()),
            lb: 1,
            ub: Some(2),
        };
        let out = optimize(p);
        // Expected: Union(Edge, Concat(Edge, Edge))
        match out {
            PathPattern::Union(a, b) => {
                assert!(matches!(*a, PathPattern::EdgeUndirected(_)));
                assert!(matches!(*b, PathPattern::Concat(_, _)));
            }
            other => panic!("expected Union, got {other:#?}"),
        }
    }

    #[test]
    fn keeps_repeat_when_ub_unbounded() {
        let p = PathPattern::Repeat {
            pattern: Box::new(knows_edge()),
            lb: 1,
            ub: None,
        };
        assert!(matches!(optimize(p), PathPattern::Repeat { ub: None, .. }));
    }

    #[test]
    fn keeps_repeat_when_range_too_wide() {
        let p = PathPattern::Repeat {
            pattern: Box::new(knows_edge()),
            lb: 1,
            ub: Some(20),
        };
        assert!(matches!(
            optimize(p),
            PathPattern::Repeat { ub: Some(20), .. }
        ));
    }

    #[test]
    fn keeps_repeat_when_inner_has_named_var() {
        let p = PathPattern::Repeat {
            pattern: Box::new(PathPattern::EdgeUndirected(Some(Descriptor {
                var: Some("e".into()),
                dtype: DescriptorType {
                    label: LabelType::Label("knows".into()),
                    props: PropertyType::Open(Default::default()),
                },
                value_filters: vec![],
                value_preds: vec![],
            }))),
            lb: 1,
            ub: Some(2),
        };
        assert!(matches!(optimize(p), PathPattern::Repeat { .. }));
    }

    #[test]
    fn keeps_repeat_when_lb_zero() {
        let p = PathPattern::Repeat {
            pattern: Box::new(knows_edge()),
            lb: 0,
            ub: Some(2),
        };
        assert!(matches!(optimize(p), PathPattern::Repeat { lb: 0, .. }));
    }

    #[test]
    fn lb_eq_ub_does_not_wrap_in_union() {
        let p = PathPattern::Repeat {
            pattern: Box::new(knows_edge()),
            lb: 2,
            ub: Some(2),
        };
        match optimize(p) {
            PathPattern::Concat(_, _) => {}
            other => panic!("expected Concat, got {other:#?}"),
        }
    }
}
