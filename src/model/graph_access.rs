use crate::model::graph::Props;
use crate::model::value::{Id, PathValue, Value};
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

    // --- Secondary index ---

    /// Look up node IDs by `(label, prop) = value` via a secondary index, if
    /// one exists. `Some(vec![])` means the index exists but no node matches;
    /// `None` means there is no index for this (label, prop) and the caller
    /// must fall back to a scan. Default impl: no indexes.
    fn lookup_node_eq(&self, _label: &str, _prop: &str, _value: &Value) -> Option<Vec<Id>> {
        None
    }

    /// Range lookup against a btree secondary index. Returns Some(matching
    /// node IDs) when a btree exists for `(label, prop)`, None otherwise.
    /// Bounds follow `std::ops::Bound` semantics. Default impl: no indexes.
    fn lookup_node_range(
        &self,
        _label: &str,
        _prop: &str,
        _lo: std::ops::Bound<Value>,
        _hi: std::ops::Bound<Value>,
    ) -> Option<Vec<Id>> {
        None
    }

    /// All node IDs carrying `(label, prop)`, returned in sort order over
    /// the btree's keys. `ascending = true` walks ASC, `false` walks DESC.
    /// Returns None when no btree index exists for `(label, prop)` —
    /// caller must fall back to a runtime sort. Used by the BTree-driven
    /// ORDER BY path. Default impl: no indexes.
    fn lookup_node_ordered(&self, _label: &str, _prop: &str, _ascending: bool) -> Option<Vec<Id>> {
        None
    }
}

/// Shape of a `dependent object error` raised by NODETACH DELETE when a
/// node still has incident edges (ISO §13.5 GR5, SQLSTATE G1001).
#[derive(Debug, Clone)]
pub struct G1001 {
    pub node: Id,
    pub remaining_edges: Vec<Id>,
}

impl std::fmt::Display for G1001 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "G1001: dependent object error — node {} still has {} edge(s); use DETACH DELETE",
            self.node,
            self.remaining_edges.len()
        )
    }
}

impl std::error::Error for G1001 {}

/// Mutation API for graph backends that accept `INSERT` / `DELETE` /
/// `DETACH DELETE`. Methods take `&self` because the underlying mutation
/// state lives behind a `RefCell` overlay; this preserves the existing
/// `Runtime` lifetime that already plumbs `&G` through the engine.
///
/// MVP-0 surface (see plan §"Fase 2"). SET / REMOVE arrive in MVP-1.
pub trait GraphAccessMut: GraphAccess {
    /// Insert a brand-new node and return its allocated internal id. The
    /// returned id is dense above the previous high watermark; deletions
    /// create tombstones rather than gaps that future inserts can fill.
    fn insert_node(&self, labels: LabelType, props: crate::model::graph::Props) -> Id;

    /// Insert a directed (`directed = true`) or undirected edge between
    /// two existing endpoints. `src` and `tgt` must satisfy
    /// `is_node_alive`; otherwise the implementation raises
    /// `dependent object error — endpoint node not in current working
    /// graph (G1003)` per ISO §13.2 GR9.
    fn insert_edge(
        &self,
        src: Id,
        tgt: Id,
        directed: bool,
        labels: LabelType,
        props: crate::model::graph::Props,
    ) -> Id;

    /// Tombstone an edge. Idempotent (re-deleting a tombstoned edge is a
    /// no-op).
    fn delete_edge(&self, id: Id);

    /// Tombstone a node and every edge incident to it. ISO §13.5 SR6:
    /// the implicit DELETE form is NODETACH; callers reach this entry
    /// only when the parser saw an explicit `DETACH DELETE`.
    fn detach_delete_node(&self, id: Id);

    /// Tombstone a node only when it has zero live incident edges. Maps
    /// directly to `NODETACH DELETE`. Returns `Err(G1001)` (without
    /// mutating anything) when the node still has edges.
    fn delete_node_no_detach(&self, id: Id) -> Result<(), G1001>;

    /// `true` iff the node id resolves to a live node (existing on disk
    /// and not tombstoned, or a freshly inserted overlay node).
    fn is_node_alive(&self, id: Id) -> bool;

    /// `true` iff the edge id resolves to a live edge.
    fn is_edge_alive(&self, id: Id) -> bool;

    /// Drop every overlay change since the store was opened or last
    /// saved. Used by the runtime when a DML statement aborts midway
    /// (atomicity per ISO §13.5 Note 196).
    fn rollback_session(&self);

    // --- ISO §13.3 SET (MVP-1.B) -------------------------------------

    /// `SET x.prop = value` on a node. Idempotent within a session: a
    /// later `SET` on the same `(id, prop)` overwrites the earlier
    /// staged value.
    fn set_node_prop(&self, id: Id, prop: &str, value: crate::model::value::Value);

    /// `SET x.prop = value` on an edge.
    fn set_edge_prop(&self, id: Id, prop: &str, value: crate::model::value::Value);

    /// `SET x = { ... }` (clear+set) on a node. ISO §13.3 GR8 b.i:
    /// every existing property of `x` is removed first, then the given
    /// map is applied. The implementation marks the record as cleared
    /// in the overlay; readers ignore disk props and only see the new
    /// set. New (overlay-allocated) nodes overwrite their props in
    /// place.
    fn replace_node_props(&self, id: Id, props: crate::model::graph::Props);

    /// `SET x = { ... }` (clear+set) on an edge.
    fn replace_edge_props(&self, id: Id, props: crate::model::graph::Props);

    // --- ISO §13.4 REMOVE (MVP-1.C) ----------------------------------

    /// `REMOVE x.prop` on a node. Idempotent: removing a missing
    /// property is a no-op (matches ISO §13.4 GR4 a — the property is
    /// only removed "if it is contained in the property set").
    fn remove_node_prop(&self, id: Id, prop: &str);

    /// `REMOVE x.prop` on an edge.
    fn remove_edge_prop(&self, id: Id, prop: &str);

    // --- ISO §13.3 / §13.4 SET / REMOVE labels (MVP-1.D, Feature GD02)

    /// `SET x:Label`. Idempotent: adding a label the element already
    /// carries is a no-op. The same call cancels any previously staged
    /// `REMOVE x:Label` so reads see only the latest intent.
    fn add_node_label(&self, id: Id, label: &str);

    /// `SET x:Label` on an edge. Same idempotency rules as nodes.
    fn add_edge_label(&self, id: Id, label: &str);

    /// `REMOVE x:Label`. Idempotent — removing a label the element does
    /// not carry is a no-op (ISO §13.4 GR4 b). Cancels any previously
    /// staged `SET x:Label` for the same `(id, label)` pair.
    fn remove_node_label(&self, id: Id, label: &str);

    /// `REMOVE x:Label` on an edge.
    fn remove_edge_label(&self, id: Id, label: &str);
}
