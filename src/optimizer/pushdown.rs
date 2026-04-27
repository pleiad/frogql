//! Predicate pushdown: extract type constraints from WHERE clauses and merge
//! them into pattern descriptors.
//!
//! Transforms:
//!   `(x)-[y]->(z) WHERE x.a bool and y.b str`
//! into:
//!   `(x:{a bool})-[y:{b str}]->(z)`
//!
//! Only works for top-level AND conjuncts. OR expressions cannot be pushed down
//! because neither side is guaranteed to hold.

use std::collections::HashMap;

use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr};
use crate::syntax::path_pattern::PathPattern;
use crate::typing::simple_type::SimpleType;

/// A pushable constraint: variable.attr is type.
#[derive(Debug, Clone)]
struct TypeConstraint {
    var: String,
    attr: String,
    ty: SimpleType,
}

/// Optimize a path pattern by pushing WHERE type constraints into descriptors.
pub fn optimize(pattern: PathPattern) -> PathPattern {
    rewrite(pattern)
}

fn rewrite(p: PathPattern) -> PathPattern {
    match p {
        PathPattern::Filter(inner, expr) => {
            // Flatten the AND-chain and separate pushable vs non-pushable
            let conjuncts = flatten_and(&expr);
            let (pushable, remaining): (Vec<_>, Vec<_>) = conjuncts
                .into_iter()
                .partition(|c| extract_type_constraint(c).is_some());

            if pushable.is_empty() {
                // Nothing to push — recurse into inner and keep filter
                return PathPattern::Filter(Box::new(rewrite(*inner)), expr);
            }

            // Collect constraints per variable
            let mut constraints: HashMap<String, Vec<(String, SimpleType)>> = HashMap::new();
            for c in &pushable {
                if let Some(tc) = extract_type_constraint(c) {
                    constraints
                        .entry(tc.var)
                        .or_default()
                        .push((tc.attr, tc.ty));
                }
            }

            // Rewrite the inner pattern with merged descriptors
            let rewritten = merge_constraints(*inner, &constraints);

            // Rebuild remaining filter (if any non-pushable conjuncts remain)
            if remaining.is_empty() {
                rewrite(rewritten)
            } else {
                let remaining_expr = rebuild_and(remaining);
                PathPattern::Filter(Box::new(rewrite(rewritten)), remaining_expr)
            }
        }
        // Recurse into structural patterns
        PathPattern::Concat(p1, p2) => {
            PathPattern::Concat(Box::new(rewrite(*p1)), Box::new(rewrite(*p2)))
        }
        PathPattern::Union(p1, p2) => {
            PathPattern::Union(Box::new(rewrite(*p1)), Box::new(rewrite(*p2)))
        }
        PathPattern::Repeat { pattern, lb, ub } => PathPattern::Repeat {
            pattern: Box::new(rewrite(*pattern)),
            lb,
            ub,
        },
        PathPattern::Questioned(p) => PathPattern::Questioned(Box::new(rewrite(*p))),
        PathPattern::Join(p1, p2) => {
            PathPattern::Join(Box::new(rewrite(*p1)), Box::new(rewrite(*p2)))
        }
        // Leaf patterns — no rewriting needed
        other => other,
    }
}

/// Flatten an AND-chain into a list of conjuncts.
/// `a and b and c` → `[a, b, c]`
/// Non-AND expressions return as a single-element list.
fn flatten_and(expr: &Expr) -> Vec<Expr> {
    match expr {
        Expr::Binop {
            op: BinOp::And,
            left,
            right,
        } => {
            let mut result = flatten_and(left);
            result.extend(flatten_and(right));
            result
        }
        other => vec![other.clone()],
    }
}

/// Rebuild an AND-chain from a list of conjuncts.
fn rebuild_and(mut exprs: Vec<Expr>) -> Expr {
    assert!(!exprs.is_empty());
    if exprs.len() == 1 {
        return exprs.pop().unwrap();
    }
    let first = exprs.remove(0);
    let rest = rebuild_and(exprs);
    Expr::Binop {
        op: BinOp::And,
        left: Box::new(first),
        right: Box::new(rest),
    }
}

/// Try to extract a type constraint from `var.attr is type`.
fn extract_type_constraint(expr: &Expr) -> Option<TypeConstraint> {
    match expr {
        Expr::Binop {
            op: BinOp::Is,
            left,
            right,
        } => {
            let (var, attr) = match left.as_ref() {
                Expr::AttrLookup { var, attr } => (var.clone(), attr.clone()),
                _ => return None,
            };
            let ty = match right.as_ref() {
                Expr::Type(t) => t.clone(),
                _ => return None,
            };
            Some(TypeConstraint { var, attr, ty })
        }
        _ => None,
    }
}

