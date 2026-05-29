use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json;

use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, PathValue, Value};
use crate::store::overlay::{
    apply_label_mods, label_type_with_added, label_type_with_removed, labels_from_strings,
    MutationOverlay,
};
use crate::typing::label_type::LabelType;

/// Property map: attribute name → Value.
pub type Props = HashMap<String, Value>;

/// A property graph with u32 internal IDs.
///
/// String user-facing IDs (from JSON) are mapped to sequential u32 IDs at load time.
/// All internal operations use u32 IDs for performance.
pub struct MemoryGraphStore {
    // --- User-facing names (indexed by internal ID) ---
    pub node_names: Vec<String>,
    pub edge_names: Vec<String>,

    // --- Topology ---
    pub edge_src: Vec<Id>,        // edge internal ID → src node internal ID
    pub edge_tgt: Vec<Id>,        // edge internal ID → tgt node internal ID
    pub edge_directed: Vec<bool>, // edge internal ID → is directed?

    // --- Data (indexed by internal ID) ---
    pub node_labels: Vec<LabelType>,
    pub edge_labels: Vec<LabelType>,
    pub node_props: Vec<Props>,
    pub edge_props: Vec<Props>,

    // --- Indexes ---
    label_to_nodes: HashMap<String, Vec<Id>>,
    label_to_edges_d: HashMap<String, Vec<Id>>,
    label_to_edges_u: HashMap<String, Vec<Id>>,
    outgoing: Vec<Vec<Id>>, // node internal ID → outgoing edge internal IDs
    incoming: Vec<Vec<Id>>, // node internal ID → incoming edge internal IDs
    undirected_adj: Vec<Vec<Id>>, // node internal ID → undirected edge internal IDs

    // --- Reverse lookup (user name → internal ID) ---
    node_name_to_id: HashMap<String, Id>,

    // --- Mutation overlay (ISO §13 DML) ---
    /// All mutations applied since load: inserted nodes/edges (IDs dense
    /// above the base counts), tombstones for deletions, and per-record
    /// property / label modifications. Reads merge base + overlay exactly
    /// like `LazyGraphStore`, so both backends share the same DML
    /// semantics. The base Vecs above stay immutable until `save()`
    /// compacts the merged view.
    overlay: RefCell<MutationOverlay>,
}

impl MemoryGraphStore {
    pub fn node_count(&self) -> usize {
        self.node_names.len()
    }
    pub fn edge_count(&self) -> usize {
        self.edge_names.len()
    }

    /// Load a graph from a JSON file path.
    pub fn from_file(path: &Path) -> Result<Self, GraphError> {
        let content = fs::read_to_string(path).map_err(|e| GraphError::Io(e.to_string()))?;
        Self::from_json_str(&content)
    }

    /// Load a graph from a JSON string.
    pub fn from_json_str(json_str: &str) -> Result<Self, GraphError> {
        let json: serde_json::Value =
            serde_json::from_str(json_str).map_err(|e| GraphError::Parse(e.to_string()))?;
        Self::from_json_value(&json)
    }

