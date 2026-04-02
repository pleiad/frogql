use crate::model::graph::Props;
use crate::model::value::PathValue;
use crate::typing::label_type::LabelType;

/// Trait abstracting graph data access.
/// `Graph` implements this for in-memory access. `LazyGraphStore` (future) will
/// implement it for on-demand page-cache-backed access to large graphs.
pub trait GraphAccess {
    /// All node IDs.
    fn nodes(&self) -> Vec<String>;
    /// All directed edge IDs.
    fn edges_directed(&self) -> Vec<String>;
    /// All undirected edge IDs.
    fn edges_undirected(&self) -> Vec<String>;
    /// Label type for a node or edge by its user ID.
    fn labels(&self, id: &str) -> &LabelType;
    /// Properties for a node or edge by its user ID.
    fn props(&self, id: &str) -> &Props;
    /// Source node ID of a directed edge.
    fn src(&self, edge_id: &str) -> &str;
    /// Target node ID of a directed edge.
    fn tgt(&self, edge_id: &str) -> &str;
    /// Both endpoints of an edge (src, tgt for directed; ep0, ep1 for undirected).
    fn endpoints(&self, edge_id: &str) -> (&str, &str);
    /// Whether an edge is directed.
    fn is_directed(&self, edge_id: &str) -> bool;
    /// Build the PathValue for an edge (EdgeDirectional or EdgeUndirectional).
    fn edge_path_value(&self, edge_id: &str) -> PathValue;

    // --- Index-aware methods (optimizer uses these) ---

    /// Nodes with a specific label. Returns None if no index available (fallback to scan).
    fn nodes_with_label(&self, _label: &str) -> Option<Vec<String>> { None }
    /// Directed edges with a specific label.
    fn directed_edges_with_label(&self, _label: &str) -> Option<Vec<String>> { None }
    /// Undirected edges with a specific label.
    fn undirected_edges_with_label(&self, _label: &str) -> Option<Vec<String>> { None }

    // --- Adjacency methods (optimizer uses these for concat) ---

    /// Outgoing directed edges from a node.
    fn outgoing_edges(&self, _node_id: &str) -> Vec<String> { vec![] }
    /// Incoming directed edges to a node.
    fn incoming_edges(&self, _node_id: &str) -> Vec<String> { vec![] }
    /// Undirected edges connected to a node.
    fn undirected_edges_of(&self, _node_id: &str) -> Vec<String> { vec![] }
}
