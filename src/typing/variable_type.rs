use std::fmt;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use super::descriptor_type::DescriptorType;
use super::simple_type::SimpleType;

/// Types for pattern variables (nodes, edges, unions, lists, bottom).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VariableType {
    Node(DescriptorType),
    EdgeDirectional {
        desc: DescriptorType,
        left: Box<VariableType>,  // NodeVariableType
        right: Box<VariableType>, // NodeVariableType
    },
    EdgeNonDirectional {
        desc: DescriptorType,
        left: Box<VariableType>,
        right: Box<VariableType>,
    },
    Union(Box<VariableType>, Box<VariableType>),
    Group(Box<VariableType>),
    /// The singleton type for the null value. Introduced when a variable
    /// appears in only one branch of a `TypeEnvironment` join, per the rule
    /// `Γ₁ ⊔ Γ₂` for keys present on a single side.
    Null,
    /// A path variable (`MATCH p = ...`). Terminal: it does not carry a
    /// descriptor, never refines against the schema, and is never shared
    /// across operands, so it stays inert under every lattice operation.
    /// Lifts to `SimpleType::Path`.
    Path,
    Zero,
}

impl VariableType {
    pub fn node_star() -> Self {
        VariableType::Node(DescriptorType::star())
    }

    pub fn edge_directional(desc: DescriptorType) -> Self {
        VariableType::EdgeDirectional {
            desc,
            left: Box::new(VariableType::node_star()),
            right: Box::new(VariableType::node_star()),
        }
    }

    pub fn edge_non_directional(desc: DescriptorType) -> Self {
        VariableType::EdgeNonDirectional {
            desc,
            left: Box::new(VariableType::node_star()),
            right: Box::new(VariableType::node_star()),
        }
    }

    /// Get the descriptor (for Node or Edge variants).
    pub fn descriptor(&self) -> Option<&DescriptorType> {
        match self {
            VariableType::Node(d) => Some(d),
            VariableType::EdgeDirectional { desc, .. } => Some(desc),
            VariableType::EdgeNonDirectional { desc, .. } => Some(desc),
            _ => None,
        }
    }

    /// Get attribute type from the property type.
    pub fn get_attribute(&self, attr: &str) -> SimpleType {
        match self {
            VariableType::Node(d) => d.props.get(attr),
            VariableType::EdgeDirectional { desc, .. } => desc.props.get(attr),
            VariableType::EdgeNonDirectional { desc, .. } => desc.props.get(attr),
            VariableType::Union(t1, t2) => {
                SimpleType::union(&t1.get_attribute(attr), &t2.get_attribute(attr))
            }
            VariableType::Group(t) => SimpleType::Group(Box::new(t.get_attribute(attr))),
            VariableType::Null => SimpleType::Zero,
            // A path has no attributes — `path.attr` is undefined.
            VariableType::Path => SimpleType::Zero,
            VariableType::Zero => SimpleType::Zero,
        }
    }

    // --- Meet ---

    fn meet_node(a: &DescriptorType, b: &DescriptorType) -> VariableType {
        VariableType::Node(DescriptorType::meet(a, b))
    }

    fn meet_edge_directional(
        d1: &DescriptorType,
        l1: &VariableType,
        r1: &VariableType,
        d2: &DescriptorType,
        l2: &VariableType,
        r2: &VariableType,
    ) -> VariableType {
        // l1, l2, r1, r2 should be Node variants
        let ld = match (l1, l2) {
            (VariableType::Node(a), VariableType::Node(b)) => DescriptorType::meet(a, b),
            _ => return VariableType::Zero,
        };
        let rd = match (r1, r2) {
            (VariableType::Node(a), VariableType::Node(b)) => DescriptorType::meet(a, b),
            _ => return VariableType::Zero,
        };
        VariableType::EdgeDirectional {
            desc: DescriptorType::meet(d1, d2),
            left: Box::new(VariableType::Node(ld)),
            right: Box::new(VariableType::Node(rd)),
        }
    }

