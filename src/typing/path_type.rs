//! Path types — the shape of a sequence of nodes and edges.
//!
//! Ported from `fppc/src/typechecker/path_type.rs` per option 1 of the
//! migration plan: a parallel module alongside `variable_type.rs`. The path
//! shape is computed by the checker in `check_path_pattern`; consumers
//! (Concat, Union, Repeat) operate on the parallel `PathType` lattice.
//!
//! The known costs of this representation (clones at concat/union, parallel
//! info with `VariableType` at every edge position) are documented in
//! `docs/internals/typechecker_migration.md` and accepted for phase 1.

use super::descriptor_type::DescriptorType;
use super::variable_type::{Schema, VariableType};

/// Direction at which an edge is observed in a path. Mirrors fppc's
/// `ast::EdgeDirection`. `None` means "no direction declared at this
/// position" — gqlite's `EdgeUndirected` lowers to this; `Any` covers
/// `EdgeAnyDirection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeDir {
    Right,
    Left,
    Any,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodePathType {
    pub desc: DescriptorType,
}

impl NodePathType {
    pub fn new(desc: DescriptorType) -> Self {
        NodePathType { desc }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgePathType {
    pub p1: Box<PathType>,
    pub n2: NodePathType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathType {
    /// A single node in the path.
    Node(NodePathType),
    /// An edge connecting a path to a node: `path - node`.
    Edge(EdgePathType),
    /// Union of two path types.
    Union(Box<PathType>, Box<PathType>),
    /// Bottom — empty / inconsistent path.
    Zero,
}

impl Default for PathType {
    fn default() -> Self {
        PathType::Node(NodePathType::new(DescriptorType::star()))
    }
}

impl PathType {
    pub fn node(desc: DescriptorType) -> Self {
        PathType::Node(NodePathType::new(desc))
    }

    /// Build a `PathType` from a `VariableType` and the direction at which
    /// the corresponding edge is observed. Mirrors fppc's
    /// `From<(&VariableType, EdgeDirection)>`.
    pub fn from_variable(t: &VariableType, dir: EdgeDir) -> PathType {
        match t {
            VariableType::Node(d) => PathType::node(d.clone()),
            VariableType::EdgeDirectional { left, right, .. } => {
                directed_edge_to_path(left, right, dir)
            }
            VariableType::EdgeNonDirectional { left, right, .. } => {
                // Treat undirected as both orientations regardless of dir.
                directed_edge_to_path(left, right, EdgeDir::Any)
            }
            VariableType::Union(t1, t2) => PathType::union(
                PathType::from_variable(t1, dir),
                PathType::from_variable(t2, dir),
            ),
            VariableType::Group(_)
            | VariableType::Null
            | VariableType::Path
            | VariableType::Zero => PathType::Zero,
        }
    }

    /// Number of edges in the path (0 for a single node).
    pub fn len(&self) -> usize {
        match self {
            PathType::Node(_) => 0,
            PathType::Edge(e) => e.p1.len() + 1,
            PathType::Union(p1, p2) => p1.len().min(p2.len()),
            PathType::Zero => 0,
        }
    }

    /// True when the path has no edges.
    pub fn is_empty(&self) -> bool {
        match self {
            PathType::Node(_) | PathType::Zero => true,
            PathType::Edge(_) => false,
            PathType::Union(p1, p2) => p1.is_empty() || p2.is_empty(),
        }
    }

    /// Bottom check.
    pub fn is_unsatisfiable(&self) -> bool {
        match self {
            PathType::Zero => true,
            PathType::Node(n) => n.desc.is_empty(),
            PathType::Edge(e) => e.p1.is_unsatisfiable() || e.n2.desc.is_empty(),
            PathType::Union(p1, p2) => p1.is_unsatisfiable() && p2.is_unsatisfiable(),
        }
    }

    pub fn union(p1: PathType, p2: PathType) -> PathType {
        match (&p1, &p2) {
            (PathType::Zero, _) => p2,
            (_, PathType::Zero) => p1,
            _ if p1 == p2 => p1,
            _ => PathType::Union(Box::new(p1), Box::new(p2)),
        }
    }

    pub fn union_from_list(paths: Vec<PathType>) -> PathType {
        paths
            .into_iter()
            .reduce(PathType::union)
            .unwrap_or(PathType::Zero)
    }

    /// Greatest lower bound. Combines two path fragments — used by `Concat`
    /// in the checker. The implementation walks both shapes structurally and
    /// uses the schema to refine the descriptors at each shared node
    /// position.
    pub fn meet(schema: &Schema, p1: &PathType, p2: &PathType) -> PathType {
        match (p1, p2) {
            (PathType::Zero, _) | (_, PathType::Zero) => PathType::Zero,

            (PathType::Node(n1), PathType::Node(n2)) => {
                let met = VariableType::Node(DescriptorType::meet(&n1.desc, &n2.desc));
                let nodes = VariableType::refine_to_nodes(schema, &met);
                PathType::union_from_list(
                    nodes
                        .into_iter()
                        .filter_map(node_descriptor)
                        .map(PathType::node)
                        .collect(),
                )
            }

            (PathType::Edge(p1_edge), PathType::Node(n2)) => {
                let met = VariableType::Node(DescriptorType::meet(&p1_edge.n2.desc, &n2.desc));
                let nodes = VariableType::refine_to_nodes(schema, &met);
                PathType::union_from_list(
                    nodes
                        .into_iter()
                        .filter_map(node_descriptor)
                        .map(|d| {
                            PathType::Edge(EdgePathType {
                                p1: p1_edge.p1.clone(),
                                n2: NodePathType::new(d),
                            })
                        })
                        .collect(),
                )
            }

            (_, PathType::Edge(p2_edge)) => PathType::Edge(EdgePathType {
                p1: Box::new(PathType::meet(schema, p1, &p2_edge.p1)),
                n2: p2_edge.n2.clone(),
            }),

            (_, PathType::Union(u1, u2)) => {
                let m1 = PathType::meet(schema, p1, u1);
                let m2 = PathType::meet(schema, p1, u2);
                if m1.is_unsatisfiable() {
                    return m2;
                }
                if m2.is_unsatisfiable() {
                    return m1;
                }
                PathType::union(m1, m2)
            }

            (PathType::Union(u1, u2), _) => {
                let m1 = PathType::meet(schema, u1, p2);
                let m2 = PathType::meet(schema, u2, p2);
                if m1.is_unsatisfiable() {
                    return m2;
                }
                if m2.is_unsatisfiable() {
                    return m1;
                }
                PathType::union(m1, m2)
            }
        }
    }
}

fn directed_edge_to_path(left: &VariableType, right: &VariableType, dir: EdgeDir) -> PathType {
    let left_desc = match left {
        VariableType::Node(d) => d.clone(),
        _ => return PathType::Zero,
    };
    let right_desc = match right {
        VariableType::Node(d) => d.clone(),
        _ => return PathType::Zero,
    };

    let forward = PathType::Edge(EdgePathType {
        p1: Box::new(PathType::node(left_desc.clone())),
        n2: NodePathType::new(right_desc.clone()),
    });
    let reversed = PathType::Edge(EdgePathType {
        p1: Box::new(PathType::node(right_desc)),
        n2: NodePathType::new(left_desc),
    });

    match dir {
        EdgeDir::Right => forward,
        EdgeDir::Left => reversed,
        EdgeDir::Any | EdgeDir::None => PathType::union(forward, reversed),
    }
}

fn node_descriptor(v: VariableType) -> Option<DescriptorType> {
    match v {
        VariableType::Node(d) => Some(d),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typing::label_type::LabelType;
    use crate::typing::property_type::PropertyType;

    // is_unsatisfiable — bottom check for paths.
    #[test]
    fn test_zero_is_unsatisfiable() {
        assert!(PathType::Zero.is_unsatisfiable());
    }
    #[test]
    fn test_node_unsat_iff_descriptor_empty() {
        let empty_d = DescriptorType::new(LabelType::Star, PropertyType::Zero);
        assert!(PathType::node(empty_d).is_unsatisfiable());
        assert!(!PathType::node(DescriptorType::star()).is_unsatisfiable());
    }
    #[test]
    fn test_union_unsat_iff_both_unsat() {
        // Different from `is_empty` (which uses OR for length min) —
        // `is_unsatisfiable` uses AND for Union.
        let n = || PathType::node(DescriptorType::star());
        assert!(
            PathType::Union(Box::new(PathType::Zero), Box::new(PathType::Zero)).is_unsatisfiable()
        );
        assert!(!PathType::Union(Box::new(PathType::Zero), Box::new(n())).is_unsatisfiable());
        assert!(!PathType::Union(Box::new(n()), Box::new(PathType::Zero)).is_unsatisfiable());
    }

    // union — Zero is identity for join.
    #[test]
    fn test_union_drops_left_zero() {
        let n = PathType::node(DescriptorType::star());
        assert_eq!(PathType::union(PathType::Zero, n.clone()), n);
    }
    #[test]
    fn test_union_drops_right_zero() {
        let n = PathType::node(DescriptorType::star());
        assert_eq!(PathType::union(n.clone(), PathType::Zero), n);
    }
}
