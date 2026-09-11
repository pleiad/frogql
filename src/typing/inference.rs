//! Schema inference from a live graph. Mirrors the grouping logic of the
//! `print_schema_simple` REPL helper: nodes group by sorted label list,
//! edges group by `(edge_labels, src_labels, tgt_labels, directed)`. For
//! each group, the record is the **union** of every key its instances
//! carry: the type at a key is the union of the types seen there, plus
//! `NULL` when some instance lacked it. The walk sees every element, so
//! the record is closed.
//!
//! It used to take the intersection instead — a key missing from one
//! instance, or typed differently on another, was dropped and the record
//! left open so it stayed addressable as `Star`. That threw away what the
//! walk knew: a label whose instances mostly carry `nombre` reported
//! `{oaci: str, *}`, where the `*` names no property in particular and
//! the schema says nothing about the one property a reader came for. A
//! missing key is not an unknown key. It reads as null, and null is a
//! type this lattice can state.
//!
//! Used to populate the reserved `DEFAULT` graph type at import time and
//! whenever the user runs `USE GRAPH TYPE DEFAULT`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::model::graph::MemoryGraphStore;
use crate::model::graph_access::GraphAccess;
use crate::model::value::Value;

use super::descriptor_type::DescriptorType;
use super::label_type::LabelType;
use super::property_type::PropertyType;
use super::simple_type::SimpleType;
use super::variable_type::{Schema, VariableType};

/// Infer a Schema from the actual data in a graph store.
pub fn infer_simple_schema<G: GraphAccess>(g: &G) -> Schema {
    let node_groups = group_nodes(g);
    let edge_groups = group_edges(g, &node_groups);

    let nodes = node_groups
        .iter()
        .map(|(labels, gp)| node_variable_type(labels, gp))
        .collect();

    let edges = edge_groups
        .iter()
        .map(|(key, gp)| edge_variable_type(key, gp, &node_groups))
        .collect();

    Schema::from_parts(nodes, edges)
}

/// Property type for a single value. Lists infer their element type by
/// unifying every element; an empty list bottoms out at `Star`. Records
/// recurse field-by-field. Atoms map directly.
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
        // Schema inference walks stored property values; reference
        // values and paths are runtime-only and never reach this path.
        // Mapping them to Star keeps the function total without affecting
        // any inferred property type.
        Value::Node(_) | Value::Edge(_) | Value::Path(_) => SimpleType::Star,
    }
}

/// Per-group accumulator: every property key the group's instances carry,
/// the union of the types seen at each, and how many instances carried it.
///
/// It used to accumulate the *intersection* instead, dropping any key that
/// was missing from one instance or typed differently on another, and
/// marking the record open so the dropped keys stayed addressable as
/// `Star`. That threw away what it knew. A label whose instances mostly
/// carry `nombre` reported `{oaci: str, *}` — the `*` naming no property
/// in particular and the schema saying nothing about the one property a
/// reader was looking for. A missing key is not an unknown key: it reads
/// as null, which is a type the lattice can state.
struct Group {
    /// Union of the types seen at each key, over the instances that had
    /// it. A key typed `int` on one instance and `str` on another lands
    /// as `int | str` rather than disappearing.
    types: BTreeMap<String, SimpleType>,
    /// How many instances carried each key. Short of `count`, the key is
    /// optional and the inferred type gains `| NULL`.
    present: BTreeMap<String, usize>,
    count: usize,
}

impl Group {
    fn new() -> Self {
        Group {
            types: BTreeMap::new(),
            present: BTreeMap::new(),
            count: 0,
        }
    }

    fn update(&mut self, instance_props: BTreeMap<String, SimpleType>) {
        self.count += 1;
        for (k, t) in instance_props {
            *self.present.entry(k.clone()).or_insert(0) += 1;
            match self.types.get(&k) {
                None => {
                    self.types.insert(k, t);
                }
                Some(prev) => {
                    let merged = SimpleType::union(prev, &t);
                    self.types.insert(k, merged);
                }
            }
        }
    }