/// Walk the pattern tree and merge type constraints into descriptors.
fn merge_constraints(
    p: PathPattern,
    constraints: &HashMap<String, Vec<(String, SimpleType)>>,
) -> PathPattern {
    match p {
        PathPattern::Node(desc) => PathPattern::Node(merge_into_descriptor(desc, constraints)),
        PathPattern::EdgeRight(desc) => {
            PathPattern::EdgeRight(merge_into_descriptor(desc, constraints))
        }
        PathPattern::EdgeLeft(desc) => {
            PathPattern::EdgeLeft(merge_into_descriptor(desc, constraints))
        }
        PathPattern::EdgeUndirected(desc) => {
            PathPattern::EdgeUndirected(merge_into_descriptor(desc, constraints))
        }
        PathPattern::EdgeAnyDirection(desc) => {
            PathPattern::EdgeAnyDirection(merge_into_descriptor(desc, constraints))
        }
        PathPattern::Concat(p1, p2) => PathPattern::Concat(
            Box::new(merge_constraints(*p1, constraints)),
            Box::new(merge_constraints(*p2, constraints)),
        ),
        PathPattern::Union(p1, p2) => PathPattern::Union(
            Box::new(merge_constraints(*p1, constraints)),
            Box::new(merge_constraints(*p2, constraints)),
        ),
        PathPattern::Filter(inner, expr) => {
            PathPattern::Filter(Box::new(merge_constraints(*inner, constraints)), expr)
        }
        PathPattern::Repeat { pattern, lb, ub } => PathPattern::Repeat {
            pattern: Box::new(merge_constraints(*pattern, constraints)),
            lb,
            ub,
        },
        PathPattern::Questioned(inner) => {
            PathPattern::Questioned(Box::new(merge_constraints(*inner, constraints)))
        }
        PathPattern::Join(p1, p2) => PathPattern::Join(
            Box::new(merge_constraints(*p1, constraints)),
            Box::new(merge_constraints(*p2, constraints)),
        ),
    }
}

/// Merge constraints into a descriptor if the variable matches.
fn merge_into_descriptor(
    desc: Option<Descriptor>,
    constraints: &HashMap<String, Vec<(String, SimpleType)>>,
) -> Option<Descriptor> {
    let d = desc?;
    let var_name = d.var.as_ref()?;

    if let Some(attrs) = constraints.get(var_name) {
        let mut new_dtype = d.dtype.clone();
        for (attr, ty) in attrs {
            new_dtype.props.extend(attr.clone(), ty.clone());
        }
        Some(Descriptor::new(d.var, new_dtype))
    } else {
        Some(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn test_pushdown_simple_is() {
        // ((x)-[y]->(z) WHERE x.a bool and y.b str)
        // → (x:{a:bool})-[y:{b:str}]->(z)
        let p = parse("((x)-[y]->(z) WHERE x.a bool and y.b str)").unwrap();
        let optimized = optimize(p);

        // The filter should be completely removed (all conjuncts pushed)
        assert!(
            !matches!(&optimized, PathPattern::Filter(_, _)),
            "expected no remaining filter, got: {optimized}"
        );

        // Check that the optimized pattern string contains the property constraints
        let s = optimized.to_string();
        assert!(s.contains("bool"), "expected bool in descriptor: {s}");
        assert!(s.contains("str"), "expected str in descriptor: {s}");
    }

    #[test]
    fn test_pushdown_partial() {
        // ((x)-[y]->(z) WHERE x.a bool and y.b > 10)
        // → (x:{a:bool})-[y]->(z) WHERE y.b > 10
        let p = parse("((x)-[y]->(z) WHERE x.a bool and y.b > 10)").unwrap();
        let optimized = optimize(p);

        // Should still have a filter for the non-pushable y.b > 10
        assert!(
            matches!(&optimized, PathPattern::Filter(_, _)),
            "expected remaining filter, got: {optimized}"
        );
    }

    #[test]
    fn test_no_pushdown_or() {
        // (x WHERE x.a bool or x.b str)
        // → no pushdown (OR can't be pushed)
        let p = parse("(x WHERE x.a bool or x.b str)").unwrap();
        let optimized = optimize(p.clone());

        // Should keep the filter unchanged
        assert!(matches!(&optimized, PathPattern::Filter(_, _)));
    }

    #[test]
    fn test_pushdown_same_var_multiple_attrs() {
        // ((x) WHERE x.a bool and x.b int)
        // → (x:{a:bool, b:int})
        let p = parse("((x) WHERE x.a bool and x.b int)").unwrap();
        let optimized = optimize(p);

        assert!(
            !matches!(&optimized, PathPattern::Filter(_, _)),
            "expected no filter: {optimized}"
        );
    }

    #[test]
    fn test_pushdown_preserves_semantics() {
        // Verify that pushed-down queries produce the same results as originals
        use crate::model::graph::Graph;
        use crate::runtime::engine::Runtime;
        use std::path::Path;

        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
        let g = Graph::from_file(&path).unwrap();
        let rt = Runtime::new(&g);

        let queries = vec![
            "(x WHERE x.isBlocked bool)",
            "(x WHERE x.isDummy bool)",
            "((x)-[y]->(z) WHERE x.isBlocked bool)",
            "(x)-[y WHERE y.amount int]->(z)",
        ];

        for q in queries {
            let original = parse(q).unwrap();
            let optimized = optimize(original.clone());

            let r_orig = rt.run(&original).rows.len();
            let r_opt = rt.run(&optimized).rows.len();

            assert_eq!(
                r_orig, r_opt,
                "mismatch for '{q}': original={r_orig}, optimized={r_opt}"
            );
        }
    }
}
