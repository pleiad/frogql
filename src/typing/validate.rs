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

/// True if some `T` in `schema_types` admits `instance`.
///
/// Labels must match exactly — mutual subtyping, so a node with an extra
/// label does not satisfy a narrower entry. The schema is expected to
/// enumerate every concrete variant present in the data (e.g. `City&Place`
/// separately from `Country&Place`).
///
/// Properties are checked **one way, key by key**: the element's value
/// type must be a subtype of the schema's, and a key the element does not
/// carry reads as `NULL`, the rule `PropertyType::get` applies to a closed
/// record. So `opt STRING | NULL` and `opt ANY` admit an element without
/// `opt`, while `opt STRING` requires it.
///
/// This used to be mutual subtyping on the whole type, which demands the
/// same key set on both sides and a schema type no wider than the value.
/// Inference prints `opt STRING | NULL` for a property some elements lack,
/// so the DEFAULT inferred from a graph failed `VALIDATE` against that
/// same graph — for every element missing an optional property, and for
/// every edge whose endpoint was one.
fn instance_admitted(instance: &VariableType, schema_types: &[VariableType]) -> bool {
    schema_types.iter().any(|t| admits(t, instance))
}

fn admits(schema: &VariableType, instance: &VariableType) -> bool {
    match (schema, instance) {
        (VariableType::Node(s), VariableType::Node(i)) => descriptor_admits(s, i),
        (
            VariableType::EdgeDirectional {
                desc: sd,
                left: sl,
                right: sr,
            },
            VariableType::EdgeDirectional {
                desc: id,
                left: il,
                right: ir,
            },
        ) => descriptor_admits(sd, id) && admits(sl, il) && admits(sr, ir),
        (
            VariableType::EdgeNonDirectional {
                desc: sd,
                left: sl,
                right: sr,
            },
            VariableType::EdgeNonDirectional {
                desc: id,
                left: il,
                right: ir,
            },
        ) => {
            descriptor_admits(sd, id)
                && ((admits(sl, il) && admits(sr, ir)) || (admits(sl, ir) && admits(sr, il)))
        }
        (VariableType::Union(a, b), _) => admits(a, instance) || admits(b, instance),
        _ => {
            VariableType::is_subtype(instance, schema) && VariableType::is_subtype(schema, instance)
        }
    }
}

fn descriptor_admits(schema: &DescriptorType, instance: &DescriptorType) -> bool {
    labels_admit(&schema.label, &instance.label) && props_admit(&schema.props, &instance.props)
}

/// Whether the schema label admits the element's exact label set.
///
/// An element without labels is described by `Star` (see
/// `element_descriptor`), and `Star` is the gradual *unknown* label: it is
/// a subtype of every label and every label is a subtype of it. So mutual
/// subtyping let an unlabelled node pass any node type at all —
/// `INSERT ({name: 'x'})` under `(:Actor {name STRING})` was accepted and
/// stored with no label. The empty label set is checked here instead, by
/// evaluating the schema label on it.
fn labels_admit(schema: &LabelType, instance: &LabelType) -> bool {
    if *instance == LabelType::Star {
        return admits_no_labels(schema);
    }
    LabelType::is_subtype(instance, schema) && LabelType::is_subtype(schema, instance)
}

/// Whether an element with no labels satisfies `l`. A type written with
/// no label (`({k INT})`, printed for `Star` and `Top` alike) constrains
/// none, so it admits one; any atomic label it requires does not.
fn admits_no_labels(l: &LabelType) -> bool {
    match l {
        LabelType::Star | LabelType::Top | LabelType::Empty => true,
        LabelType::Label(_) => false,
        LabelType::And(a, b) => admits_no_labels(a) && admits_no_labels(b),
        LabelType::Or(a, b) => admits_no_labels(a) || admits_no_labels(b),
        LabelType::Neg(a) => !admits_no_labels(a),
    }
}

