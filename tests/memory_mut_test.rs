//! Parity tests: GraphAccessMut + MutationOverlay on `MemoryGraphStore`.
//!
//! `MemoryGraphStore` is the in-RAM JSON-backed backend. It implements the
//! same `GraphAccess` + `GraphAccessMut` surface and the same overlay
//! semantics as `LazyGraphStore`, so these mirror `lazy_mut_test.rs` and the
//! `dm_*_test.rs` suites but construct the store directly from the fixture
//! (no save / reopen). Every assertion is on the in-RAM merged view.

use std::collections::HashSet;
use std::path::Path;

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::{GraphAccess, GraphAccessMut};
use gqlrust::model::value::Value;
use gqlrust::parser::parse_statement;
use gqlrust::runtime::dm::run_dm;
use gqlrust::syntax::statement::Statement;
use gqlrust::typing::label_type::LabelType;

fn fraud_mem() -> MemoryGraphStore {
    let json_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    MemoryGraphStore::from_file(&json_path).unwrap()
}

fn props(items: &[(&str, Value)]) -> std::collections::HashMap<String, Value> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn parse_dm_or_panic(input: &str) -> gqlrust::syntax::dm::DmStatement {
    match parse_statement(input).unwrap() {
        Statement::DataModification(dm) => dm,
        other => panic!("expected DM, got {other:?}"),
    }
}

fn count_nodes_with_label(store: &MemoryGraphStore, label: &str) -> usize {
    store.nodes_with_label(label).map(|v| v.len()).unwrap_or(0)
}

// ===================== Trait-level mutation (mirrors lazy_mut_test) =====================

#[test]
fn insert_node_grows_node_set() {
    let store = fraud_mem();
    let base = store.nodes().len();

    let id = store.insert_node(LabelType::Label("Person".into()), props(&[]));
    assert_eq!(id, base as u32, "first overlay id is contiguous with base");
    assert_eq!(store.nodes().len(), base + 1);
    assert!(store.is_node_alive(id));
    assert_eq!(store.node_labels(id), LabelType::Label("Person".into()));
}

#[test]
fn insert_node_with_props_round_trips() {
    let store = fraud_mem();
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
    let store = fraud_mem();
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
    let store = fraud_mem();
    let a = store.insert_node(LabelType::Label("Person".into()), props(&[]));
    let b = store.insert_node(LabelType::Label("Person".into()), props(&[]));
    let e = store.insert_edge(a, b, false, LabelType::Label("FRIENDS".into()), props(&[]));

    assert_eq!(store.undirected_edges_of(a), vec![e]);
    assert_eq!(store.undirected_edges_of(b), vec![e]);
    assert!(!store.is_directed(e));
}

#[test]
fn delete_edge_filters_from_adjacency_and_listing() {
    let store = fraud_mem();
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
    let store = fraud_mem();
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
    let store = fraud_mem();
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
    let store = fraud_mem();
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
    let store = fraud_mem();
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
    let store = fraud_mem();
    assert!(store.nodes_with_label("BrandNew").is_none());
    let id = store.insert_node(LabelType::Label("BrandNew".into()), props(&[]));
    let listed = store.nodes_with_label("BrandNew").unwrap();
    assert!(listed.contains(&id));
}

#[test]
fn set_then_remove_node_prop_on_base_node() {
    let store = fraud_mem();
    let n = store.nodes()[0];
    store.set_node_prop(n, "tag", Value::Str("x".into()));
    assert_eq!(
        store.node_props(n).get("tag"),
        Some(&Value::Str("x".into()))
    );
    store.remove_node_prop(n, "tag");
    assert!(!store.node_props(n).contains_key("tag"));
}

#[test]
fn label_add_remove_on_base_node_reflows_index() {
    let store = fraud_mem();
    let n = store.nodes()[0];
    store.add_node_label(n, "Flagged");
    assert!(store.nodes_with_label("Flagged").unwrap().contains(&n));
    store.remove_node_label(n, "Flagged");
    assert!(!store
        .nodes_with_label("Flagged")
        .unwrap_or_default()
        .contains(&n));
}

// ===================== End-to-end DML via run_dm (mirrors dm_*_test) =====================

#[test]
fn run_dm_standalone_insert() {
    let store = fraud_mem();
    let pre = count_nodes_with_label(&store, "Person");
    let dm = parse_dm_or_panic("INSERT (a:Person {name: 'Alice'})");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_inserted, 1);
    assert_eq!(count_nodes_with_label(&store, "Person"), pre + 1);
}

