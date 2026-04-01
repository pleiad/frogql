use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json;

use crate::model::value::{PathValue, Value};
use crate::typing::label_type::LabelType;

/// Property map: attribute name → Value.
pub type Props = HashMap<String, Value>;

/// A property graph loaded from JSON.
///
/// Mirrors the Python `Graph` class: separate lists for directed/undirected edges,
/// and lookup maps for labels, properties, and endpoints.
pub struct Graph {
    /// All node IDs.
    pub nodes: Vec<String>,
    /// Directed edge IDs.
    pub edges_d: Vec<String>,
    /// Undirected edge IDs.
    pub edges_u: Vec<String>,
    /// Labels for both nodes and edges (id → LabelType).
    pub labels: HashMap<String, LabelType>,
    /// Properties for both nodes and edges (id → Props).
    pub props: HashMap<String, Props>,
    /// Edge endpoints (edge_id → (endpoint0, endpoint1)).
    pub endpoints: HashMap<String, (String, String)>,
    /// Source node of directed edges (edge_id → node_id).
    pub src: HashMap<String, String>,
    /// Target node of directed edges (edge_id → node_id).
    pub tgt: HashMap<String, String>,

    // --- Indexes (built at load time) ---
    /// label → node IDs with that label.
    label_to_nodes: HashMap<String, Vec<String>>,
    /// label → directed edge IDs with that label.
    label_to_edges_d: HashMap<String, Vec<String>>,
    /// label → undirected edge IDs with that label.
    label_to_edges_u: HashMap<String, Vec<String>>,
    /// node_id → outgoing directed edge IDs.
    outgoing: HashMap<String, Vec<String>>,
    /// node_id → incoming directed edge IDs.
    incoming: HashMap<String, Vec<String>>,
    /// node_id → undirected edge IDs.
    undirected_adj: HashMap<String, Vec<String>>,
}

impl Graph {
    /// Load a graph from a JSON file path.
    pub fn from_file(path: &Path) -> Result<Self, GraphError> {
        let content = fs::read_to_string(path)
            .map_err(|e| GraphError::Io(e.to_string()))?;
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

        let mut nodes = Vec::new();
        let mut labels = HashMap::new();
        let mut props = HashMap::new();
        let mut label_to_nodes: HashMap<String, Vec<String>> = HashMap::new();
        let mut label_to_edges_d: HashMap<String, Vec<String>> = HashMap::new();
        let mut label_to_edges_u: HashMap<String, Vec<String>> = HashMap::new();
        let mut outgoing: HashMap<String, Vec<String>> = HashMap::new();
        let mut incoming: HashMap<String, Vec<String>> = HashMap::new();
        let mut undirected_adj: HashMap<String, Vec<String>> = HashMap::new();

        // Parse nodes
        for n in json_nodes {
            let id = n["id"]
                .as_str()
                .ok_or_else(|| GraphError::Parse("node missing 'id'".into()))?
                .to_string();

            let node_labels: Vec<String> = n["labels"]
                .as_array()
                .ok_or_else(|| GraphError::Parse(format!("node {id} missing 'labels'")))?
                .iter()
                .map(|l| l.as_str().unwrap().to_string())
                .collect();

            for l in &node_labels {
                label_to_nodes.entry(l.clone()).or_default().push(id.clone());
            }

            labels.insert(id.clone(), LabelType::from_list(&node_labels));
            props.insert(id.clone(), Self::parse_props(&n["props"])?);
            nodes.push(id);
        }

        // Parse edges
        let mut edges_d = Vec::new();
        let mut edges_u = Vec::new();
        let mut endpoints_map = HashMap::new();
        let mut src_map = HashMap::new();
        let mut tgt_map = HashMap::new();

        for e in json_edges {
            let id = e["id"]
                .as_str()
                .ok_or_else(|| GraphError::Parse("edge missing 'id'".into()))?
                .to_string();

            let edge_labels: Vec<String> = e["labels"]
                .as_array()
                .ok_or_else(|| GraphError::Parse(format!("edge {id} missing 'labels'")))?
                .iter()
                .map(|l| l.as_str().unwrap().to_string())
                .collect();

            let eps = e["endpoints"]
                .as_array()
                .ok_or_else(|| GraphError::Parse(format!("edge {id} missing 'endpoints'")))?;
            let ep0 = eps[0].as_str().unwrap().to_string();
            let ep1 = eps[1].as_str().unwrap().to_string();

            let dir = e["directionality"]
                .as_str()
                .ok_or_else(|| GraphError::Parse(format!("edge {id} missing 'directionality'")))?;

            labels.insert(id.clone(), LabelType::from_list(&edge_labels));
            props.insert(id.clone(), Self::parse_props(&e["props"])?);
            endpoints_map.insert(id.clone(), (ep0.clone(), ep1.clone()));

            match dir {
                "->" => {
                    for l in &edge_labels {
                        label_to_edges_d.entry(l.clone()).or_default().push(id.clone());
                    }
                    outgoing.entry(ep0.clone()).or_default().push(id.clone());
                    incoming.entry(ep1.clone()).or_default().push(id.clone());
                    src_map.insert(id.clone(), ep0);
                    tgt_map.insert(id.clone(), ep1);
                    edges_d.push(id);
                }
                "~~" => {
                    for l in &edge_labels {
                        label_to_edges_u.entry(l.clone()).or_default().push(id.clone());
                    }
                    undirected_adj.entry(ep0.clone()).or_default().push(id.clone());
                    undirected_adj.entry(ep1.clone()).or_default().push(id.clone());
                    edges_u.push(id);
                }
                other => {
                    return Err(GraphError::Parse(format!(
                        "unknown directionality '{other}'"
                    )));
                }
            }
        }

        Ok(Graph {
            nodes,
            edges_d,
            edges_u,
            labels,
            props,
            endpoints: endpoints_map,
            src: src_map,
            tgt: tgt_map,
            label_to_nodes,
            label_to_edges_d,
            label_to_edges_u,
            outgoing,
            incoming,
            undirected_adj,
        })
    }

