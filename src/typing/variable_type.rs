use std::fmt;

use super::descriptor_type::DescriptorType;
use super::simple_type::SimpleType;

/// Types for pattern variables (nodes, edges, unions, lists, bottom).
#[derive(Debug, Clone, PartialEq, Eq)]
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
    List(Box<VariableType>),
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
            VariableType::List(t) => SimpleType::List(Box::new(t.get_attribute(attr))),
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
            (VariableType::List(ta), VariableType::List(tb)) => {
                VariableType::List(Box::new(VariableType::meet(ta, tb)))
            }
            (VariableType::Node(da), VariableType::Node(db)) => Self::meet_node(da, db),
            (
                VariableType::EdgeDirectional { desc: d1, left: l1, right: r1 },
                VariableType::EdgeDirectional { desc: d2, left: l2, right: r2 },
            ) => Self::meet_edge_directional(d1, l1, r1, d2, l2, r2),
            (
                VariableType::EdgeNonDirectional { desc: d1, left: l1, right: r1 },
                VariableType::EdgeNonDirectional { desc: d2, left: l2, right: r2 },
            ) => {
                // Try both orientations, join the results
                let n1 = Self::meet_edge_directional(d1, l1, r1, d2, l2, r2);
                let n2 = Self::meet_edge_directional(d1, l1, r1, d2, r2, l2);
                match (&n1, &n2) {
                    (
                        VariableType::EdgeDirectional { desc: da, left: la, right: ra },
                        VariableType::EdgeDirectional { desc: db, left: lb, right: rb },
                    ) => VariableType::join(
                        &VariableType::EdgeNonDirectional {
                            desc: da.clone(),
                            left: la.clone(),
                            right: ra.clone(),
                        },
                        &VariableType::EdgeNonDirectional {
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
                match (&r1, &r2) {
                    (VariableType::Zero, _) => r2,
                    (_, VariableType::Zero) => r1,
                    _ => VariableType::Union(Box::new(r1), Box::new(r2)),
                }
            }
            (_, VariableType::Union(_, _)) => VariableType::meet(b, a),
            (VariableType::Zero, _) | (_, VariableType::Zero) => VariableType::Zero,
            _ => VariableType::Zero,
        }
    }

    // --- Join ---

    pub fn join(a: &VariableType, b: &VariableType) -> VariableType {
        if *a == VariableType::Zero {
            return b.clone();
        }
        if *b == VariableType::Zero {
            return a.clone();
        }
        if a == b {
            return a.clone();
        }
        VariableType::Union(Box::new(a.clone()), Box::new(b.clone()))
    }

    pub fn join_from_list(types: &[VariableType]) -> VariableType {
        if types.is_empty() {
            return VariableType::Zero;
        }
        types.iter().skip(1).fold(types[0].clone(), |acc, t| VariableType::join(&acc, t))
    }

    // --- Subtyping ---

    pub fn is_subtype(t1: &VariableType, t2: &VariableType) -> bool {
        match (t1, t2) {
            (VariableType::Node(a), VariableType::Node(b)) => DescriptorType::is_subtype(a, b),
            (
                VariableType::EdgeDirectional { desc: d1, left: l1, right: r1 },
                VariableType::EdgeDirectional { desc: d2, left: l2, right: r2 },
            ) => {
                DescriptorType::is_subtype(d1, d2)
                    && match (l1.as_ref(), l2.as_ref()) {
                        (VariableType::Node(a), VariableType::Node(b)) => DescriptorType::is_subtype(a, b),
                        _ => false,
                    }
                    && match (r1.as_ref(), r2.as_ref()) {
                        (VariableType::Node(a), VariableType::Node(b)) => DescriptorType::is_subtype(a, b),
                        _ => false,
                    }
            }
            (
                VariableType::EdgeNonDirectional { desc: d1, left: l1, right: r1 },
                VariableType::EdgeNonDirectional { desc: d2, left: l2, right: r2 },
            ) => {
                // Symmetric check
                let as_dir1 = VariableType::EdgeDirectional {
                    desc: d1.clone(), left: l1.clone(), right: r1.clone(),
                };
                let as_dir2a = VariableType::EdgeDirectional {
                    desc: d2.clone(), left: l2.clone(), right: r2.clone(),
                };
                let as_dir2b = VariableType::EdgeDirectional {
                    desc: d2.clone(), left: r2.clone(), right: l2.clone(),
                };
                VariableType::is_subtype(&as_dir1, &as_dir2a)
                    || VariableType::is_subtype(&as_dir1, &as_dir2b)
            }
            _ => false,
        }
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
                VariableType::join_from_list(&matches)
            }
            VariableType::EdgeDirectional { .. } | VariableType::EdgeNonDirectional { .. } => {
                let matches: Vec<VariableType> = schema
                    .edges
                    .iter()
                    .filter(|e| VariableType::is_subtype(e, node))
                    .map(|e| VariableType::meet(e, node))
                    .collect();
                VariableType::join_from_list(&matches)
            }
            VariableType::Union(t1, t2) => VariableType::join(
                &VariableType::refine(schema, t1),
                &VariableType::refine(schema, t2),
            ),
            VariableType::List(t) => {
                VariableType::List(Box::new(VariableType::refine(schema, t)))
            }
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
            VariableType::List(t) => t.is_empty(),
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
            VariableType::List(t) => write!(f, "[{t}]"),
            VariableType::Zero => write!(f, "⊥"),
        }
    }
}

/// Schema: a set of allowed node and edge types.
pub struct Schema {
    pub nodes: Vec<VariableType>,
    pub edges: Vec<VariableType>,
}

impl Schema {
    /// Permissive schema that allows anything.
    pub fn star() -> Self {
        Schema {
            nodes: vec![VariableType::node_star()],
            edges: vec![
                VariableType::edge_directional(DescriptorType::star()),
                VariableType::edge_non_directional(DescriptorType::star()),
            ],
        }
    }
}
