//! MVP-1.C tests: ISO §13.4 REMOVE on node and edge properties.

use std::path::{Path, PathBuf};

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::model::value::Value;
use gqlrust::parser::parse_statement;
use gqlrust::runtime::dm::run_dm;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::statement::Statement;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_dm_remove");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn fraud_store(name: &str) -> LazyGraphStore {
    let db_path = temp_db(name);
    let json_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    let g = MemoryGraphStore::from_file(&json_path).unwrap();
    g.save(&db_path).unwrap();
    LazyGraphStore::open(&db_path).unwrap()
}

fn parse_dm_or_panic(input: &str) -> gqlrust::syntax::dm::DmStatement {
    match parse_statement(input).unwrap() {
        Statement::DataModification(dm) => dm,
        other => panic!("expected DM, got {other:?}"),
    }
}

#[test]
fn remove_property_drops_key() {
    let store = fraud_store("remove_basic.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) REMOVE a.isBlocked");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_modified, 1);

    let id = store
        .nodes_with_label("Account")
        .unwrap()
        .into_iter()
        .find(|&n| matches!(store.node_props(n).get("owner"), Some(Value::Str(s)) if s == "Aretha"))
        .unwrap();
    assert!(!store.node_props(id).contains_key("isBlocked"));
    // Other props survive.
    assert_eq!(
        store.node_props(id).get("owner"),
        Some(&Value::Str("Aretha".into()))
    );
}

#[test]
fn remove_missing_property_is_noop() {
    // ISO §13.4 GR4 a — "if it is contained in the property set"; the
    // remove is silently skipped when the property is absent.
    let store = fraud_store("remove_missing.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) REMOVE a.brand_new");
    let exec = run_dm(&store, &dm, None).unwrap();
    // Counts the operation; idempotent at the data level.
    assert_eq!(exec.nodes_modified, 1);
}

#[test]
fn remove_after_set_round_trip() {
    let store = fraud_store("remove_after_set.gdb");
    let _ = run_dm(
        &store,
        &parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a.note = 'temp'"),
        None,
    )
    .unwrap();
    let _ = run_dm(
        &store,
        &parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) REMOVE a.note"),
        None,
    )
    .unwrap();

    let id = store
        .nodes_with_label("Account")
        .unwrap()
        .into_iter()
        .find(|&n| matches!(store.node_props(n).get("owner"), Some(Value::Str(s)) if s == "Aretha"))
        .unwrap();
    assert!(!store.node_props(id).contains_key("note"));
}

#[test]
fn remove_edge_property() {
    let store = fraud_store("remove_edge_prop.gdb");
    // Set + remove on an edge to confirm the same path.
    let _ = run_dm(
        &store,
        &parse_dm_or_panic("MATCH ()-[e:Transfer]->() SET e.flag = true"),
        None,
    )
    .unwrap();
    let _ = run_dm(
        &store,
        &parse_dm_or_panic("MATCH ()-[e:Transfer]->() REMOVE e.flag"),
        None,
    )
    .unwrap();
    for id in store.directed_edges_with_label("Transfer").unwrap() {
        assert!(!store.edge_props(id).contains_key("flag"));
    }
}