    pub fn meet(a: &VariableType, b: &VariableType) -> VariableType {
        match (a, b) {
            (VariableType::Group(ta), VariableType::Group(tb)) => {
                VariableType::Group(Box::new(VariableType::meet(ta, tb)))
            }
            (VariableType::Node(da), VariableType::Node(db)) => Self::meet_node(da, db),
            (
                VariableType::EdgeDirectional {
                    desc: d1,
                    left: l1,
                    right: r1,
                },
                VariableType::EdgeDirectional {
                    desc: d2,
                    left: l2,
                    right: r2,
                },
            ) => Self::meet_edge_directional(d1, l1, r1, d2, l2, r2),
            (
                VariableType::EdgeNonDirectional {
                    desc: d1,
                    left: l1,
                    right: r1,
                },
                VariableType::EdgeNonDirectional {
                    desc: d2,
                    left: l2,
                    right: r2,
                },
            ) => {
                // Try both orientations, join the results
                let n1 = Self::meet_edge_directional(d1, l1, r1, d2, l2, r2);
                let n2 = Self::meet_edge_directional(d1, l1, r1, d2, r2, l2);
                match (&n1, &n2) {
                    (
                        VariableType::EdgeDirectional {
                            desc: da,
                            left: la,
                            right: ra,
                        },
                        VariableType::EdgeDirectional {
                            desc: db,
                            left: lb,
                            right: rb,
                        },
                    ) => VariableType::join(
                        VariableType::EdgeNonDirectional {
                            desc: da.clone(),
                            left: la.clone(),
                            right: ra.clone(),
                        },
                        VariableType::EdgeNonDirectional {
                            desc: db.clone(),
                            left: lb.clone(),
                            right: rb.clone(),
                        },
                    ),
                    _ => VariableType::Zero,
                }
            }
            (VariableType::Union(t1, t2), _) => {
                let r1 = VariableType::meet(t1, b);
                let r2 = VariableType::meet(t2, b);
                VariableType::join(r1, r2)
            }
            (_, VariableType::Union(_, _)) => VariableType::meet(b, a),
            (VariableType::Null, VariableType::Null) => VariableType::Null,
            (VariableType::Null, _) | (_, VariableType::Null) => VariableType::Zero,
            // Path only meets itself. A path variable is unique to its
            // operand and never shared across a comma-join/concat, so this
            // arm is reached only by a degenerate `p = ..., p = ...`.
            (VariableType::Path, VariableType::Path) => VariableType::Path,
            (VariableType::Zero, _) | (_, VariableType::Zero) => VariableType::Zero,
            _ => VariableType::Zero,
        }
    }

    // --- Join ---

    pub fn join(a: VariableType, b: VariableType) -> VariableType {
        if a == VariableType::Zero {
            return b;
        }
        if b == VariableType::Zero {
            return a;
        }
        if a == b {
            return a;
        }
        VariableType::Union(Box::new(a), Box::new(b))
    }

    pub fn join_from_list(types: Vec<VariableType>) -> VariableType {
        types.into_iter().fold(VariableType::Zero, Self::join)
    }

    // --- Subtyping ---

    /// Subtype check for the Node endpoints of an Edge variant.
    fn node_endpoint_subtype(a: &VariableType, b: &VariableType) -> bool {
        match (a, b) {
            (VariableType::Node(a), VariableType::Node(b)) => DescriptorType::is_subtype(a, b),
            _ => false,
        }
    }

    /// Subtype rule for a single orientation of an edge: descriptor
    /// subtype plus pointwise Node-endpoint subtype on left and right.
    /// Shared by both `EdgeDirectional` and the orientation-OR check
    /// of `EdgeNonDirectional`.
    fn edge_directional_subtype(
        d1: &DescriptorType,
        l1: &VariableType,
        r1: &VariableType,
        d2: &DescriptorType,
        l2: &VariableType,
        r2: &VariableType,
    ) -> bool {
        DescriptorType::is_subtype(d1, d2)
            && Self::node_endpoint_subtype(l1, l2)
            && Self::node_endpoint_subtype(r1, r2)
    }

    pub fn is_subtype(t1: &VariableType, t2: &VariableType) -> bool {
        match (t1, t2) {
            (VariableType::Zero, _) => true,
            (VariableType::Null, VariableType::Null) => true,
            (VariableType::Path, VariableType::Path) => true,
            (VariableType::Node(a), VariableType::Node(b)) => DescriptorType::is_subtype(a, b),
            (
                VariableType::EdgeDirectional {
                    desc: d1,
                    left: l1,
                    right: r1,
                },
                VariableType::EdgeDirectional {
                    desc: d2,
                    left: l2,
                    right: r2,
                },
            ) => Self::edge_directional_subtype(d1, l1, r1, d2, l2, r2),
            (
                VariableType::EdgeNonDirectional {
                    desc: d1,
                    left: l1,
                    right: r1,
                },
                VariableType::EdgeNonDirectional {
                    desc: d2,
                    left: l2,
                    right: r2,
                },
            ) => {
                Self::edge_directional_subtype(d1, l1, r1, d2, l2, r2)
                    || Self::edge_directional_subtype(d1, l1, r1, d2, r2, l2)
            }
            (VariableType::Group(a), VariableType::Group(b)) => VariableType::is_subtype(a, b),
            (VariableType::Union(a, b), _) => {
                VariableType::is_subtype(a, t2) || VariableType::is_subtype(b, t2)
            }
            (_, VariableType::Union(a, b)) => {
                VariableType::is_subtype(t1, a) || VariableType::is_subtype(t1, b)
            }
            _ => false,
        }
    }

