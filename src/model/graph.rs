use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json;

use crate::model::value::{Id, PathValue, Value};
use crate::typing::label_type::LabelType;

/// Property map: attribute name → Value.
pub type Props = HashMap<String, Value>;

/// A property graph with u32 internal IDs.
///
/// String user-facing IDs (from JSON) are mapped to sequential u32 IDs at load time.
/// All internal operations use u32 IDs for performance.
pub struct Graph {
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

    // --- Mutation tombstones (ISO §13 DML) ---
    /// Per-node liveness flag. Defaults to `true` (existing data is alive
    /// at load time). Set to `false` by `delete_node_no_detach` /
    /// `detach_delete_node`. Reads filter dead nodes; they get compacted
    /// out at save() time.
    node_alive: Vec<bool>,
    edge_alive: Vec<bool>,
}

impl Graph {
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

        let node_alive = vec![true; node_names.len()];
        let edge_alive = vec![true; edge_names.len()];
        Ok(Graph {
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
            node_alive,
            edge_alive,
        })
    }

    /// Build a Graph from pre-parsed components (used by store::io::load_graph).
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

        let node_alive = vec![true; node_names.len()];
        let edge_alive = vec![true; edge_names.len()];
        Graph {
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
            node_alive,
            edge_alive,
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
            serde_json::Value::Null => Err("null not supported".into()),
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
}

impl super::graph_access::GraphAccess for Graph {
    fn nodes(&self) -> Vec<Id> {
        (0..self.node_names.len() as Id)
            .filter(|&id| self.node_alive[id as usize])
            .collect()
    }
    fn edges_directed(&self) -> Vec<Id> {
        (0..self.edge_names.len() as Id)
            .filter(|&eid| self.edge_alive[eid as usize] && self.edge_directed[eid as usize])
            .collect()
    }
    fn edges_undirected(&self) -> Vec<Id> {
        (0..self.edge_names.len() as Id)
            .filter(|&eid| self.edge_alive[eid as usize] && !self.edge_directed[eid as usize])
            .collect()
    }
    fn node_labels(&self, id: Id) -> LabelType {
        self.node_labels[id as usize].clone()
    }
    fn edge_labels(&self, id: Id) -> LabelType {
        self.edge_labels[id as usize].clone()
    }
    fn node_props(&self, id: Id) -> Props {
        self.node_props[id as usize].clone()
    }
    fn edge_props(&self, id: Id) -> Props {
        self.edge_props[id as usize].clone()
    }
    fn src(&self, edge_id: Id) -> Id {
        self.edge_src[edge_id as usize]
    }
    fn tgt(&self, edge_id: Id) -> Id {
        self.edge_tgt[edge_id as usize]
    }
    fn is_directed(&self, edge_id: Id) -> bool {
        self.edge_directed[edge_id as usize]
    }
    fn edge_path_value(&self, edge_id: Id) -> PathValue {
        if self.edge_directed[edge_id as usize] {
            PathValue::EdgeDirectional(edge_id)
        } else {
            PathValue::EdgeUndirectional(edge_id)
        }
    }
    fn node_name(&self, id: Id) -> &str {
        &self.node_names[id as usize]
    }
    fn edge_name(&self, id: Id) -> &str {
        &self.edge_names[id as usize]
    }
    fn nodes_with_label(&self, label: &str) -> Option<Vec<Id>> {
        self.label_to_nodes.get(label).map(|v| {
            v.iter()
                .copied()
                .filter(|id| self.node_alive[*id as usize])
                .collect()
        })
    }
    fn directed_edges_with_label(&self, label: &str) -> Option<Vec<Id>> {
        self.label_to_edges_d.get(label).map(|v| {
            v.iter()
                .copied()
                .filter(|id| self.edge_alive[*id as usize])
                .collect()
        })
    }
    fn undirected_edges_with_label(&self, label: &str) -> Option<Vec<Id>> {
        self.label_to_edges_u.get(label).map(|v| {
            v.iter()
                .copied()
                .filter(|id| self.edge_alive[*id as usize])
                .collect()
        })
    }
    fn outgoing_edges(&self, node_id: Id) -> Vec<Id> {
        self.outgoing[node_id as usize]
            .iter()
            .copied()
            .filter(|id| self.edge_alive[*id as usize])
            .collect()
    }
    fn incoming_edges(&self, node_id: Id) -> Vec<Id> {
        self.incoming[node_id as usize]
            .iter()
            .copied()
            .filter(|id| self.edge_alive[*id as usize])
            .collect()
    }
    fn undirected_edges_of(&self, node_id: Id) -> Vec<Id> {
        self.undirected_adj[node_id as usize]
            .iter()
            .copied()
            .filter(|id| self.edge_alive[*id as usize])
            .collect()
    }
}

