use crate::model::graph::Props;
use crate::model::value::{Id, PathValue};
use crate::typing::label_type::LabelType;

/// Trait abstracting graph data access.
/// All IDs are internal u32 identifiers. String conversion is only at boundaries.
pub trait GraphAccess {
    /// All node internal IDs.
    fn nodes(&self) -> Vec<Id>;
    /// All directed edge internal IDs.
    fn edges_directed(&self) -> Vec<Id>;
    /// All undirected edge internal IDs.
    fn edges_undirected(&self) -> Vec<Id>;
    /// Label type for a node by internal ID.
    fn node_labels(&self, id: Id) -> LabelType;
    /// Label type for an edge by internal ID.
    fn edge_labels(&self, id: Id) -> LabelType;
    /// Properties for a node by internal ID.
    fn node_props(&self, id: Id) -> Props;
    /// Properties for an edge by internal ID.
    fn edge_props(&self, id: Id) -> Props;
    /// Source node ID of a directed edge.
    fn src(&self, edge_id: Id) -> Id;
    /// Target node ID of a directed edge.
    fn tgt(&self, edge_id: Id) -> Id;
    /// Whether an edge is directed.
    fn is_directed(&self, edge_id: Id) -> bool;
    /// Build the PathValue for an edge (EdgeDirectional or EdgeUndirectional).
    fn edge_path_value(&self, edge_id: Id) -> PathValue;

    /// Resolve an internal node ID to its user-facing string name.
    fn node_name(&self, id: Id) -> &str;
    /// Resolve an internal edge ID to its user-facing string name.
    fn edge_name(&self, id: Id) -> &str;

    // --- Index-aware methods ---

    /// Nodes with a specific label.
    fn nodes_with_label(&self, _label: &str) -> Option<Vec<Id>> {
        None
    }
    /// Directed edges with a specific label.
    fn directed_edges_with_label(&self, _label: &str) -> Option<Vec<Id>> {
        None
    }
    /// Undirected edges with a specific label.
    fn undirected_edges_with_label(&self, _label: &str) -> Option<Vec<Id>> {
        None
    }

    // --- Adjacency methods ---

    /// Outgoing directed edges from a node.
    fn outgoing_edges(&self, node_id: Id) -> Vec<Id> {
        let _ = node_id;
        vec![]
    }
    /// Incoming directed edges to a node.
    fn incoming_edges(&self, node_id: Id) -> Vec<Id> {
        let _ = node_id;
        vec![]
    }
    /// Undirected edges connected to a node.
    fn undirected_edges_of(&self, node_id: Id) -> Vec<Id> {
        let _ = node_id;
        vec![]
    }
}