    /// Load from a parsed serde_json::Value.
    pub fn from_json_value(json: &serde_json::Value) -> Result<Self, GraphError> {
        let json_nodes = json["nodes"]
            .as_array()
            .ok_or_else(|| GraphError::Parse("missing 'nodes' array".into()))?;
        let json_edges = json["edges"]
            .as_array()
            .ok_or_else(|| GraphError::Parse("missing 'edges' array".into()))?;

        let mut node_names = Vec::new();
        let mut node_labels_vec = Vec::new();
        let mut node_props_vec = Vec::new();
        let mut node_name_to_id: HashMap<String, Id> = HashMap::new();
        let mut label_to_nodes: HashMap<String, Vec<Id>> = HashMap::new();

        // Parse nodes
        for n in json_nodes {
            let name = n["id"]
                .as_str()
                .ok_or_else(|| GraphError::Parse("node missing 'id'".into()))?
                .to_string();

            let nid = node_names.len() as Id;
            node_name_to_id.insert(name.clone(), nid);

            let labs: Vec<String> = n["labels"]
                .as_array()
                .ok_or_else(|| GraphError::Parse(format!("node {name} missing 'labels'")))?
                .iter()
                .map(|l| l.as_str().unwrap().to_string())
                .collect();

            for l in &labs {
                label_to_nodes.entry(l.clone()).or_default().push(nid);
            }

            node_labels_vec.push(LabelType::from_list(&labs));
            node_props_vec.push(Self::parse_props(&n["props"])?);
            node_names.push(name);
        }

        let num_nodes = node_names.len();
        let mut outgoing = vec![Vec::new(); num_nodes];
        let mut incoming = vec![Vec::new(); num_nodes];
        let mut undirected_adj = vec![Vec::new(); num_nodes];

        let mut edge_names = Vec::new();
        let mut edge_labels_vec = Vec::new();
        let mut edge_props_vec = Vec::new();
        let mut edge_src = Vec::new();
        let mut edge_tgt = Vec::new();
        let mut edge_directed = Vec::new();
        let mut label_to_edges_d: HashMap<String, Vec<Id>> = HashMap::new();
        let mut label_to_edges_u: HashMap<String, Vec<Id>> = HashMap::new();

        for e in json_edges {
            let name = e["id"]
                .as_str()
                .ok_or_else(|| GraphError::Parse("edge missing 'id'".into()))?
                .to_string();

            let eid = edge_names.len() as Id;

            let labs: Vec<String> = e["labels"]
                .as_array()
                .ok_or_else(|| GraphError::Parse(format!("edge {name} missing 'labels'")))?
                .iter()
                .map(|l| l.as_str().unwrap().to_string())
                .collect();

            let eps = e["endpoints"]
                .as_array()
                .ok_or_else(|| GraphError::Parse(format!("edge {name} missing 'endpoints'")))?;
            let ep0_name = eps[0].as_str().unwrap();
            let ep1_name = eps[1].as_str().unwrap();
            let ep0 = node_name_to_id[ep0_name];
            let ep1 = node_name_to_id[ep1_name];

            let dir = e["directionality"].as_str().ok_or_else(|| {
                GraphError::Parse(format!("edge {name} missing 'directionality'"))
            })?;

            let is_dir = match dir {
                "->" => {
                    for l in &labs {
                        label_to_edges_d.entry(l.clone()).or_default().push(eid);
                    }
                    outgoing[ep0 as usize].push(eid);
                    incoming[ep1 as usize].push(eid);
                    true
                }
                "~~" => {
                    for l in &labs {
                        label_to_edges_u.entry(l.clone()).or_default().push(eid);
                    }
                    undirected_adj[ep0 as usize].push(eid);
                    undirected_adj[ep1 as usize].push(eid);
                    false
                }
                other => {
                    return Err(GraphError::Parse(format!(
                        "unknown directionality '{other}'"
                    )))
                }
            };

            edge_labels_vec.push(LabelType::from_list(&labs));
            edge_props_vec.push(Self::parse_props(&e["props"])?);
            edge_src.push(ep0);
            edge_tgt.push(ep1);
            edge_directed.push(is_dir);
            edge_names.push(name);
        }

        let overlay = RefCell::new(MutationOverlay::new(
            node_names.len() as u32,
            edge_names.len() as u32,
        ));
        Ok(MemoryGraphStore {
            node_names,
            edge_names,
            edge_src,
            edge_tgt,
            edge_directed,
            node_labels: node_labels_vec,
            edge_labels: edge_labels_vec,
            node_props: node_props_vec,
            edge_props: edge_props_vec,
            label_to_nodes,
            label_to_edges_d,
            label_to_edges_u,
            outgoing,
            incoming,
            undirected_adj,
            node_name_to_id,
            overlay,
        })
    }