    /// Refine a variable type against the schema and flatten the result into
    /// the concrete `Node` variants reachable. Mirrors fppc's
    /// `VariableType::refine_to_nodes` and is consumed by `PathType::meet`.
    pub fn refine_to_nodes(schema: &Schema, t: &VariableType) -> Vec<VariableType> {
        let mut out = Vec::new();
        let mut stack = vec![VariableType::refine(schema, t)];
        while let Some(curr) = stack.pop() {
            match curr {
                VariableType::Node(_) => out.push(curr),
                VariableType::Union(t1, t2) => {
                    stack.push(*t2);
                    stack.push(*t1);
                }
                _ => {}
            }
        }
        out
    }

    // --- Refine ---

    pub fn refine(schema: &Schema, node: &VariableType) -> VariableType {
        match node {
            VariableType::Node(_) => {
                let matches: Vec<VariableType> = schema
                    .nodes
                    .iter()
                    .filter(|n| VariableType::is_subtype(n, node))
                    .map(|n| VariableType::meet(n, node))
                    .collect();
                VariableType::join_from_list(matches)
            }
            VariableType::EdgeDirectional { .. } | VariableType::EdgeNonDirectional { .. } => {
                let matches: Vec<VariableType> = schema
                    .edges
                    .iter()
                    .filter(|e| VariableType::is_subtype(e, node))
                    .map(|e| VariableType::meet(e, node))
                    .collect();
                VariableType::join_from_list(matches)
            }
            VariableType::Union(t1, t2) => VariableType::join(
                VariableType::refine(schema, t1),
                VariableType::refine(schema, t2),
            ),
            VariableType::Group(t) => {
                VariableType::Group(Box::new(VariableType::refine(schema, t)))
            }
            VariableType::Null => VariableType::Null,
            VariableType::Path => VariableType::Path,
            VariableType::Zero => VariableType::Zero,
        }
    }

    // --- Is empty ---

    pub fn is_empty(&self) -> bool {
        match self {
            VariableType::Zero => true,
            VariableType::Node(d) => d.is_empty(),
            VariableType::EdgeDirectional { desc, left, right }
            | VariableType::EdgeNonDirectional { desc, left, right } => {
                desc.is_empty() || left.is_empty() || right.is_empty()
            }
            VariableType::Union(t1, t2) => t1.is_empty() && t2.is_empty(),
            VariableType::Group(t) => t.is_empty(),
            VariableType::Null => false,
            // A path binding is always inhabited; it never empties an env.
            VariableType::Path => false,
        }
    }
}

impl fmt::Display for VariableType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VariableType::Node(d) => write!(f, "⸨{d}⸩"),
            VariableType::EdgeDirectional { desc, left, right } => {
                write!(f, "{left}-[{desc}]->{right}")
            }
            VariableType::EdgeNonDirectional { desc, left, right } => {
                write!(f, "{left}-[{desc}]-{right}")
            }
            VariableType::Union(t1, t2) => write!(f, "{t1} + {t2}"),
            VariableType::Group(t) => write!(f, "group<{t}>"),
            VariableType::Null => write!(f, "Null"),
            VariableType::Path => write!(f, "path"),
            VariableType::Zero => write!(f, "⊥"),
        }
    }
}

/// Schema: a set of allowed node and edge types.
///
/// `nodes` and `edges` are wrapped in `Rc` so `Clone` is cheap — every
/// `Typechecker::new` call clones the active Schema, and without the
/// `Rc` wrapping that clone deep-copies the entire descriptor tree.
///
/// The fields stay `pub` for read-side compatibility (callers iterate
/// or index via `&Rc<Vec<T>>` → `&Vec<T>` deref). Schemas are immutable
/// after construction — DDL replaces the whole Schema rather than
/// mutating in place, so there is no `Rc::make_mut` call site today.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub nodes: Rc<Vec<VariableType>>,
    pub edges: Rc<Vec<VariableType>>,
}