    fn parse_props(obj: &serde_json::Value) -> Result<Props, GraphError> {
        let map = match obj.as_object() {
            Some(m) => m,
            None => return Ok(HashMap::new()),
        };
        let mut props = HashMap::new();
        for (k, v) in map {
            let val = match v {
                serde_json::Value::String(s) => Value::Str(s.clone()),
                serde_json::Value::Bool(b) => Value::Bool(*b),
                serde_json::Value::Number(n) => {
                    Value::Int(n.as_i64().ok_or_else(|| {
                        GraphError::Parse(format!("property '{k}' is not an integer"))
                    })?)
                }
                _ => {
                    return Err(GraphError::Parse(format!(
                        "unsupported property value type for '{k}'"
                    )));
                }
            };
            props.insert(k.clone(), val);
        }
        Ok(props)
    }

    /// Get the PathValue for a node ID.
    pub fn node_value(&self, id: &str) -> PathValue {
        PathValue::Node(id.to_string())
    }

    /// Get the PathValue for an edge ID (directed or undirected).
    pub fn edge_value(&self, id: &str) -> PathValue {
        if self.src.contains_key(id) {
            PathValue::EdgeDirectional(id.to_string())
        } else {
            PathValue::EdgeUndirectional(id.to_string())
        }
    }
}