    /// Build a MemoryGraphStore from pre-parsed components (used by store::io::load_graph).
    // 9 parallel Vecs — one per columnar field. Bundling into a struct is
    // a refactor for another day.
    #[allow(clippy::too_many_arguments)]
    pub fn from_raw(
        node_names: Vec<String>,
        node_labels: Vec<LabelType>,
        node_props: Vec<Props>,
        edge_names: Vec<String>,
        edge_labels: Vec<LabelType>,
        edge_props: Vec<Props>,
        edge_src: Vec<Id>,
        edge_tgt: Vec<Id>,
        edge_directed: Vec<bool>,
    ) -> Self {
        let num_nodes = node_names.len();
        let mut node_name_to_id: HashMap<String, Id> = HashMap::new();
        for (i, name) in node_names.iter().enumerate() {
            node_name_to_id.insert(name.clone(), i as Id);
        }

        let mut label_to_nodes: HashMap<String, Vec<Id>> = HashMap::new();
        let mut label_to_edges_d: HashMap<String, Vec<Id>> = HashMap::new();
        let mut label_to_edges_u: HashMap<String, Vec<Id>> = HashMap::new();
        let mut outgoing = vec![Vec::new(); num_nodes];
        let mut incoming = vec![Vec::new(); num_nodes];
        let mut undirected_adj = vec![Vec::new(); num_nodes];

        for (nid, lt) in node_labels.iter().enumerate() {
            for l in Self::label_strings(lt) {
                label_to_nodes.entry(l).or_default().push(nid as Id);
            }
        }
        for (eid, _) in edge_names.iter().enumerate() {
            let eid = eid as Id;
            if edge_directed[eid as usize] {
                for l in Self::label_strings(&edge_labels[eid as usize]) {
                    label_to_edges_d.entry(l).or_default().push(eid);
                }
                outgoing[edge_src[eid as usize] as usize].push(eid);
                incoming[edge_tgt[eid as usize] as usize].push(eid);
            } else {
                for l in Self::label_strings(&edge_labels[eid as usize]) {
                    label_to_edges_u.entry(l).or_default().push(eid);
                }
                undirected_adj[edge_src[eid as usize] as usize].push(eid);
                undirected_adj[edge_tgt[eid as usize] as usize].push(eid);
            }
        }

        let overlay = RefCell::new(MutationOverlay::new(
            node_names.len() as u32,
            edge_names.len() as u32,
        ));
        MemoryGraphStore {
            node_names,
            edge_names,
            edge_src,
            edge_tgt,
            edge_directed,
            node_labels,
            edge_labels,
            node_props,
            edge_props,
            label_to_nodes,
            label_to_edges_d,
            label_to_edges_u,
            outgoing,
            incoming,
            undirected_adj,
            node_name_to_id,
            overlay,
        }
    }

    /// Save this graph to a .gql database file.
    pub fn save(&self, path: &Path) -> Result<(), GraphError> {
        crate::store::io::save_graph(self, path).map_err(|e| GraphError::Io(e.to_string()))
    }

    /// Open a graph from a .gql database file (loads everything into memory).
    pub fn open(path: &Path) -> Result<Self, GraphError> {
        crate::store::io::load_graph(path).map_err(|e| GraphError::Io(e.to_string()))
    }

    /// Extract label strings from a LabelType (for index building).
    pub fn label_strings(lt: &LabelType) -> Vec<String> {
        match lt {
            LabelType::Label(s) => vec![s.clone()],
            LabelType::And(a, b) => {
                let mut v = Self::label_strings(a);
                v.extend(Self::label_strings(b));
                v
            }
            _ => vec![],
        }
    }

    fn parse_props(obj: &serde_json::Value) -> Result<Props, GraphError> {
        let map = match obj.as_object() {
            Some(m) => m,
            None => return Ok(HashMap::new()),
        };
        let mut props = HashMap::new();
        for (k, v) in map {
            let val = Self::json_to_value(v)
                .map_err(|e| GraphError::Parse(format!("property '{k}': {e}")))?;
            props.insert(k.clone(), val);
        }
        Ok(props)
    }

