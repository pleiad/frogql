//! Phase 2 tests: GraphAccessMut + MutationOverlay on LazyGraphStore.
//!
//! Uses the existing `test_data/fraud.json` fixture (5 nodes, 5 edges)
//! save-and-reopened as a `.gdb`. We never `save()` in these tests:
//! every assertion is on the in-RAM merged view (base + overlay).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::{GraphAccess, GraphAccessMut};
use gqlrust::model::value::Value;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::typing::label_type::LabelType;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_dm_phase2");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn fraud_store(name: &str) -> (LazyGraphStore, PathBuf) {
    let db_path = temp_db(name);
    let json_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    let g = MemoryGraphStore::from_file(&json_path).unwrap();
    g.save(&db_path).unwrap();
    let store = LazyGraphStore::open(&db_path).unwrap();
    (store, db_path)
}

fn props(items: &[(&str, Value)]) -> std::collections::HashMap<String, Value> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

#[test]
fn insert_node_grows_node_set() {
    let (store, _path) = fraud_store("insert_node_grows.gdb");
    let base = store.nodes().len();

    let id = store.insert_node(LabelType::Label("Person".into()), props(&[]));
    assert_eq!(id, base as u32, "first overlay id is contiguous with base");
    assert_eq!(store.nodes().len(), base + 1);
    assert!(store.is_node_alive(id));
    assert_eq!(store.node_labels(id), LabelType::Label("Person".into()));
}

#[test]
fn insert_node_with_props_round_trips() {
    let (store, _path) = fraud_store("insert_node_with_props.gdb");
    let id = store.insert_node(
        LabelType::Label("Person".into()),
        props(&[
            ("name", Value::Str("Alice".into())),
            ("age", Value::Int(30)),
        ]),
    );
    let props_back = store.node_props(id);
    assert_eq!(props_back.get("name"), Some(&Value::Str("Alice".into())));
    assert_eq!(props_back.get("age"), Some(&Value::Int(30)));
}

#[test]
fn insert_edge_appears_in_adjacency() {
    let (store, _path) = fraud_store("insert_edge_adj.gdb");
    let a = store.insert_node(LabelType::Label("Person".into()), props(&[]));
    let b = store.insert_node(LabelType::Label("Person".into()), props(&[]));
    let e = store.insert_edge(a, b, true, LabelType::Label("KNOWS".into()), props(&[]));

    assert!(store.is_edge_alive(e));
    assert_eq!(store.outgoing_edges(a), vec![e]);
    assert_eq!(store.incoming_edges(b), vec![e]);
    assert_eq!(store.src(e), a);
    assert_eq!(store.tgt(e), b);
    assert!(store.is_directed(e));
}

#[test]
fn undirected_edge_appears_in_both_endpoints() {
    let (store, _path) = fraud_store("insert_und_edge.gdb");
    let a = store.insert_node(LabelType::Label("Person".into()), props(&[]));
    let b = store.insert_node(LabelType::Label("Person".into()), props(&[]));
    let e = store.insert_edge(a, b, false, LabelType::Label("FRIENDS".into()), props(&[]));

    assert_eq!(store.undirected_edges_of(a), vec![e]);
    assert_eq!(store.undirected_edges_of(b), vec![e]);
    assert!(!store.is_directed(e));
}

#[test]
fn delete_edge_filters_from_adjacency_and_listing() {
    let (store, _path) = fraud_store("delete_edge.gdb");
    let any_edge = store.edges_directed()[0];
    let src = store.src(any_edge);
    let pre_count = store.outgoing_edges(src).len();
    assert!(pre_count > 0);

    store.delete_edge(any_edge);

    assert!(!store.is_edge_alive(any_edge));
    assert!(!store.outgoing_edges(src).contains(&any_edge));
    let listed: HashSet<_> = store.edges_directed().into_iter().collect();
    assert!(!listed.contains(&any_edge));
}

#[test]
fn delete_node_no_detach_rejects_when_edges_remain() {
    let (store, _path) = fraud_store("nodetach_rejects.gdb");
    // Pick a node that has at least one outgoing edge.
    let busy_node = store
        .nodes()
        .into_iter()
        .find(|&n| !store.outgoing_edges(n).is_empty())
        .expect("fraud fixture must contain a connected node");

    let err = store.delete_node_no_detach(busy_node).unwrap_err();
    assert_eq!(err.node, busy_node);
    assert!(!err.remaining_edges.is_empty());
    // Atomicity: nothing got tombstoned.
    assert!(store.is_node_alive(busy_node));
}

#[test]
fn detach_delete_node_drops_node_and_incident_edges() {
    let (store, _path) = fraud_store("detach_delete.gdb");
    let busy_node = store
        .nodes()
        .into_iter()
        .find(|&n| !store.outgoing_edges(n).is_empty())
        .unwrap();
    let incident: Vec<_> = store
        .outgoing_edges(busy_node)
        .into_iter()
        .chain(store.incoming_edges(busy_node))
        .chain(store.undirected_edges_of(busy_node))
        .collect();
    assert!(!incident.is_empty());

    store.detach_delete_node(busy_node);

    assert!(!store.is_node_alive(busy_node));
    for e in incident {
        assert!(!store.is_edge_alive(e));
    }
}

#[test]
fn rollback_session_restores_clean_state() {
    let (store, _path) = fraud_store("rollback.gdb");
    let base_nodes = store.nodes().len();
    let base_edges = store.edges_directed().len() + store.edges_undirected().len();

    let a = store.insert_node(LabelType::Label("X".into()), props(&[]));
    let b = store.insert_node(LabelType::Label("X".into()), props(&[]));
    store.insert_edge(a, b, true, LabelType::Label("E".into()), props(&[]));
    store.delete_edge(store.edges_directed()[0]);

    store.rollback_session();

    assert_eq!(store.nodes().len(), base_nodes);
    assert_eq!(
        store.edges_directed().len() + store.edges_undirected().len(),
        base_edges
    );
}

#[test]
fn nodes_with_label_includes_overlay() {
    let (store, _path) = fraud_store("nodes_with_label.gdb");
    // Fraud has 4 :Account nodes; insert one more.
    let pre: HashSet<_> = store
        .nodes_with_label("Account")
        .unwrap_or_default()
        .into_iter()
        .collect();
    let new_id = store.insert_node(LabelType::Label("Account".into()), props(&[]));
    let post: HashSet<_> = store
        .nodes_with_label("Account")
        .unwrap_or_default()
        .into_iter()
        .collect();
    assert!(post.contains(&new_id));
    assert_eq!(post.len(), pre.len() + 1);
}

#[test]
fn nodes_with_brand_new_label_is_visible() {
    let (store, _path) = fraud_store("brand_new_label.gdb");
    // Label "BrandNew" is not in the fraud fixture.
    assert!(store.nodes_with_label("BrandNew").is_none());
    let id = store.insert_node(LabelType::Label("BrandNew".into()), props(&[]));
    let listed = store.nodes_with_label("BrandNew").unwrap();
    assert!(listed.contains(&id));
}