impl super::graph_access::GraphAccessMut for Graph {
    fn insert_node(&self, _labels: LabelType, _props: Props) -> Id {
        // `Graph` is the in-RAM JSON-backed fixture used by tests; its
        // fields are plain Vecs without RefCell, so true `&self`
        // mutability would require a wider refactor that's not on the
        // MVP-0 critical path. The user-visible motor is `LazyGraphStore`,
        // and that one *does* implement `GraphAccessMut` correctly.
        unimplemented!(
            "Graph::insert_node: in-RAM Graph mutability is not wired in MVP-0 (use LazyGraphStore)"
        );
    }

    fn insert_edge(
        &self,
        _src: Id,
        _tgt: Id,
        _directed: bool,
        _labels: LabelType,
        _props: Props,
    ) -> Id {
        unimplemented!(
            "Graph::insert_edge: in-RAM Graph mutability is not wired in MVP-0 (use LazyGraphStore)"
        );
    }

    fn delete_edge(&self, _id: Id) {
        unimplemented!(
            "Graph::delete_edge: in-RAM Graph mutability is not wired in MVP-0 (use LazyGraphStore)"
        );
    }

    fn detach_delete_node(&self, _id: Id) {
        unimplemented!(
            "Graph::detach_delete_node: in-RAM Graph mutability is not wired in MVP-0 (use LazyGraphStore)"
        );
    }

    fn delete_node_no_detach(&self, _id: Id) -> Result<(), super::graph_access::G1001> {
        unimplemented!(
            "Graph::delete_node_no_detach: in-RAM Graph mutability is not wired in MVP-0 (use LazyGraphStore)"
        );
    }

    fn is_node_alive(&self, id: Id) -> bool {
        self.node_alive.get(id as usize).copied().unwrap_or(false)
    }

    fn is_edge_alive(&self, id: Id) -> bool {
        self.edge_alive.get(id as usize).copied().unwrap_or(false)
    }

    fn rollback_session(&self) {
        // No overlay to rewind; mutability is unimplemented in MVP-0.
    }

    fn set_node_prop(&self, _id: Id, _prop: &str, _value: super::value::Value) {
        unimplemented!("Graph::set_node_prop: use LazyGraphStore for mutability");
    }

    fn set_edge_prop(&self, _id: Id, _prop: &str, _value: super::value::Value) {
        unimplemented!("Graph::set_edge_prop: use LazyGraphStore for mutability");
    }

    fn replace_node_props(&self, _id: Id, _props: Props) {
        unimplemented!("Graph::replace_node_props: use LazyGraphStore for mutability");
    }

    fn replace_edge_props(&self, _id: Id, _props: Props) {
        unimplemented!("Graph::replace_edge_props: use LazyGraphStore for mutability");
    }

    fn remove_node_prop(&self, _id: Id, _prop: &str) {
        unimplemented!("Graph::remove_node_prop: use LazyGraphStore for mutability");
    }

    fn remove_edge_prop(&self, _id: Id, _prop: &str) {
        unimplemented!("Graph::remove_edge_prop: use LazyGraphStore for mutability");
    }

    fn add_node_label(&self, _id: Id, _label: &str) {
        unimplemented!("Graph::add_node_label: use LazyGraphStore for mutability");
    }

    fn add_edge_label(&self, _id: Id, _label: &str) {
        unimplemented!("Graph::add_edge_label: use LazyGraphStore for mutability");
    }

    fn remove_node_label(&self, _id: Id, _label: &str) {
        unimplemented!("Graph::remove_node_label: use LazyGraphStore for mutability");
    }

    fn remove_edge_label(&self, _id: Id, _label: &str) {
        unimplemented!("Graph::remove_edge_label: use LazyGraphStore for mutability");
    }
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

    fn fraud_graph() -> Graph {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
        Graph::from_file(&path).unwrap()
    }

    fn social_graph() -> Graph {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/social-network.json");
        Graph::from_file(&path).unwrap()
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
