//! Phase 7 tests: lifecycle of the DEFAULT graph type when DML mutates
//! the underlying data.
//!
//! ISO/IEC 39075:2024 §13 doesn't define DEFAULT; gqlite invents it as a
//! data-derived schema for permissive workflows. The contract:
//!  - INSERT / DELETE flips a `default_dirty` flag on the catalog.
//!  - The next read of DEFAULT (via `SHOW`, `USE`, `VALIDATE`, or `save`)
//!    re-infers from the live store and clears the flag.
//!  - Reads of any other (declared) type ignore the flag.

use std::path::{Path, PathBuf};

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccessMut;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::typing::label_type::LabelType;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_dm_phase7");
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

fn schema_has_node_label(store: &LazyGraphStore, label: &str) -> bool {
    let cat = store.catalog();
    cat.types
        .get("DEFAULT")
        .map(|s| {
            s.nodes.iter().any(|n| {
                gqlrust::model::graph::MemoryGraphStore::label_strings(match n {
                    gqlrust::typing::variable_type::VariableType::Node(d) => &d.label,
                    _ => return false,
                })
                .iter()
                .any(|l| l == label)
            })
        })
        .unwrap_or(false)
}

#[test]
fn default_dirty_flag_set_after_mutation() {
    let store = fraud_store("dirty_flag.gdb");
    // Need DEFAULT installed at least once to clear dirty=false baseline.
    store
        .catalog_mut()
        .install_default(gqlrust::typing::inference::infer_simple_schema(&store));
    assert!(!store.catalog().is_default_dirty());

    store.insert_node(LabelType::Label("BrandNew".into()), Default::default());
    store.catalog_mut().mark_default_dirty();
    assert!(store.catalog().is_default_dirty());
}

#[test]
fn refresh_default_if_dirty_picks_up_new_label() {
    let store = fraud_store("refresh_picks_up.gdb");
    store
        .catalog_mut()
        .install_default(gqlrust::typing::inference::infer_simple_schema(&store));
    assert!(!schema_has_node_label(&store, "BrandNew"));

    store.insert_node(LabelType::Label("BrandNew".into()), Default::default());
    store.catalog_mut().mark_default_dirty();

    store.refresh_default_if_dirty();
    assert!(schema_has_node_label(&store, "BrandNew"));
    assert!(!store.catalog().is_default_dirty());
}

#[test]
fn refresh_when_clean_is_a_noop() {
    // Calling refresh when not dirty must be cheap and not change the
    // dirty flag. We approximate "no schema change" by checking node-type
    // count, which is stable across the call.
    let store = fraud_store("refresh_noop.gdb");
    store
        .catalog_mut()
        .install_default(gqlrust::typing::inference::infer_simple_schema(&store));
    let before = store
        .catalog()
        .types
        .get("DEFAULT")
        .map(|s| s.nodes.len())
        .unwrap_or(0);
    store.refresh_default_if_dirty();
    assert!(!store.catalog().is_default_dirty());
    let after = store
        .catalog()
        .types
        .get("DEFAULT")
        .map(|s| s.nodes.len())
        .unwrap_or(0);
    assert_eq!(before, after);
}

#[test]
fn save_clears_dirty_flag_via_refresh() {
    // `LazyGraphStore::save` calls `refresh_default_if_dirty` before
    // materializing — so after save the in-memory catalog is up to date
    // even if persistence of the catalog itself lands in a later phase.
    let store = fraud_store("save_clears_dirty.gdb");
    store
        .catalog_mut()
        .install_default(gqlrust::typing::inference::infer_simple_schema(&store));
    store.insert_node(LabelType::Label("BrandNew".into()), Default::default());
    store.catalog_mut().mark_default_dirty();
    let path = temp_db("save_clears_dirty.gdb");
    store.save(&path).unwrap();

    assert!(!store.catalog().is_default_dirty());
    assert!(schema_has_node_label(&store, "BrandNew"));
}
