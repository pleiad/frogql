//! Data-vs-schema validation for `VALIDATE GRAPH TYPE`.
//!
//! For each node and edge, build an instance `VariableType` reflecting
//! the element's actual labels and property types, then check whether
//! any type in the schema admits the instance via
//! `VariableType::is_subtype(instance, schema_type)`.
//!
//! The design choice is opt-in: nothing in the open / use path runs
//! this. The user invokes it explicitly through the REPL DDL. Cost is
//! O(N + E) over the graph; for very large graphs the report keeps
//! per-element details bounded.

use std::collections::BTreeMap;

use crate::model::graph::MemoryGraphStore;
use crate::model::graph_access::GraphAccess;
use crate::model::value::Value;

use super::descriptor_type::DescriptorType;
use super::label_type::LabelType;
use super::property_type::PropertyType;
use super::simple_type::SimpleType;
use super::variable_type::{Schema, VariableType};

/// Maximum number of per-element violation samples retained. Beyond
/// this we just bump the count.
const MAX_SAMPLES: usize = 16;

/// Outcome of a `VALIDATE GRAPH TYPE` walk.
#[derive(Debug, Clone)]
pub struct ValidationReport {
    pub nodes_checked: u32,
    pub edges_checked: u32,
    pub node_violations: u32,
    pub edge_violations: u32,
    /// First few violation samples (capped at `MAX_SAMPLES` per kind).
    pub samples: Vec<Violation>,
}

#[derive(Debug, Clone)]
pub struct Violation {
    pub kind: ElementKind,
    /// Internal id of the offending element. The REPL/Python layer can
    /// resolve to a user-facing name through the store if needed.
    pub id: u32,
    /// Sorted labels observed on the element.
    pub labels: Vec<String>,
    /// Property types observed on the element.
    pub props: BTreeMap<String, SimpleType>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementKind {
    Node,
    Edge,
}

impl ValidationReport {
    pub fn ok(&self) -> bool {
        self.node_violations == 0 && self.edge_violations == 0
    }