    fn json_to_value(v: &serde_json::Value) -> Result<Value, String> {
        match v {
            serde_json::Value::String(s) => Ok(Value::Str(s.clone())),
            serde_json::Value::Bool(b) => Ok(Value::Bool(*b)),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Value::Int(i))
                } else if let Some(x) = n.as_f64() {
                    Ok(Value::Float(x))
                } else {
                    Err("number is not representable".into())
                }
            }
            serde_json::Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    out.push(Self::json_to_value(it)?);
                }
                Ok(Value::List(out))
            }
            // `Value::Null` is a first-class variant; accepting it on import
            // makes the JSON dump round-trip losslessly (the on-disk `.gdb`
            // format already carries null via tag 6).
            serde_json::Value::Null => Ok(Value::Null),
            serde_json::Value::Object(map) => {
                let mut fields = std::collections::BTreeMap::new();
                for (k, v) in map {
                    fields.insert(k.clone(), Self::json_to_value(v)?);
                }
                Ok(Value::Record(fields))
            }
        }
    }

    /// Lookup node internal ID by user-facing name.
    pub fn node_id_by_name(&self, name: &str) -> Option<Id> {
        self.node_name_to_id.get(name).copied()
    }

    /// Serialise the live merged view (base + overlay) to a `serde_json`
    /// value in the same shape `from_json_value` consumes. No filesystem
    /// access, so it works under `wasm32`. Round-trips losslessly:
    /// `from_json_value(&g.to_json_value())` reproduces `g`'s shape
    /// (internal ids may renumber; user-facing names are preserved).
    pub fn to_json_value(&self) -> serde_json::Value {
        crate::store::dump::dump_to_json_value(self)
    }

    /// `to_json_value` rendered as a compact JSON string — the unit a
    /// browser binding hands to IndexedDB for persistence.
    pub fn to_json_string(&self) -> String {
        self.to_json_value().to_string()
    }
}

