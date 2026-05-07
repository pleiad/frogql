//! Predicate pushdown: extract WHERE conjuncts and merge them into pattern
//! descriptors so they can prune candidates inside the join loop instead of
//! filtering join output.
//!
//! Two flavours of conjunct are pushed:
//!   - Type tests (`var.attr is T`) merge into the descriptor's property type
//!     and feed the typechecker plus the LTJ NodeProperty filter.
//!   - Value comparisons (`var.attr <op> literal`, op ∈ {=, ≠, <, ≤, >, ≥})
//!     merge into the descriptor's `value_preds` and feed the LTJ
//!     NodeAttrCmp filter, so a constant like `p.id = 30786325579101` reduces
//!     `p` to a single candidate before the join expands.
//!
//! Transforms:
//!   `(x)-[y]->(z) WHERE x.a bool and y.b str and x.k = 7`
//! into:
//!   `(x:{a bool, value_preds=[k = 7]})-[y:{b str}]->(z)`
//!
//! Only works for top-level AND conjuncts. OR expressions cannot be pushed down
//! because neither side is guaranteed to hold. Value pushdown applies to both
//! node and edge descriptors: nodes feed `FilterKind::NodeAttrCmp` (with
//! optional index folding); edges feed `FilterKind::EdgeAttrCmp` whose runtime
//! resolves the edge_id from the already-bound src/tgt + edge label.

use std::collections::HashMap;

use crate::model::value::Value;
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::{BinOp, Expr};
use crate::syntax::path_pattern::PathPattern;
use crate::typing::simple_type::SimpleType;

/// A pushable type constraint: `var.attr is type`.
#[derive(Debug, Clone)]
struct TypeConstraint {
    var: String,
    attr: String,
    ty: SimpleType,
}

/// A pushable value predicate: `var.attr <op> literal`.
#[derive(Debug, Clone)]
struct ValuePred {
    var: String,
    attr: String,
    op: BinOp,
    value: Value,
}

#[derive(Default)]
struct Constraints {
    types: HashMap<String, Vec<(String, SimpleType)>>,
    values: HashMap<String, Vec<(String, BinOp, Value)>>,
}

impl Constraints {
    fn is_empty(&self) -> bool {
        self.types.is_empty() && self.values.is_empty()
    }
}

/// Optimize a path pattern by pushing WHERE conjuncts into descriptors.
pub fn optimize(pattern: PathPattern) -> PathPattern {
    rewrite(pattern)
}