impl super::graph_access::GraphAccess for Graph {
    fn nodes(&self) -> Vec<String> {
        self.nodes.clone()
    }
    fn edges_directed(&self) -> Vec<String> {
        self.edges_d.clone()
    }
    fn edges_undirected(&self) -> Vec<String> {
        self.edges_u.clone()
    }
    fn labels(&self, id: &str) -> &LabelType {
        &self.labels[id]
    }
    fn props(&self, id: &str) -> &Props {
        &self.props[id]
    }
    fn src(&self, edge_id: &str) -> &str {
        &self.src[edge_id]
    }
    fn tgt(&self, edge_id: &str) -> &str {
        &self.tgt[edge_id]
    }
    fn endpoints(&self, edge_id: &str) -> (&str, &str) {
        let (a, b) = &self.endpoints[edge_id];
        (a, b)
    }
    fn is_directed(&self, edge_id: &str) -> bool {
        self.src.contains_key(edge_id)
    }
    fn edge_path_value(&self, edge_id: &str) -> PathValue {
        self.edge_value(edge_id)
    }
    fn nodes_with_label(&self, label: &str) -> Option<Vec<String>> {
        self.label_to_nodes.get(label).cloned()
    }
    fn directed_edges_with_label(&self, label: &str) -> Option<Vec<String>> {
        self.label_to_edges_d.get(label).cloned()
    }
    fn undirected_edges_with_label(&self, label: &str) -> Option<Vec<String>> {
        self.label_to_edges_u.get(label).cloned()
    }
    fn outgoing_edges(&self, node_id: &str) -> Vec<String> {
        self.outgoing.get(node_id).cloned().unwrap_or_default()
    }
    fn incoming_edges(&self, node_id: &str) -> Vec<String> {
        self.incoming.get(node_id).cloned().unwrap_or_default()
    }
    fn undirected_edges_of(&self, node_id: &str) -> Vec<String> {
        self.undirected_adj.get(node_id).cloned().unwrap_or_default()
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
        assert_eq!(g.nodes.len(), 5);
    }

    #[test]
    fn test_fraud_edge_counts() {
        let g = fraud_graph();
        assert_eq!(g.edges_d.len(), 5);
        assert_eq!(g.edges_u.len(), 0);
    }

    #[test]
    fn test_fraud_labels() {
        let g = fraud_graph();
        assert_eq!(g.labels["a1"], LabelType::Label("Account".into()));
        // d1 has two labels: Dummy & Person
        assert_eq!(
            g.labels["d1"],
            LabelType::And(
                Box::new(LabelType::Label("Dummy".into())),
                Box::new(LabelType::Label("Person".into())),
            )
        );
    }

    #[test]
    fn test_fraud_props() {
        let g = fraud_graph();
        assert_eq!(g.props["a1"]["owner"], Value::Str("Aretha".into()));
        assert_eq!(g.props["a1"]["isBlocked"], Value::Bool(false));
        assert_eq!(g.props["t1"]["amount"], Value::Int(2500000));
    }

    #[test]
    fn test_fraud_endpoints() {
        let g = fraud_graph();
        assert_eq!(g.src["t1"], "p1");
        assert_eq!(g.tgt["t1"], "p2");
        assert_eq!(g.endpoints["t1"], ("p1".into(), "p2".into()));
    }

    #[test]
    fn test_social_node_count() {
        let g = social_graph();
        assert_eq!(g.nodes.len(), 3);
    }

    #[test]
    fn test_social_edge_counts() {
        let g = social_graph();
        assert_eq!(g.edges_d.len(), 2); // e2 (Likes ->), e3 (Author ->)
        assert_eq!(g.edges_u.len(), 1); // e1 (Knows ~~)
    }

    #[test]
    fn test_social_multi_labels() {
        let g = social_graph();
        // n1 has labels: Person & Teacher
        assert_eq!(
            g.labels["n1"],
            LabelType::And(
                Box::new(LabelType::Label("Person".into())),
                Box::new(LabelType::Label("Teacher".into())),
            )
        );
    }

    #[test]
    fn test_social_mixed_prop_types() {
        let g = social_graph();
        // n1.name is string, n2.status is int, n3.status is bool
        assert_eq!(g.props["n1"]["name"], Value::Str("Alice".into()));
        assert_eq!(g.props["n2"]["status"], Value::Int(1));
        assert_eq!(g.props["n3"]["status"], Value::Bool(true));
    }

    #[test]
    fn test_social_undirected_edge() {
        let g = social_graph();
        // e1 is undirected, should not be in src/tgt maps
        assert!(!g.src.contains_key("e1"));
        assert!(!g.tgt.contains_key("e1"));
        assert_eq!(g.endpoints["e1"], ("n1".into(), "n2".into()));
    }

    #[test]
    fn test_edge_value_type() {
        let g = fraud_graph();
        assert!(matches!(g.edge_value("t1"), PathValue::EdgeDirectional(_)));

        let g2 = social_graph();
        assert!(matches!(
            g2.edge_value("e1"),
            PathValue::EdgeUndirectional(_)
        ));
    }
}