impl super::graph_access::GraphAccess for MemoryGraphStore {
    fn nodes(&self) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = (0..overlay.base_node_count)
            .filter(|id| !overlay.is_node_deleted(*id))
            .collect();
        for offset in 0..overlay.new_nodes.len() as u32 {
            let id = overlay.base_node_count + offset;
            if !overlay.is_node_deleted(id) {
                out.push(id);
            }
        }
        out
    }
    fn edges_directed(&self) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = (0..overlay.base_edge_count)
            .filter(|i| !overlay.is_edge_deleted(*i) && self.edge_directed[*i as usize])
            .collect();
        for (offset, e) in overlay.new_edges.iter().enumerate() {
            let id = overlay.base_edge_count + offset as u32;
            if !overlay.is_edge_deleted(id) && e.directed {
                out.push(id);
            }
        }
        out
    }
    fn edges_undirected(&self) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = (0..overlay.base_edge_count)
            .filter(|i| !overlay.is_edge_deleted(*i) && !self.edge_directed[*i as usize])
            .collect();
        for (offset, e) in overlay.new_edges.iter().enumerate() {
            let id = overlay.base_edge_count + offset as u32;
            if !overlay.is_edge_deleted(id) && !e.directed {
                out.push(id);
            }
        }
        out
    }
    fn node_labels(&self, id: Id) -> LabelType {
        let overlay = self.overlay.borrow();
        if let Some(n) = overlay.get_new_node(id) {
            return n.labels.clone();
        }
        let mut labels = Self::label_strings(&self.node_labels[id as usize]);
        apply_label_mods(&mut labels, overlay.mod_node_labels.get(&id));
        labels_from_strings(labels)
    }
    fn edge_labels(&self, id: Id) -> LabelType {
        let overlay = self.overlay.borrow();
        if let Some(e) = overlay.get_new_edge(id) {
            return e.labels.clone();
        }
        let mut labels = Self::label_strings(&self.edge_labels[id as usize]);
        apply_label_mods(&mut labels, overlay.mod_edge_labels.get(&id));
        labels_from_strings(labels)
    }
    fn node_props(&self, id: Id) -> Props {
        let overlay = self.overlay.borrow();
        if let Some(n) = overlay.get_new_node(id) {
            return n.props.clone();
        }
        let mut base = self.node_props[id as usize].clone();
        apply_prop_mods(&mut base, overlay.mod_node_props.get(&id));
        base
    }
    fn edge_props(&self, id: Id) -> Props {
        let overlay = self.overlay.borrow();
        if let Some(e) = overlay.get_new_edge(id) {
            return e.props.clone();
        }
        let mut base = self.edge_props[id as usize].clone();
        apply_prop_mods(&mut base, overlay.mod_edge_props.get(&id));
        base
    }
    fn src(&self, edge_id: Id) -> Id {
        if let Some(e) = self.overlay.borrow().get_new_edge(edge_id) {
            return e.src;
        }
        self.edge_src[edge_id as usize]
    }
    fn tgt(&self, edge_id: Id) -> Id {
        if let Some(e) = self.overlay.borrow().get_new_edge(edge_id) {
            return e.tgt;
        }
        self.edge_tgt[edge_id as usize]
    }
    fn is_directed(&self, edge_id: Id) -> bool {
        if let Some(e) = self.overlay.borrow().get_new_edge(edge_id) {
            return e.directed;
        }
        self.edge_directed[edge_id as usize]
    }
    fn edge_path_value(&self, edge_id: Id) -> PathValue {
        if self.is_directed(edge_id) {
            PathValue::EdgeDirectional(edge_id)
        } else {
            PathValue::EdgeUndirectional(edge_id)
        }
    }
    fn node_name(&self, id: Id) -> &str {
        // Overlay-allocated nodes have no entry in the base name vec; hand
        // back a synthetic display name (leaked, like LazyGraphStore — only
        // hit on the display path, never the hot loop).
        if id >= self.overlay.borrow().base_node_count {
            return Box::leak(Box::new(format!("auto-n-{id}")));
        }
        &self.node_names[id as usize]
    }
    fn edge_name(&self, id: Id) -> &str {
        if id >= self.overlay.borrow().base_edge_count {
            return Box::leak(Box::new(format!("auto-e-{id}")));
        }
        &self.edge_names[id as usize]
    }
    fn nodes_with_label(&self, label: &str) -> Option<Vec<Id>> {
        let overlay = self.overlay.borrow();
        let base = self.label_to_nodes.get(label);
        let overlay_dirty = !overlay.new_nodes.is_empty()
            || !overlay.deleted_nodes.is_empty()
            || !overlay.mod_node_labels.is_empty();
        if !overlay_dirty {
            return base.cloned();
        }
        let mut out: Vec<Id> = base
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|id| !overlay.is_node_deleted(*id))
                    .filter(|id| {
                        overlay
                            .mod_node_labels
                            .get(id)
                            .map(|m| !m.removed.contains(label))
                            .unwrap_or(true)
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (id, mods) in &overlay.mod_node_labels {
            if mods.added.contains(label) && !overlay.is_node_deleted(*id) && !out.contains(id) {
                out.push(*id);
            }
        }
        for (offset, n) in overlay.new_nodes.iter().enumerate() {
            let id = overlay.base_node_count + offset as u32;
            if overlay.is_node_deleted(id) {
                continue;
            }
            if Self::label_strings(&n.labels).iter().any(|l| l == label) {
                out.push(id);
            }
        }
        if base.is_some() || !out.is_empty() {
            Some(out)
        } else {
            None
        }
    }
    fn directed_edges_with_label(&self, label: &str) -> Option<Vec<Id>> {
        self.edges_with_label_dir(label, true)
    }
    fn undirected_edges_with_label(&self, label: &str) -> Option<Vec<Id>> {
        self.edges_with_label_dir(label, false)
    }
    fn outgoing_edges(&self, node_id: Id) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = base_adj(&self.outgoing, node_id)
            .iter()
            .copied()
            .filter(|id| !overlay.is_edge_deleted(*id))
            .collect();
        if let Some(extra) = overlay.new_outgoing.get(&node_id) {
            for &eid in extra {
                if !overlay.is_edge_deleted(eid) {
                    out.push(eid);
                }
            }
        }
        out
    }
    fn incoming_edges(&self, node_id: Id) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = base_adj(&self.incoming, node_id)
            .iter()
            .copied()
            .filter(|id| !overlay.is_edge_deleted(*id))
            .collect();
        if let Some(extra) = overlay.new_incoming.get(&node_id) {
            for &eid in extra {
                if !overlay.is_edge_deleted(eid) {
                    out.push(eid);
                }
            }
        }
        out
    }
    fn undirected_edges_of(&self, node_id: Id) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = base_adj(&self.undirected_adj, node_id)
            .iter()
            .copied()
            .filter(|id| !overlay.is_edge_deleted(*id))
            .collect();
        if let Some(extra) = overlay.new_undirected.get(&node_id) {
            for &eid in extra {
                if !overlay.is_edge_deleted(eid) {
                    out.push(eid);
                }
            }
        }
        out
    }
}