fn props_admit(schema: &PropertyType, instance: &PropertyType) -> bool {
    let keys_of = |p: &PropertyType| -> Vec<String> {
        match p {
            PropertyType::Open(m) | PropertyType::Closed(m) => m.keys().cloned().collect(),
            PropertyType::Zero => Vec::new(),
        }
    };
    if matches!(schema, PropertyType::Zero) {
        return false;
    }
    keys_of(schema)
        .into_iter()
        .chain(keys_of(instance))
        .all(|k| SimpleType::is_subtype(&instance.get(&k), &schema.get(&k)))
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
///
/// No labels becomes `Star`, for want of a label type meaning "exactly
/// none". `labels_admit` reads it that way; nothing else may compare an
/// instance's labels.
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
        // Temporal values are query-time only in this phase; they never
        // occur in stored properties, but the mapping is total anyway.
        Value::Date(_) => SimpleType::Date,
        Value::LocalDatetime(_) => SimpleType::LocalDatetime,
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

    /// The DEFAULT inferred from a graph validates that graph. Inference
    /// prints `opt STRING | NULL` for a property only some elements carry;
    /// the validator demanded the same key set on both sides, so every
    /// element missing `opt` failed — and so did every edge touching one.
    #[test]
    fn validate_passes_inferred_default_with_optional_property() {
        let g = build_graph(
            r#"{
                "nodes": [
                    {"id":"a","labels":["A"],"props":{"k":"x","opt":"y"}},
                    {"id":"b","labels":["A"],"props":{"k":"z"}},
                    {"id":"c","labels":["B"],"props":{}}
                ],
                "edges": [
                    {"id":"e","labels":["R"],"endpoints":["b","c"],"directionality":"->"}
                ]
            }"#,
        );
        let schema = infer_simple_schema(&g);
        let report = validate_against_data(&g, &schema);
        assert!(report.ok(), "report = {report:?}");
    }

    /// `T | NULL` and `ANY` admit a missing key; plain `T` does not; a
    /// key the schema does not list is still rejected by a closed record.
    #[test]
    fn validate_reads_a_missing_key_as_null() {
        let g =
            build_graph(r#"{"nodes": [{"id":"a","labels":["A"],"props":{"k":"x"}}], "edges": []}"#);
        let node = |props: &[(&str, SimpleType)]| {
            Schema::from_parts(
                vec![VariableType::Node(DescriptorType::new(
                    LabelType::Label("A".into()),
                    PropertyType::Closed(
                        props
                            .iter()
                            .map(|(k, t)| (k.to_string(), t.clone()))
                            .collect(),
                    ),
                ))],
                vec![],
            )
        };
        let nullable_s = SimpleType::Union(Box::new(SimpleType::S), Box::new(SimpleType::Null));
        let ok = |sch: Schema| validate_against_data(&g, &sch).ok();
        assert!(ok(node(&[("k", SimpleType::S), ("opt", nullable_s)])));
        assert!(ok(node(&[("k", SimpleType::S), ("opt", SimpleType::Star)])));
        assert!(!ok(node(&[("k", SimpleType::S), ("opt", SimpleType::S)])));
        assert!(!ok(node(&[("opt", SimpleType::Star)])), "k is not declared");
        assert!(!ok(node(&[("k", SimpleType::Z)])), "k holds a string");
    }

    /// An element with no labels used to pass every node type, because it
    /// is described by `Star` and `Star` is compatible with any label.
    #[test]
    fn validate_flags_unlabelled_node_under_a_labelled_type() {
        let g =
            build_graph(r#"{"nodes": [{"id":"a","labels":[],"props":{"name":"x"}}], "edges": []}"#);
        let typed = |label: LabelType| {
            let mut props = BTreeMap::new();
            props.insert("name".to_string(), SimpleType::S);
            Schema::from_parts(
                vec![VariableType::Node(DescriptorType::new(
                    label,
                    PropertyType::Closed(props),
                ))],
                vec![],
            )
        };
        let ok = |sch: Schema| validate_against_data(&g, &sch).ok();
        assert!(!ok(typed(LabelType::Label("Actor".into()))));
        assert!(!ok(typed(LabelType::And(
            Box::new(LabelType::Label("Actor".into())),
            Box::new(LabelType::Star),
        ))));
        // A type written without a label constrains none.
        assert!(ok(typed(LabelType::Star)));
        assert!(ok(typed(LabelType::Neg(Box::new(LabelType::Label(
            "Actor".into()
        ))))));
        // And the inferred DEFAULT still validates the graph it came from.
        assert!(validate_against_data(&g, &infer_simple_schema(&g)).ok());
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
