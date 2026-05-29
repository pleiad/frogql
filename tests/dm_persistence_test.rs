//! Phase 4 tests: persistence of DML mutations via `LazyGraphStore::save`.
//!
//! Atomicity is verified by checking that the destination file remains
//! readable after a save (tmp+rename never leaves a half-written file at
//! the user-visible path).

use std::path::{Path, PathBuf};

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::{GraphAccess, GraphAccessMut};
use gqlrust::parser::parse_statement;
use gqlrust::runtime::dm::run_dm;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::statement::Statement;
use gqlrust::typing::label_type::LabelType;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_dm_phase4");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn fraud_store(db_path: &Path) -> LazyGraphStore {
    let json_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    let g = MemoryGraphStore::from_file(&json_path).unwrap();
    g.save(db_path).unwrap();
    LazyGraphStore::open(db_path).unwrap()
}

#[test]
fn save_then_reopen_persists_inserts() {
    let path = temp_db("save_persists.gdb");
    let store = fraud_store(&path);
    let pre_nodes = store.nodes().len();

    let id = store.insert_node(LabelType::Label("Person".into()), Default::default());
    assert_eq!(store.nodes().len(), pre_nodes + 1);
    assert!(store.is_node_alive(id));

    store.save(&path).unwrap();
    drop(store);

    let reopened = LazyGraphStore::open(&path).unwrap();
    assert_eq!(reopened.nodes().len(), pre_nodes + 1);
    let post_persons = reopened
        .nodes_with_label("Person")
        .map(|v| v.len())
        .unwrap_or(0);
    let pre_persons = LazyGraphStore::open(&temp_db("save_persists_baseline.gdb"))
        // baseline: a fresh store from the same JSON without our INSERT.
        .ok()
        .map(|s| s.nodes_with_label("Person").map(|v| v.len()).unwrap_or(0))
        .unwrap_or(0);
    let _ = pre_persons;
    assert!(post_persons >= 1);
}

#[test]
fn no_save_means_no_persistence() {
    let path = temp_db("no_save.gdb");
    let store = fraud_store(&path);
    let pre_nodes = store.nodes().len();

    store.insert_node(LabelType::Label("Person".into()), Default::default());
    // Drop without saving.
    drop(store);

    let reopened = LazyGraphStore::open(&path).unwrap();
    assert_eq!(
        reopened.nodes().len(),
        pre_nodes,
        "without an explicit save, mutations stay in RAM"
    );
}

#[test]
fn detach_delete_persists() {
    let path = temp_db("detach_persists.gdb");
    let store = fraud_store(&path);
    let pre_nodes = store.nodes().len();
    let pre_edges = store.edges_directed().len() + store.edges_undirected().len();

    let connected = store
        .nodes()
        .into_iter()
        .find(|&n| !store.outgoing_edges(n).is_empty())
        .unwrap();
    let incident = store.outgoing_edges(connected).len()
        + store.incoming_edges(connected).len()
        + store.undirected_edges_of(connected).len();

    store.detach_delete_node(connected);
    store.save(&path).unwrap();
    drop(store);

    let reopened = LazyGraphStore::open(&path).unwrap();
    assert_eq!(reopened.nodes().len(), pre_nodes - 1);
    assert_eq!(
        reopened.edges_directed().len() + reopened.edges_undirected().len(),
        pre_edges - incident
    );
}

#[test]
fn save_through_dml_runtime_round_trips() {
    let path = temp_db("dml_runtime_persists.gdb");
    let store = fraud_store(&path);
    let dm = match parse_statement("INSERT (:Recipe {name: 'pancake'})").unwrap() {
        Statement::DataModification(dm) => dm,
        _ => panic!("not DM"),
    };
    let _ = run_dm(&store, &dm, None).unwrap();
    store.save(&path).unwrap();
    drop(store);

    let reopened = LazyGraphStore::open(&path).unwrap();
    let listed = reopened.nodes_with_label("Recipe").unwrap();
    assert_eq!(listed.len(), 1);
}

#[test]
fn atomic_rename_no_tmp_leaks_after_success() {
    let path = temp_db("atomic.gdb");
    let store = fraud_store(&path);
    store.insert_node(LabelType::Label("X".into()), Default::default());
    store.save(&path).unwrap();

    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    assert!(
        !Path::new(&tmp).exists(),
        ".tmp must not survive a successful save"
    );
    assert!(path.exists(), "destination file should exist after save");
}