impl MemoryGraphStore {
    /// Shared body for `directed_edges_with_label` / `undirected_edges_with_label`.
    /// The base label maps are already split by direction, so `want_directed`
    /// only filters the overlay contributions.
    fn edges_with_label_dir(&self, label: &str, want_directed: bool) -> Option<Vec<Id>> {
        let overlay = self.overlay.borrow();
        let base = if want_directed {
            self.label_to_edges_d.get(label)
        } else {
            self.label_to_edges_u.get(label)
        };
        let overlay_dirty = !overlay.new_edges.is_empty()
            || !overlay.deleted_edges.is_empty()
            || !overlay.mod_edge_labels.is_empty();
        if !overlay_dirty {
            return base.cloned();
        }
        let mut out: Vec<Id> = base
            .map(|ids| {
                ids.iter()
                    .copied()
                    .filter(|iid| !overlay.is_edge_deleted(*iid))
                    .filter(|iid| {
                        overlay
                            .mod_edge_labels
                            .get(iid)
                            .map(|m| !m.removed.contains(label))
                            .unwrap_or(true)
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (id, mods) in &overlay.mod_edge_labels {
            if mods.added.contains(label)
                && (*id as usize) < self.edge_directed.len()
                && self.edge_directed[*id as usize] == want_directed
                && !overlay.is_edge_deleted(*id)
                && !out.contains(id)
            {
                out.push(*id);
            }
        }
        for (offset, e) in overlay.new_edges.iter().enumerate() {
            let id = overlay.base_edge_count + offset as u32;
            if overlay.is_edge_deleted(id) || e.directed != want_directed {
                continue;
            }
            if Self::label_strings(&e.labels).iter().any(|l| l == label) {
                out.push(id);
            }
        }
        if base.is_some() || !out.is_empty() {
            Some(out)
        } else {
            None
        }
    }
}

impl super::graph_access::GraphAccessMut for MemoryGraphStore {
    fn insert_node(&self, labels: LabelType, props: Props) -> Id {
        self.overlay.borrow_mut().insert_node(labels, props)
    }

    fn insert_edge(&self, src: Id, tgt: Id, directed: bool, labels: LabelType, props: Props) -> Id {
        // Endpoint validation lives in the runtime so the caller can raise
        // the right ISO error code; here we only record the mutation.
        self.overlay
            .borrow_mut()
            .insert_edge(src, tgt, directed, labels, props)
    }

    fn delete_edge(&self, id: Id) {
        self.overlay.borrow_mut().delete_edge(id);
    }

    fn detach_delete_node(&self, id: Id) {
        // Snapshot incident edges from the merged view before mutating —
        // borrowing the overlay mutably below would otherwise alias the
        // reads inside outgoing_edges/etc.
        let incident: Vec<Id> = self
            .outgoing_edges(id)
            .into_iter()
            .chain(self.incoming_edges(id))
            .chain(self.undirected_edges_of(id))
            .collect();
        let mut overlay = self.overlay.borrow_mut();
        for eid in incident {
            overlay.delete_edge(eid);
        }
        overlay.delete_node(id);
    }

    fn delete_node_no_detach(&self, id: Id) -> Result<(), super::graph_access::G1001> {
        let remaining: Vec<Id> = self
            .outgoing_edges(id)
            .into_iter()
            .chain(self.incoming_edges(id))
            .chain(self.undirected_edges_of(id))
            .collect();
        if !remaining.is_empty() {
            return Err(super::graph_access::G1001 {
                node: id,
                remaining_edges: remaining,
            });
        }
        self.overlay.borrow_mut().delete_node(id);
        Ok(())
    }

    fn is_node_alive(&self, id: Id) -> bool {
        let overlay = self.overlay.borrow();
        if overlay.is_node_deleted(id) {
            return false;
        }
        if id < overlay.base_node_count {
            return true;
        }
        overlay.get_new_node(id).is_some()
    }

    fn is_edge_alive(&self, id: Id) -> bool {
        let overlay = self.overlay.borrow();
        if overlay.is_edge_deleted(id) {
            return false;
        }
        if id < overlay.base_edge_count {
            return true;
        }
        overlay.get_new_edge(id).is_some()
    }

    fn rollback_session(&self) {
        let mut overlay = self.overlay.borrow_mut();
        let bn = overlay.base_node_count;
        let be = overlay.base_edge_count;
        overlay.clear(bn, be);
    }

    fn set_node_prop(&self, id: Id, prop: &str, value: super::value::Value) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_node_count {
            let off = (id - overlay.base_node_count) as usize;
            if let Some(n) = overlay.new_nodes.get_mut(off) {
                n.props.insert(prop.to_string(), value);
            }
            return;
        }
        let entry = overlay.mod_node_props.entry(id).or_default();
        entry.set.insert(prop.to_string(), Some(value));
    }

    fn set_edge_prop(&self, id: Id, prop: &str, value: super::value::Value) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_edge_count {
            let off = (id - overlay.base_edge_count) as usize;
            if let Some(e) = overlay.new_edges.get_mut(off) {
                e.props.insert(prop.to_string(), value);
            }
            return;
        }
        let entry = overlay.mod_edge_props.entry(id).or_default();
        entry.set.insert(prop.to_string(), Some(value));
    }

    fn replace_node_props(&self, id: Id, props: Props) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_node_count {
            let off = (id - overlay.base_node_count) as usize;
            if let Some(n) = overlay.new_nodes.get_mut(off) {
                n.props = props;
            }
            return;
        }
        // ISO §13.3 GR8 b.i: clear existing props, then apply the new map.
        let mut entry = crate::store::overlay::PropMods {
            cleared: true,
            set: HashMap::new(),
        };
        for (k, v) in props {
            entry.set.insert(k, Some(v));
        }
        overlay.mod_node_props.insert(id, entry);
    }

    fn replace_edge_props(&self, id: Id, props: Props) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_edge_count {
            let off = (id - overlay.base_edge_count) as usize;
            if let Some(e) = overlay.new_edges.get_mut(off) {
                e.props = props;
            }
            return;
        }
        let mut entry = crate::store::overlay::PropMods {
            cleared: true,
            set: HashMap::new(),
        };
        for (k, v) in props {
            entry.set.insert(k, Some(v));
        }
        overlay.mod_edge_props.insert(id, entry);
    }