    pub fn total_violations(&self) -> u32 {
        self.node_violations + self.edge_violations
    }
}

/// Walk every node and edge in `g` and check each against `schema`.
pub fn validate_against_data<G: GraphAccess>(g: &G, schema: &Schema) -> ValidationReport {
    let mut report = ValidationReport {
        nodes_checked: 0,
        edges_checked: 0,
        node_violations: 0,
        edge_violations: 0,
        samples: Vec::new(),
    };

    for nid in g.nodes() {
        report.nodes_checked += 1;
        let instance = node_instance_type(g, nid);
        if !instance_admitted(&instance, &schema.nodes) {
            report.node_violations += 1;
            push_sample(
                &mut report.samples,
                Violation {
                    kind: ElementKind::Node,
                    id: nid,
                    labels: sorted_label_strings(&g.node_labels(nid)),
                    props: instance_props_map(&instance),
                },
            );
        }
    }

    for eid in g.edges_directed().into_iter().chain(g.edges_undirected()) {
        report.edges_checked += 1;
        let instance = edge_instance_type(g, eid);
        if !instance_admitted(&instance, &schema.edges) {
            report.edge_violations += 1;
            push_sample(
                &mut report.samples,
                Violation {
                    kind: ElementKind::Edge,
                    id: eid,
                    labels: sorted_label_strings(&g.edge_labels(eid)),
                    props: instance_props_map(&instance),
                },
            );
        }
    }

    report
}

/// Per-element validation used by ISO §13 DML to surface G2000 right
/// after an INSERT. Returns the human-readable failure message when the
/// element does not satisfy any node type in `schema.nodes`.
pub fn validate_node_against_schema(
    labels: &LabelType,
    props: &std::collections::HashMap<String, Value>,
    schema: &Schema,
) -> Result<(), String> {
    let instance = VariableType::Node(element_descriptor(labels, &props_to_simple(props)));
    if instance_admitted(&instance, &schema.nodes) {
        Ok(())
    } else {
        let label_strs = sorted_label_strings(labels).join(",");
        Err(format!(
            "G2000 graph type violation: inserted node with labels {{{label_strs}}} \
             does not match any node type in the active GRAPH TYPE"
        ))
    }
}

/// Edge equivalent of `validate_node_against_schema`. Endpoints are
/// captured live so the schema's `(:Source)-[:Edge]->(:Target)` shape can
/// be checked against the actual neighbours.
pub fn validate_edge_against_schema<G: GraphAccess>(
    g: &G,
    eid: u32,
    schema: &Schema,
) -> Result<(), String> {
    let instance = edge_instance_type(g, eid);
    if instance_admitted(&instance, &schema.edges) {
        Ok(())
    } else {
        let label_strs = sorted_label_strings(&g.edge_labels(eid)).join(",");
        Err(format!(
            "G2000 graph type violation: inserted edge with labels {{{label_strs}}} \
             does not match any edge type in the active GRAPH TYPE"
        ))
    }
}

fn push_sample(samples: &mut Vec<Violation>, v: Violation) {
    let same_kind: usize = samples.iter().filter(|s| s.kind == v.kind).count();
    if same_kind < MAX_SAMPLES {
        samples.push(v);
    }
}

/// True if some `T` in `schema_types` is *consistent* with `instance`,
/// i.e. mutually subtypes: `instance <: T` AND `T <: instance`. This is
/// stricter than plain `is_subtype` — a node with extra labels or props
/// no longer satisfies a more permissive schema entry. The schema is
/// expected to enumerate every concrete variant present in the data
/// (e.g. `City&Place` separately from `Country&Place`).
fn instance_admitted(instance: &VariableType, schema_types: &[VariableType]) -> bool {
    schema_types
        .iter()
        .any(|t| VariableType::is_subtype(instance, t) && VariableType::is_subtype(t, instance))
}

fn node_instance_type<G: GraphAccess>(g: &G, nid: u32) -> VariableType {
    VariableType::Node(element_descriptor(
        &g.node_labels(nid),
        &props_to_simple(&g.node_props(nid)),
    ))
}

fn edge_instance_type<G: GraphAccess>(g: &G, eid: u32) -> VariableType {
    let desc = element_descriptor(&g.edge_labels(eid), &props_to_simple(&g.edge_props(eid)));
    let left = VariableType::Node(element_descriptor(
        &g.node_labels(g.src(eid)),
        &props_to_simple(&g.node_props(g.src(eid))),
    ));
    let right = VariableType::Node(element_descriptor(
        &g.node_labels(g.tgt(eid)),
        &props_to_simple(&g.node_props(g.tgt(eid))),
    ));
    if g.is_directed(eid) {
        VariableType::EdgeDirectional {
            desc,
            left: Box::new(left),
            right: Box::new(right),
        }
    } else {
        VariableType::EdgeNonDirectional {
            desc,
            left: Box::new(left),
            right: Box::new(right),
        }
    }
}

/// Build a closed descriptor that exactly captures the element's
/// observed labels and prop types. Closed is correct here: an instance
/// has exactly these properties, no more.
fn element_descriptor(labels: &LabelType, props: &BTreeMap<String, SimpleType>) -> DescriptorType {
    let label_strs = sorted_label_strings(labels);
    let label_t = if label_strs.is_empty() {
        LabelType::Star
    } else {
        LabelType::from_list(&label_strs)
    };
    DescriptorType::new(label_t, PropertyType::Closed(props.clone()))
}

fn sorted_label_strings(lt: &LabelType) -> Vec<String> {
    let mut v = MemoryGraphStore::label_strings(lt);
    v.sort();
    v
}

fn props_to_simple(
    props: &std::collections::HashMap<String, Value>,
) -> BTreeMap<String, SimpleType> {
    props
        .iter()
        .map(|(k, v)| (k.clone(), value_to_simple_type(v)))
        .collect()
}

fn value_to_simple_type(v: &Value) -> SimpleType {
    match v {
        Value::Null => SimpleType::Zero,
        Value::Int(_) => SimpleType::Z,
        Value::Float(_) => SimpleType::F,
        Value::Bool(_) => SimpleType::B,
        Value::Str(_) => SimpleType::S,
        Value::List(items) => {
            if items.is_empty() {
                SimpleType::List(Box::new(SimpleType::Star))
            } else {
                let elem = items
                    .iter()
                    .map(value_to_simple_type)
                    .reduce(|acc, t| SimpleType::union(&acc, &t))
                    .unwrap_or(SimpleType::Star);
                SimpleType::List(Box::new(elem))
            }
        }
        Value::Record(fields) => {
            let m: BTreeMap<String, SimpleType> = fields
                .iter()
                .map(|(k, v)| (k.clone(), value_to_simple_type(v)))
                .collect();
            SimpleType::Record(m)
        }
        // Reference values and paths are runtime-only; validation only
        // sees stored property values, never these.
        Value::Node(_) | Value::Edge(_) | Value::Path(_) => SimpleType::Star,
    }
}

fn instance_props_map(vt: &VariableType) -> BTreeMap<String, SimpleType> {
    match vt {
        VariableType::Node(d)
        | VariableType::EdgeDirectional { desc: d, .. }
        | VariableType::EdgeNonDirectional { desc: d, .. } => match &d.props {
            PropertyType::Closed(m) | PropertyType::Open(m) => m.clone(),
            PropertyType::Zero => BTreeMap::new(),
        },
        _ => BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typing::inference::infer_simple_schema;

    fn build_graph(json: &str) -> MemoryGraphStore {
        MemoryGraphStore::from_json_str(json).expect("test graph")
    }

    #[test]
    fn validate_passes_against_inferred_default() {
        let g = build_graph(
            r#"{
                "nodes": [
                    {"id":"a","labels":["Person"],"props":{"name":"Ada","age":30}},
                    {"id":"b","labels":["Person"],"props":{"name":"Bob","age":25}}
                ],
                "edges": []
            }"#,
        );
        let schema = infer_simple_schema(&g);
        let report = validate_against_data(&g, &schema);
        assert!(report.ok(), "report = {report:?}");
        assert_eq!(report.nodes_checked, 2);
    }

    #[test]
    fn validate_flags_missing_required_prop() {
        // Schema says Person is closed-record with name+age. One node lacks age.
        let g = build_graph(
            r#"{
                "nodes": [
                    {"id":"a","labels":["Person"],"props":{"name":"Ada","age":30}},
                    {"id":"b","labels":["Person"],"props":{"name":"Bob"}}
                ],
                "edges": []
            }"#,
        );
        let mut required = BTreeMap::new();
        required.insert("name".to_string(), SimpleType::S);
        required.insert("age".to_string(), SimpleType::Z);
        let schema = Schema::from_parts(
            vec![VariableType::Node(DescriptorType::new(
                LabelType::Label("Person".into()),
                PropertyType::Closed(required),
            ))],
            vec![],
        );
        let report = validate_against_data(&g, &schema);
        assert_eq!(report.node_violations, 1);
        assert!(!report.ok());
    }