    /// The group's record: every key it ever saw, with `| NULL` on the
    /// ones some instance lacked.
    fn property_types(&self) -> BTreeMap<String, SimpleType> {
        self.types
            .iter()
            .map(|(k, t)| {
                let optional = self.present.get(k).copied().unwrap_or(0) < self.count;
                let t = if optional {
                    SimpleType::union(t, &SimpleType::Null)
                } else {
                    t.clone()
                };
                (k.clone(), t)
            })
            .collect()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct EdgeKey {
    edge_labels: Vec<String>,
    src_labels: Vec<String>,
    tgt_labels: Vec<String>,
    directed: bool,
}

fn group_nodes<G: GraphAccess>(g: &G) -> BTreeMap<Vec<String>, Group> {
    let mut groups: BTreeMap<Vec<String>, Group> = BTreeMap::new();
    for nid in g.nodes() {
        let mut labels = MemoryGraphStore::label_strings(&g.node_labels(nid));
        labels.sort();
        let props: BTreeMap<String, SimpleType> = g
            .node_props(nid)
            .iter()
            .map(|(k, v)| (k.clone(), value_to_simple_type(v)))
            .collect();
        groups
            .entry(labels)
            .or_insert_with(Group::new)
            .update(props);
    }
    groups
}

fn group_edges<G: GraphAccess>(
    g: &G,
    _node_groups: &BTreeMap<Vec<String>, Group>,
) -> BTreeMap<EdgeKey, Group> {
    let mut groups: BTreeMap<EdgeKey, Group> = BTreeMap::new();
    for eid in g.edges_directed().into_iter().chain(g.edges_undirected()) {
        let mut edge_labels = MemoryGraphStore::label_strings(&g.edge_labels(eid));
        edge_labels.sort();
        let mut src_labels = MemoryGraphStore::label_strings(&g.node_labels(g.src(eid)));
        src_labels.sort();
        let mut tgt_labels = MemoryGraphStore::label_strings(&g.node_labels(g.tgt(eid)));
        tgt_labels.sort();
        let directed = g.is_directed(eid);

        let props: BTreeMap<String, SimpleType> = g
            .edge_props(eid)
            .iter()
            .map(|(k, v)| (k.clone(), value_to_simple_type(v)))
            .collect();

        let key = EdgeKey {
            edge_labels,
            src_labels,
            tgt_labels,
            directed,
        };
        groups.entry(key).or_insert_with(Group::new).update(props);
    }
    groups
}

fn labels_to_label_type(labels: &[String]) -> LabelType {
    if labels.is_empty() {
        LabelType::Star
    } else {
        LabelType::from_list(labels)
    }
}

/// Inference walks every element, so the key set it produces is the whole
/// key set and the record is **closed**.
///
/// It used to leave the record open whenever some instance carried a key
/// the others lacked. That was the only way to keep those keys reachable
/// back when they were dropped; now they are listed, with `| NULL`, and
/// an open record would be claiming ignorance the walk does not have.
///
/// What closing changes: reading a key the schema does not list types as
/// `NULL` rather than `Star`, which is what the data says — a closed
/// record means the element definitely has no such property, and ISO
/// reads that projection as null (see `PropertyType::get`).
fn props_to_property_type(props: &BTreeMap<String, SimpleType>) -> PropertyType {
    PropertyType::Closed(props.clone())
}

fn node_descriptor(labels: &[String], gp: &Group) -> DescriptorType {
    DescriptorType::new(
        labels_to_label_type(labels),
        props_to_property_type(&gp.property_types()),
    )
}

fn node_variable_type(labels: &[String], gp: &Group) -> VariableType {
    VariableType::Node(node_descriptor(labels, gp))
}

fn edge_variable_type(
    key: &EdgeKey,
    gp: &Group,
    node_groups: &BTreeMap<Vec<String>, Group>,
) -> VariableType {
    let desc = DescriptorType::new(
        labels_to_label_type(&key.edge_labels),
        props_to_property_type(&gp.property_types()),
    );

    // Endpoints: if the inferred node group exists, reuse its descriptor.
    // Otherwise fall back to a label-only descriptor with an open record —
    // edges may reference label combos that don't appear standalone.
    let left = endpoint_node(&key.src_labels, node_groups);
    let right = endpoint_node(&key.tgt_labels, node_groups);

    if key.directed {
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

fn endpoint_node(labels: &[String], node_groups: &BTreeMap<Vec<String>, Group>) -> VariableType {
    match node_groups.get(labels) {
        Some(gp) => node_variable_type(labels, gp),
        None => VariableType::Node(DescriptorType::new(
            labels_to_label_type(labels),
            PropertyType::open_empty(),
        )),
    }
}

/// Convenience: drop `node_groups` references and just return the edge
/// label sets that appear as endpoints. Used by the REPL display path.
pub fn endpoint_label_combos<G: GraphAccess>(g: &G) -> BTreeSet<Vec<String>> {
    let mut out = BTreeSet::new();
    for eid in g.edges_directed().into_iter().chain(g.edges_undirected()) {
        let mut s = MemoryGraphStore::label_strings(&g.node_labels(g.src(eid)));
        s.sort();
        let mut t = MemoryGraphStore::label_strings(&g.node_labels(g.tgt(eid)));
        t.sort();
        out.insert(s);
        out.insert(t);
    }
    out
}

#[allow(dead_code)]
fn unused_silence_warnings() -> HashMap<u32, u32> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::graph::MemoryGraphStore;

    fn graph_from_json(src: &str) -> MemoryGraphStore {
        MemoryGraphStore::from_json_str(src).expect("test graph json")
    }

    #[test]
    fn infer_atomic_props() {
        let json = r#"{
            "nodes": [
                {"id": "a", "labels": ["Person"], "props": {"name": "Ada", "age": 30}},
                {"id": "b", "labels": ["Person"], "props": {"name": "Bob", "age": 25}}
            ],
            "edges": []
        }"#;
        let g = graph_from_json(json);
        let s = infer_simple_schema(&g);
        assert_eq!(s.nodes.len(), 1);
        match &s.nodes[0] {
            VariableType::Node(d) => {
                assert!(matches!(&d.label, LabelType::Label(l) if l == "Person"));
                match &d.props {
                    PropertyType::Closed(m) => {
                        assert_eq!(m.get("name"), Some(&SimpleType::S));
                        assert_eq!(m.get("age"), Some(&SimpleType::Z));
                    }
                    _ => panic!("expected closed record, got {:?}", d.props),
                }
            }
            _ => panic!("expected Node variant"),
        }
    }

    /// A key some instances lack is `T | NULL`, not a dropped key behind
    /// a `*`. That is what the data says: a missing property reads as
    /// null, and null is a type the lattice can state.
    #[test]
    fn infer_optional_prop_is_nullable_not_dropped() {
        let json = r#"{
            "nodes": [
                {"id": "a", "labels": ["Person"], "props": {"name": "Ada", "age": 30}},
                {"id": "b", "labels": ["Person"], "props": {"name": "Bob"}}
            ],
            "edges": []
        }"#;
        let g = graph_from_json(json);
        let s = infer_simple_schema(&g);
        match &s.nodes[0] {
            VariableType::Node(d) => match &d.props {
                PropertyType::Closed(m) => {
                    assert_eq!(m.get("name"), Some(&SimpleType::S));
                    assert_eq!(
                        m.get("age"),
                        Some(&SimpleType::union(&SimpleType::Z, &SimpleType::Null)),
                        "a key half the instances carry is int | NULL"
                    );
                }
                other => panic!("expected a closed record, got {other:?}"),
            },
            _ => panic!("expected Node variant"),
        }
    }

    /// A key every instance carries, with disagreeing types, is the union
    /// of them — and stays non-null, because nothing was missing.
    #[test]
    fn infer_conflicting_types_unions_without_null() {
        let json = r#"{
            "nodes": [
                {"id": "a", "labels": ["P"], "props": {"v": 1}},
                {"id": "b", "labels": ["P"], "props": {"v": "two"}}
            ],
            "edges": []
        }"#;
        let g = graph_from_json(json);
        let s = infer_simple_schema(&g);
        match &s.nodes[0] {
            VariableType::Node(d) => {
                let t = d.props.get("v");
                assert_eq!(t, SimpleType::union(&SimpleType::Z, &SimpleType::S));
            }
            _ => panic!("expected Node variant"),
        }
    }