    fn remove_node_prop(&self, id: Id, prop: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_node_count {
            let off = (id - overlay.base_node_count) as usize;
            if let Some(n) = overlay.new_nodes.get_mut(off) {
                n.props.remove(prop);
            }
            return;
        }
        let entry = overlay.mod_node_props.entry(id).or_default();
        entry.set.insert(prop.to_string(), None);
    }

    fn remove_edge_prop(&self, id: Id, prop: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_edge_count {
            let off = (id - overlay.base_edge_count) as usize;
            if let Some(e) = overlay.new_edges.get_mut(off) {
                e.props.remove(prop);
            }
            return;
        }
        let entry = overlay.mod_edge_props.entry(id).or_default();
        entry.set.insert(prop.to_string(), None);
    }

    fn add_node_label(&self, id: Id, label: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_node_count {
            let off = (id - overlay.base_node_count) as usize;
            if let Some(n) = overlay.new_nodes.get_mut(off) {
                n.labels = label_type_with_added(&n.labels, label);
            }
            return;
        }
        let entry = overlay.mod_node_labels.entry(id).or_default();
        entry.removed.remove(label);
        entry.added.insert(label.to_string());
    }

    fn add_edge_label(&self, id: Id, label: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_edge_count {
            let off = (id - overlay.base_edge_count) as usize;
            if let Some(e) = overlay.new_edges.get_mut(off) {
                e.labels = label_type_with_added(&e.labels, label);
            }
            return;
        }
        let entry = overlay.mod_edge_labels.entry(id).or_default();
        entry.removed.remove(label);
        entry.added.insert(label.to_string());
    }