#[test]
fn run_dm_multi_insert_with_edge() {
    let store = fraud_mem();
    let dm = parse_dm_or_panic(
        "INSERT (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'}), (a)-[:KNOWS]->(b)",
    );
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_inserted, 2);
    assert_eq!(exec.edges_inserted, 1);
}

#[test]
fn run_dm_set_prop() {
    let store = fraud_mem();
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a.owner = 'Renamed'");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_modified, 1);
    let id = store
        .nodes()
        .into_iter()
        .find(
            |&n| matches!(store.node_props(n).get("owner"), Some(Value::Str(s)) if s == "Renamed"),
        )
        .expect("renamed account must surface");
    assert_eq!(
        store.node_props(id).get("owner"),
        Some(&Value::Str("Renamed".into()))
    );
}

#[test]
fn run_dm_set_whole_record_clears_then_writes() {
    let store = fraud_mem();
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a = { only: 'thing' }");
    run_dm(&store, &dm, None).unwrap();
    let id = store
        .nodes()
        .into_iter()
        .find(|&n| store.node_props(n).get("only") == Some(&Value::Str("thing".into())))
        .expect("rewritten account must surface");
    let p = store.node_props(id);
    assert_eq!(p.get("only"), Some(&Value::Str("thing".into())));
    assert!(!p.contains_key("owner"), "owner must be cleared");
}

#[test]
fn run_dm_set_and_remove_label() {
    let store = fraud_mem();
    run_dm(
        &store,
        &parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a:VIP"),
        None,
    )
    .unwrap();
    assert!(!store.nodes_with_label("VIP").unwrap_or_default().is_empty());
    run_dm(
        &store,
        &parse_dm_or_panic("MATCH (a:VIP) REMOVE a:VIP"),
        None,
    )
    .unwrap();
    assert!(store.nodes_with_label("VIP").unwrap_or_default().is_empty());
}

#[test]
fn run_dm_detach_delete() {
    let store = fraud_mem();
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) DETACH DELETE a");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_deleted, 1);
    let still_there = store
        .nodes()
        .into_iter()
        .any(|n| matches!(store.node_props(n).get("owner"), Some(Value::Str(s)) if s == "Aretha"));
    assert!(!still_there, "Aretha must no longer surface after delete");
}

// ===================== JSON round-trip (persistence groundwork for WASM) =====================

#[test]
fn to_json_round_trips_shape_after_dml() {
    let store = fraud_mem();
    run_dm(
        &store,
        &parse_dm_or_panic("INSERT (a:Widget {name: 'w1'})"),
        None,
    )
    .unwrap();
    run_dm(
        &store,
        &parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a.note = 'kept'"),
        None,
    )
    .unwrap();
    let live_nodes = store.nodes().len();
    let live_edges = store.edges_directed().len() + store.edges_undirected().len();

    // Serialize the merged view, then reload it as a fresh in-memory store.
    let json = store.to_json_string();
    let reloaded = MemoryGraphStore::from_json_str(&json).unwrap();

    assert_eq!(reloaded.nodes().len(), live_nodes);
    assert_eq!(
        reloaded.edges_directed().len() + reloaded.edges_undirected().len(),
        live_edges
    );
    // The inserted node survived the round-trip.
    assert_eq!(
        reloaded
            .nodes_with_label("Widget")
            .unwrap_or_default()
            .len(),
        1
    );
    // The SET property survived.
    let kept = reloaded
        .nodes()
        .into_iter()
        .any(|n| reloaded.node_props(n).get("note") == Some(&Value::Str("kept".into())));
    assert!(kept, "SET note must survive JSON round-trip");
}

#[test]
fn to_json_preserves_null_props() {
    // Null is a first-class value; the import path now accepts it, so the
    // dump round-trips losslessly (the gap that JSON had vs `.gdb`).
    let json = r#"{"nodes":[{"id":"n1","labels":["T"],"props":{"a":1,"b":null}}],"edges":[]}"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let n = g.nodes()[0];
    assert_eq!(g.node_props(n).get("b"), Some(&Value::Null));

    let round = MemoryGraphStore::from_json_str(&g.to_json_string()).unwrap();
    let rn = round.nodes()[0];
    assert_eq!(round.node_props(rn).get("b"), Some(&Value::Null));
    assert_eq!(round.node_props(rn).get("a"), Some(&Value::Int(1)));
}
