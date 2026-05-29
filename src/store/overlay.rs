//! In-RAM mutation overlay over a `LazyGraphStore`.
//!
//! Holds nodes and edges inserted during the session plus tombstones for
//! deletions. Read paths consult the overlay before falling through to the
//! page-cache-backed disk store, so the on-disk image stays untouched until
//! `save()` materializes the merged view.
//!
//! MVP-0 does not yet track per-record property modifications (those land
//! when SET / REMOVE arrive in MVP-1). Today the overlay only handles:
//!   * insertion of new nodes / edges (with IDs `>= base_*_count`)
//!   * deletion (tombstone) of existing or newly-inserted nodes / edges
//!   * adjacency lists for newly-inserted edges (the base CSR stays
//!     read-only; reads merge the two slices)
//!
//! See plan §"Arquitectura: overlay sobre LazyGraphStore" for the full
//! design rationale (overlay vs. materialize-to-MemoryGraphStore vs. in-place).

use std::collections::{HashMap, HashSet};

use crate::model::graph::Props;
use crate::model::value::{Id, Value};
use crate::typing::label_type::LabelType;

/// Decoded representation of a freshly inserted node.
#[derive(Debug, Clone)]
pub struct OverlayNode {
    pub labels: LabelType,
    pub props: Props,
}

/// Decoded representation of a freshly inserted edge.
#[derive(Debug, Clone)]
pub struct OverlayEdge {
    pub src: Id,
    pub tgt: Id,
    pub directed: bool,
    pub labels: LabelType,
    pub props: Props,
}

/// Per-record property mutation accumulated during a session. Set on a
/// base-id node/edge once the user runs `SET` or `REMOVE` against it.
/// New (overlay-allocated) records modify their `OverlayNode`/`OverlayEdge`
/// entry in place rather than going through `PropMods`.
#[derive(Debug, Default, Clone)]
pub struct PropMods {
    /// `true` after `SET x = { ... }` cleared the base property set.
    /// Reads then return only the entries in `set` (with `Some` values),
    /// ignoring disk-side props entirely.
    pub cleared: bool,
    /// Per-prop ops: `Some(v)` overwrites, `None` removes (used by
    /// MVP-1.C `REMOVE x.prop`).
    pub set: HashMap<String, Option<Value>>,
}

/// Per-record label mutation accumulated during a session. Used by
/// MVP-1.D `SET x:Label` / `REMOVE x:Label` (ISO §13.3 / §13.4) on
/// base-id nodes / edges; overlay-allocated records mutate their
/// `OverlayNode`/`OverlayEdge.labels` field directly. The two sets stay
/// disjoint — `add_*_label` removes the name from `removed` and inserts
/// into `added`, and `remove_*_label` does the inverse — so reads
/// reconstruct the effective label set as `(base ∪ added) \ removed`.
#[derive(Debug, Default, Clone)]
pub struct LabelMods {
    pub added: HashSet<String>,
    pub removed: HashSet<String>,
}

/// All mutations applied since the store was opened or last saved.
///
/// IDs handed out for new nodes / edges are dense above the base counts,
/// so `id - base_node_count` indexes into `new_nodes`.
#[derive(Debug, Default)]
pub struct MutationOverlay {
    pub base_node_count: u32,
    pub base_edge_count: u32,

    pub new_nodes: Vec<OverlayNode>,
    pub new_edges: Vec<OverlayEdge>,

    pub deleted_nodes: HashSet<Id>,
    pub deleted_edges: HashSet<Id>,

    /// Property-mutation overlay over base records. Reads of node/edge
    /// props that hit a base id consult these maps after fetching disk
    /// props. New (overlay-allocated) records mutate their
    /// `OverlayNode`/`OverlayEdge` directly and bypass these.
    pub mod_node_props: HashMap<Id, PropMods>,
    pub mod_edge_props: HashMap<Id, PropMods>,

    /// Label-mutation overlay over base records. Reads of node/edge
    /// labels that hit a base id consult these maps after fetching disk
    /// labels. Overlay-allocated records mutate their
    /// `OverlayNode`/`OverlayEdge.labels` directly. (MVP-1.D)
    pub mod_node_labels: HashMap<Id, LabelMods>,
    pub mod_edge_labels: HashMap<Id, LabelMods>,

    /// Outgoing directed edges per source node, only for edges with
    /// `id >= base_edge_count`. Base edges live in the CSR and the
    /// reader merges both views.
    pub new_outgoing: HashMap<Id, Vec<Id>>,
    /// Incoming directed edges per target node, new edges only.
    pub new_incoming: HashMap<Id, Vec<Id>>,
    /// Undirected adjacency per endpoint, new edges only. An undirected
    /// edge is recorded once per endpoint (so both `new_undirected[src]`
    /// and `new_undirected[tgt]` contain the same edge id).
    pub new_undirected: HashMap<Id, Vec<Id>>,
}

impl MutationOverlay {
    pub fn new(base_node_count: u32, base_edge_count: u32) -> Self {
        Self {
            base_node_count,
            base_edge_count,
            ..Default::default()
        }
    }

    pub fn next_node_id(&self) -> Id {
        self.base_node_count + self.new_nodes.len() as u32
    }