impl Schema {
    /// Permissive schema that allows anything.
    pub fn star() -> Self {
        Schema {
            nodes: Rc::new(vec![VariableType::node_star()]),
            edges: Rc::new(vec![
                VariableType::edge_directional(DescriptorType::star()),
                VariableType::edge_non_directional(DescriptorType::star()),
            ]),
        }
    }

    /// Construct from explicit nodes/edges. Used by inference and tests.
    pub fn from_parts(nodes: Vec<VariableType>, edges: Vec<VariableType>) -> Self {
        Schema {
            nodes: Rc::new(nodes),
            edges: Rc::new(edges),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typing::label_type::LabelType;
    use crate::typing::property_type::PropertyType;

    fn node_with_label(name: &str) -> VariableType {
        VariableType::Node(DescriptorType::new(
            LabelType::Label(name.into()),
            PropertyType::open_empty(),
        ))
    }

    // is_empty — note: Edge variants use OR over (desc, left, right),
    // different from Union which uses AND.
    #[test]
    fn test_zero_is_empty() {
        assert!(VariableType::Zero.is_empty());
    }
    #[test]
    fn test_node_empty_iff_descriptor_empty() {
        let empty_d = DescriptorType::new(LabelType::Star, PropertyType::Zero);
        assert!(VariableType::Node(empty_d).is_empty());
        assert!(!VariableType::node_star().is_empty());
    }
    #[test]
    fn test_edge_directional_empty_iff_any_component_empty() {
        let star_node = || Box::new(VariableType::node_star());
        let empty_node = || {
            Box::new(VariableType::Node(DescriptorType::new(
                LabelType::Star,
                PropertyType::Zero,
            )))
        };
        // All full: not empty.
        assert!(!VariableType::EdgeDirectional {
            desc: DescriptorType::star(),
            left: star_node(),
            right: star_node(),
        }
        .is_empty());
        // Empty left → empty edge.
        assert!(VariableType::EdgeDirectional {
            desc: DescriptorType::star(),
            left: empty_node(),
            right: star_node(),
        }
        .is_empty());
        // Empty right → empty edge.
        assert!(VariableType::EdgeDirectional {
            desc: DescriptorType::star(),
            left: star_node(),
            right: empty_node(),
        }
        .is_empty());
    }
    #[test]
    fn test_union_empty_iff_both_empty() {
        // Sanity: complement to Edge's OR semantics.
        let zero = || Box::new(VariableType::Zero);
        let n = || Box::new(VariableType::node_star());
        assert!(VariableType::Union(zero(), zero()).is_empty());
        assert!(!VariableType::Union(zero(), n()).is_empty());
        assert!(!VariableType::Union(n(), zero()).is_empty());
    }

    // join — Zero is identity, equal-collapse.
    #[test]
    fn test_join_drops_left_zero() {
        let n = VariableType::node_star();
        assert_eq!(VariableType::join(VariableType::Zero, n.clone()), n);
    }
    #[test]
    fn test_join_drops_right_zero() {
        let n = VariableType::node_star();
        assert_eq!(VariableType::join(n.clone(), VariableType::Zero), n);
    }
    #[test]
    fn test_join_collapses_equal_operands() {
        let n = VariableType::node_star();
        assert_eq!(VariableType::join(n.clone(), n.clone()), n);
    }

    // meet — Node preservation + descriptor combination.
    #[test]
    fn test_meet_same_node_returns_same() {
        let n = node_with_label("Person");
        assert_eq!(VariableType::meet(&n, &n), n);
    }
    #[test]
    fn test_meet_distinct_node_atoms_collapses_descriptor() {
        // meet of two label-distinct Nodes should produce a Node whose
        // descriptor's label is the And.
        let na = node_with_label("A");
        let nb = node_with_label("B");
        match VariableType::meet(&na, &nb) {
            VariableType::Node(d) => {
                assert!(
                    matches!(d.label, LabelType::And(_, _)),
                    "meet of (:A) and (:B) should have And label, got {d}"
                );
            }
            other => panic!("meet of two Nodes should be Node, got {other:?}"),
        }
    }

    // refine — schema admission.
    #[test]
    fn test_refine_with_no_matching_label_returns_zero() {
        // Schema with only `Person`; query for `Animal` → ⊥.
        let schema = Schema::from_parts(vec![node_with_label("Person")], vec![]);
        let q = node_with_label("Animal");
        assert_eq!(VariableType::refine(&schema, &q), VariableType::Zero);
    }
}