    #[test]
    fn infer_list_element_type() {
        let json = r#"{
            "nodes": [
                {"id": "a", "labels": ["Post"], "props": {"tags": ["x", "y"]}},
                {"id": "b", "labels": ["Post"], "props": {"tags": ["z"]}}
            ],
            "edges": []
        }"#;
        let g = graph_from_json(json);
        let s = infer_simple_schema(&g);
        match &s.nodes[0] {
            VariableType::Node(d) => {
                let t = d.props.get("tags");
                assert_eq!(t, SimpleType::List(Box::new(SimpleType::S)));
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn infer_nested_list() {
        let json = r#"{
            "nodes": [
                {"id": "m", "labels": ["Mat"], "props": {"matrix": [[1,2],[3,4]]}}
            ],
            "edges": []
        }"#;
        let g = graph_from_json(json);
        let s = infer_simple_schema(&g);
        match &s.nodes[0] {
            VariableType::Node(d) => {
                let t = d.props.get("matrix");
                assert_eq!(
                    t,
                    SimpleType::List(Box::new(SimpleType::List(Box::new(SimpleType::Z))))
                );
            }
            _ => panic!("expected Node"),
        }
    }

    #[test]
    fn infer_edge_with_endpoints() {
        let json = r#"{
            "nodes": [
                {"id": "a", "labels": ["Person"], "props": {"name": "Ada"}},
                {"id": "b", "labels": ["Post"], "props": {"title": "Hi"}}
            ],
            "edges": [
                {"id": "e1", "labels": ["WROTE"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"}
            ]
        }"#;
        let g = graph_from_json(json);
        let s = infer_simple_schema(&g);
        assert_eq!(s.edges.len(), 1);
        match &s.edges[0] {
            VariableType::EdgeDirectional { desc, left, right } => {
                assert!(matches!(&desc.label, LabelType::Label(l) if l == "WROTE"));
                match left.as_ref() {
                    VariableType::Node(d) => {
                        assert!(matches!(&d.label, LabelType::Label(l) if l == "Person"))
                    }
                    _ => panic!("left endpoint should be Node"),
                }
                match right.as_ref() {
                    VariableType::Node(d) => {
                        assert!(matches!(&d.label, LabelType::Label(l) if l == "Post"))
                    }
                    _ => panic!("right endpoint should be Node"),
                }
            }
            _ => panic!("expected directed edge"),
        }
    }
}