    pub fn next_edge_id(&self) -> Id {
        self.base_edge_count + self.new_edges.len() as u32
    }

    pub fn is_node_deleted(&self, id: Id) -> bool {
        self.deleted_nodes.contains(&id)
    }

    pub fn is_edge_deleted(&self, id: Id) -> bool {
        self.deleted_edges.contains(&id)
    }

    /// Look up a freshly inserted node by id. Returns `None` if `id` does
    /// not correspond to an overlay-tracked node (either it is a base id or
    /// it has never been allocated).
    pub fn get_new_node(&self, id: Id) -> Option<&OverlayNode> {
        if id < self.base_node_count {
            return None;
        }
        let off = (id - self.base_node_count) as usize;
        self.new_nodes.get(off)
    }

    pub fn get_new_edge(&self, id: Id) -> Option<&OverlayEdge> {
        if id < self.base_edge_count {
            return None;
        }
        let off = (id - self.base_edge_count) as usize;
        self.new_edges.get(off)
    }

    /// Schedule a new node and return its id.
    pub fn insert_node(&mut self, labels: LabelType, props: Props) -> Id {
        let id = self.next_node_id();
        self.new_nodes.push(OverlayNode { labels, props });
        id
    }

    /// Schedule a new edge and update the appropriate adjacency map.
    pub fn insert_edge(
        &mut self,
        src: Id,
        tgt: Id,
        directed: bool,
        labels: LabelType,
        props: Props,
    ) -> Id {
        let id = self.next_edge_id();
        self.new_edges.push(OverlayEdge {
            src,
            tgt,
            directed,
            labels,
            props,
        });
        if directed {
            self.new_outgoing.entry(src).or_default().push(id);
            self.new_incoming.entry(tgt).or_default().push(id);
        } else {
            self.new_undirected.entry(src).or_default().push(id);
            if src != tgt {
                self.new_undirected.entry(tgt).or_default().push(id);
            }
        }
        id
    }

    /// Mark an edge as deleted. If it was inserted in this session, leave
    /// the slot in `new_edges` (we keep it for id stability) and just add
    /// the tombstone — every reader filters by `is_edge_deleted`.
    pub fn delete_edge(&mut self, id: Id) {
        self.deleted_edges.insert(id);
    }

    /// Mark a node as deleted. Edges incident to it must be handled by the
    /// caller (NODETACH validation lives in the runtime, not here).
    pub fn delete_node(&mut self, id: Id) {
        self.deleted_nodes.insert(id);
    }

    /// Restore the overlay to "no mutations applied". Used by save() after
    /// the merged view is persisted: the base counts shift to the new file
    /// and overlay starts empty again.
    pub fn clear(&mut self, new_base_node_count: u32, new_base_edge_count: u32) {
        self.base_node_count = new_base_node_count;
        self.base_edge_count = new_base_edge_count;
        self.new_nodes.clear();
        self.new_edges.clear();
        self.deleted_nodes.clear();
        self.deleted_edges.clear();
        self.mod_node_props.clear();
        self.mod_edge_props.clear();
        self.mod_node_labels.clear();
        self.mod_edge_labels.clear();
        self.new_outgoing.clear();
        self.new_incoming.clear();
        self.new_undirected.clear();
    }
}

// --- Label-mutation helpers shared by every overlay-backed backend ---
// (LazyGraphStore and MemoryGraphStore both apply these against the base
// label set when reconstructing the merged view.)

/// Apply a `LabelMods` (if any) to an in-place vector of label strings:
/// drops every name in `removed`, then appends every name in `added`
/// that is not already present. Preserves the relative order of base
/// labels for stable display.
pub(crate) fn apply_label_mods(labels: &mut Vec<String>, mods: Option<&LabelMods>) {
    let Some(mods) = mods else { return };
    if !mods.removed.is_empty() {
        labels.retain(|l| !mods.removed.contains(l));
    }
    for l in &mods.added {
        if !labels.iter().any(|x| x == l) {
            labels.push(l.clone());
        }
    }
}

/// Reverse of the on-disk label decode over a string vector. Empty input
/// collapses to `Star` to mirror the "no labels" encoding.
pub(crate) fn labels_from_strings(labels: Vec<String>) -> LabelType {
    if labels.is_empty() {
        LabelType::Star
    } else {
        LabelType::from_list(&labels)
    }
}

/// Add `label` to a label type (a `Label`, `And` chain, or `Star`).
/// Idempotent — the label is only appended when not already present.
pub(crate) fn label_type_with_added(lt: &LabelType, label: &str) -> LabelType {
    let mut labels = crate::model::graph::MemoryGraphStore::label_strings(lt);
    if !labels.iter().any(|l| l == label) {
        labels.push(label.to_string());
    }
    labels_from_strings(labels)
}

/// Drop `label` from a label type. Idempotent — missing labels are a
/// no-op (ISO §13.4 GR4 b).
pub(crate) fn label_type_with_removed(lt: &LabelType, label: &str) -> LabelType {
    let mut labels = crate::model::graph::MemoryGraphStore::label_strings(lt);
    labels.retain(|l| l != label);
    labels_from_strings(labels)
}