fn rewrite(p: PathPattern) -> PathPattern {
    match p {
        PathPattern::Filter(inner, expr) => {
            let conjuncts = flatten_and(&expr);
            let var_kinds = collect_var_kinds(&inner);

            let mut constraints = Constraints::default();
            let mut remaining: Vec<Expr> = Vec::new();

            for c in conjuncts {
                if let Some(tc) = extract_type_constraint(&c) {
                    constraints
                        .types
                        .entry(tc.var)
                        .or_default()
                        .push((tc.attr, tc.ty));
                } else if let Some(vp) = extract_value_pred(&c) {
                    // Push value predicates on both nodes AND edges. Nodes
                    // become `FilterKind::NodeAttrCmp`; edges become
                    // `FilterKind::EdgeAttrCmp` (resolved in pattern_extract).
                    // References to variables not bound by this pattern
                    // (sub-pattern correlation) stay in the residual WHERE.
                    if matches!(
                        var_kinds.get(&vp.var),
                        Some(VarKind::Node) | Some(VarKind::Edge)
                    ) {
                        constraints
                            .values
                            .entry(vp.var)
                            .or_default()
                            .push((vp.attr, vp.op, vp.value));
                    } else {
                        remaining.push(c);
                    }
                } else {
                    remaining.push(c);
                }
            }

            if constraints.is_empty() {
                return PathPattern::Filter(Box::new(rewrite(*inner)), expr);
            }

            let rewritten = merge_constraints(*inner, &constraints);

            if remaining.is_empty() {
                rewrite(rewritten)
            } else {
                PathPattern::Filter(Box::new(rewrite(rewritten)), rebuild_and(remaining))
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

/// Try to extract a value predicate from `var.attr <op> literal` (or the
/// symmetric `literal <op> var.attr`). Supports `=`, `!=`, `<`, `<=`, `>`,
/// `>=` against `Const` literals.
fn extract_value_pred(expr: &Expr) -> Option<ValuePred> {
    let (op, left, right) = match expr {
        Expr::Binop { op, left, right }
            if matches!(
                op,
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
            ) =>
        {
            (*op, left.as_ref(), right.as_ref())
        }
        _ => return None,
    };

    if let (Expr::AttrLookup { var, attr }, Expr::Const(v)) = (left, right) {
        return Some(ValuePred {
            var: var.clone(),
            attr: attr.clone(),
            op,
            value: v.clone(),
        });
    }
    if let (Expr::Const(v), Expr::AttrLookup { var, attr }) = (left, right) {
        return Some(ValuePred {
            var: var.clone(),
            attr: attr.clone(),
            op: flip_op(op),
            value: v.clone(),
        });
    }
    None
}

/// Flip a comparison so that the variable lives on the left of the operator.
fn flip_op(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Le => BinOp::Ge,
        BinOp::Gt => BinOp::Lt,
        BinOp::Ge => BinOp::Le,
        // Eq / Ne are symmetric.
        other => other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VarKind {
    Node,
    Edge,
}

/// Walk the pattern and record whether each named variable binds a node or an
/// edge. Used to confine value-predicate pushdown to nodes.
fn collect_var_kinds(p: &PathPattern) -> HashMap<String, VarKind> {
    let mut acc = HashMap::new();
    walk_kinds(p, &mut acc);
    acc
}

fn walk_kinds(p: &PathPattern, acc: &mut HashMap<String, VarKind>) {
    match p {
        PathPattern::Node(Some(d)) => {
            if let Some(v) = &d.var {
                acc.insert(v.clone(), VarKind::Node);
            }
        }
        PathPattern::EdgeRight(Some(d))
        | PathPattern::EdgeLeft(Some(d))
        | PathPattern::EdgeUndirected(Some(d))
        | PathPattern::EdgeAnyDirection(Some(d)) => {
            if let Some(v) = &d.var {
                acc.insert(v.clone(), VarKind::Edge);
            }
        }
        PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
            walk_kinds(a, acc);
            walk_kinds(b, acc);
        }
        PathPattern::Filter(inner, _) | PathPattern::Questioned(inner) => walk_kinds(inner, acc),
        PathPattern::Repeat { pattern, .. } => walk_kinds(pattern, acc),
        _ => {}
    }
}

/// Walk the pattern tree and merge type and value constraints into descriptors.
fn merge_constraints(p: PathPattern, c: &Constraints) -> PathPattern {
    match p {
        PathPattern::Node(desc) => PathPattern::Node(merge_into_node_desc(desc, c)),
        PathPattern::EdgeRight(desc) => PathPattern::EdgeRight(merge_into_edge_desc(desc, c)),
        PathPattern::EdgeLeft(desc) => PathPattern::EdgeLeft(merge_into_edge_desc(desc, c)),
        PathPattern::EdgeUndirected(desc) => {
            PathPattern::EdgeUndirected(merge_into_edge_desc(desc, c))
        }
        PathPattern::EdgeAnyDirection(desc) => {
            PathPattern::EdgeAnyDirection(merge_into_edge_desc(desc, c))
        }
        PathPattern::Concat(p1, p2) => PathPattern::Concat(
            Box::new(merge_constraints(*p1, c)),
            Box::new(merge_constraints(*p2, c)),
        ),
        PathPattern::Union(p1, p2) => PathPattern::Union(
            Box::new(merge_constraints(*p1, c)),
            Box::new(merge_constraints(*p2, c)),
        ),
        PathPattern::Filter(inner, expr) => {
            PathPattern::Filter(Box::new(merge_constraints(*inner, c)), expr)
        }
        PathPattern::Repeat { pattern, lb, ub } => PathPattern::Repeat {
            pattern: Box::new(merge_constraints(*pattern, c)),
            lb,
            ub,
        },
        PathPattern::Questioned(inner) => {
            PathPattern::Questioned(Box::new(merge_constraints(*inner, c)))
        }
        PathPattern::Join(p1, p2) => PathPattern::Join(
            Box::new(merge_constraints(*p1, c)),
            Box::new(merge_constraints(*p2, c)),
        ),
    }
}

/// Merge type and value constraints into a node descriptor. Anonymous
/// nodes have no `var` to match WHERE conjuncts against; we still must
/// return the descriptor unchanged so its own pattern-derived label/type
/// constraints reach the LTJ extractor.
fn merge_into_node_desc(desc: Option<Descriptor>, c: &Constraints) -> Option<Descriptor> {
    let mut d = desc?;

    if let Some(var_name) = d.var.clone() {
        if let Some(attrs) = c.types.get(&var_name) {
            for (attr, ty) in attrs {
                d.dtype.props.extend(attr.clone(), ty.clone());
            }
        }
        if let Some(preds) = c.values.get(&var_name) {
            d.value_preds.extend(preds.iter().cloned());
        }
    }
    Some(d)
}

/// Merge type and value constraints into an edge descriptor. Value
/// predicates land in `value_preds` and become `FilterKind::EdgeAttrCmp`
/// at pattern-extract time (the LTJ runner resolves the edge_id from the
/// already-bound src/tgt + edge label and then reads `edge_props`).
/// Anonymous edges (no `var`) carry no merged constraints, but the
/// descriptor must survive so its label reaches the LTJ extractor.
fn merge_into_edge_desc(desc: Option<Descriptor>, c: &Constraints) -> Option<Descriptor> {
    let mut d = desc?;

    if let Some(var_name) = d.var.clone() {
        if let Some(attrs) = c.types.get(&var_name) {
            for (attr, ty) in attrs {
                d.dtype.props.extend(attr.clone(), ty.clone());
            }
        }
        if let Some(preds) = c.values.get(&var_name) {
            d.value_preds.extend(preds.iter().cloned());
        }
    }
    Some(d)
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
    fn test_pushdown_partial_drops_filter_when_all_pushable() {
        // ((x)-[y]->(z) WHERE x.a bool and y.b > 10)
        // → (x:{a:bool})-[y:{value_preds=[b > 10]}]->(z); both conjuncts
        // are pushable now that edges accept value predicates.
        let p = parse("((x)-[y]->(z) WHERE x.a bool and y.b > 10)").unwrap();
        let optimized = optimize(p);

        assert!(
            !matches!(&optimized, PathPattern::Filter(_, _)),
            "expected no remaining filter, got: {optimized}"
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
            "(x WHERE x.isBlocked = true)",
            "(x WHERE x.isBlocked = false)",
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

    fn first_node_value_preds(p: &PathPattern, var: &str) -> Vec<(String, BinOp, Value)> {
        match p {
            PathPattern::Node(Some(d)) if d.var.as_deref() == Some(var) => d.value_preds.clone(),
            PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
                let mut left = first_node_value_preds(a, var);
                if !left.is_empty() {
                    return left;
                }
                left.extend(first_node_value_preds(b, var));
                left
            }
            PathPattern::Filter(inner, _) | PathPattern::Questioned(inner) => {
                first_node_value_preds(inner, var)
            }
            PathPattern::Repeat { pattern, .. } => first_node_value_preds(pattern, var),
            _ => vec![],
        }
    }

    #[test]
    fn test_pushdown_value_eq_on_node() {
        // Equality literal pushes into the node descriptor; Filter is dropped.
        let p = parse("(x WHERE x.id = 7)").unwrap();
        let optimized = optimize(p);
        assert!(
            !matches!(&optimized, PathPattern::Filter(_, _)),
            "expected no filter: {optimized}"
        );
        let preds = first_node_value_preds(&optimized, "x");
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].0, "id");
        assert_eq!(preds[0].1, BinOp::Eq);
        assert_eq!(preds[0].2, Value::Int(7));
    }

    #[test]
    fn test_pushdown_value_cmp_on_node() {
        // Range comparison also pushes.
        let p = parse("(c WHERE c.creationDate <= 1323302400000)").unwrap();
        let optimized = optimize(p);
        assert!(!matches!(&optimized, PathPattern::Filter(_, _)));
        let preds = first_node_value_preds(&optimized, "c");
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].1, BinOp::Le);
    }

    #[test]
    fn test_pushdown_value_pred_on_edge() {
        // Edge value predicates push into the edge descriptor's value_preds;
        // the LTJ runner resolves edge_id from adjacency to evaluate them.
        let p = parse("((x)-[y]->(z) WHERE y.amount > 10)").unwrap();
        let optimized = optimize(p);
        assert!(
            !matches!(&optimized, PathPattern::Filter(_, _)),
            "expected no remaining filter, got: {optimized}"
        );
        let preds = first_edge_value_preds(&optimized, "y");
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].0, "amount");
        assert_eq!(preds[0].1, BinOp::Gt);
        assert_eq!(preds[0].2, Value::Int(10));
    }

    fn first_edge_value_preds(p: &PathPattern, var: &str) -> Vec<(String, BinOp, Value)> {
        let edge_desc = match p {
            PathPattern::EdgeRight(Some(d))
            | PathPattern::EdgeLeft(Some(d))
            | PathPattern::EdgeUndirected(Some(d))
            | PathPattern::EdgeAnyDirection(Some(d))
                if d.var.as_deref() == Some(var) =>
            {
                Some(d.clone())
            }
            _ => None,
        };
        if let Some(d) = edge_desc {
            return d.value_preds;
        }
        match p {
            PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
                let mut left = first_edge_value_preds(a, var);
                if !left.is_empty() {
                    return left;
                }
                left.extend(first_edge_value_preds(b, var));
                left
            }
            PathPattern::Filter(inner, _) | PathPattern::Questioned(inner) => {
                first_edge_value_preds(inner, var)
            }
            PathPattern::Repeat { pattern, .. } => first_edge_value_preds(pattern, var),
            _ => vec![],
        }
    }

    #[test]
    fn test_pushdown_flipped_literal_lhs() {
        // `7 = x.id` has the same effect as `x.id = 7` after flipping.
        let p = parse("(x WHERE 7 = x.id)").unwrap();
        let optimized = optimize(p);
        assert!(!matches!(&optimized, PathPattern::Filter(_, _)));
        let preds = first_node_value_preds(&optimized, "x");
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].1, BinOp::Eq);
    }
}