    #[test]
    fn validate_flags_unknown_label() {
        let g = build_graph(
            r#"{
                "nodes": [
                    {"id":"a","labels":["Foo"],"props":{}}
                ],
                "edges": []
            }"#,
        );
        let schema = Schema::from_parts(
            vec![VariableType::Node(DescriptorType::new(
                LabelType::Label("Person".into()),
                PropertyType::open_empty(),
            ))],
            vec![],
        );
        let report = validate_against_data(&g, &schema);
        assert_eq!(report.node_violations, 1);
    }

    #[test]
    fn validate_caps_samples() {
        // Build 50 violating nodes; only MAX_SAMPLES retained.
        let mut nodes = String::new();
        for i in 0..50 {
            if i > 0 {
                nodes.push(',');
            }
            nodes.push_str(&format!(r#"{{"id":"n{i}","labels":["Foo"],"props":{{}}}}"#));
        }
        let json = format!(r#"{{"nodes":[{nodes}],"edges":[]}}"#);
        let g = build_graph(&json);
        let schema = Schema::from_parts(
            vec![VariableType::Node(DescriptorType::new(
                LabelType::Label("Bar".into()),
                PropertyType::open_empty(),
            ))],
            vec![],
        );
        let report = validate_against_data(&g, &schema);
        assert_eq!(report.node_violations, 50);
        let node_samples = report
            .samples
            .iter()
            .filter(|s| s.kind == ElementKind::Node)
            .count();
        assert_eq!(node_samples, MAX_SAMPLES);
    }
}