    fn remove_node_label(&self, id: Id, label: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_node_count {
            let off = (id - overlay.base_node_count) as usize;
            if let Some(n) = overlay.new_nodes.get_mut(off) {
                n.labels = label_type_with_removed(&n.labels, label);
            }
            return;
        }
        let entry = overlay.mod_node_labels.entry(id).or_default();
        entry.added.remove(label);
        entry.removed.insert(label.to_string());
    }

    fn remove_edge_label(&self, id: Id, label: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_edge_count {
            let off = (id - overlay.base_edge_count) as usize;
            if let Some(e) = overlay.new_edges.get_mut(off) {
                e.labels = label_type_with_removed(&e.labels, label);
            }
            return;
        }
        let entry = overlay.mod_edge_labels.entry(id).or_default();
        entry.added.remove(label);
        entry.removed.insert(label.to_string());
    }
}

/// Apply a `PropMods` (if any) to a base property map in place: honor a
/// prior `SET x = {...}` clear, then overwrite / remove per-prop.
fn apply_prop_mods(base: &mut Props, mods: Option<&crate::store::overlay::PropMods>) {
    let Some(mods) = mods else { return };
    if mods.cleared {
        base.clear();
    }
    for (name, op) in &mods.set {
        match op {
            Some(v) => {
                base.insert(name.clone(), v.clone());
            }
            None => {
                base.remove(name);
            }
        }
    }
}

/// Base adjacency slice for `node_id`, or an empty slice when the id is
/// overlay-allocated (its base adjacency lists never existed).
fn base_adj(adj: &[Vec<Id>], node_id: Id) -> &[Id] {
    adj.get(node_id as usize)
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("parse error: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fraud_graph() -> MemoryGraphStore {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
        MemoryGraphStore::from_file(&path).unwrap()
    }

    fn social_graph() -> MemoryGraphStore {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/social-network.json");
        MemoryGraphStore::from_file(&path).unwrap()
    }

    #[test]
    fn test_fraud_node_count() {
        let g = fraud_graph();
        assert_eq!(g.node_count(), 5);
    }

    #[test]
    fn test_fraud_edge_counts() {
        let g = fraud_graph();
        assert_eq!(g.edge_count(), 5);
    }

    #[test]
    fn test_social_node_count() {
        let g = social_graph();
        assert_eq!(g.node_count(), 3);
    }

    #[test]
    fn test_social_edge_counts() {
        let g = social_graph();
        assert_eq!(g.edge_count(), 3);
    }
}
