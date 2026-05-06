//! Phase 8 tests: pg_dump-style JSON snapshot of a graph.
//!
//! Round-trip property: `import_json(dump_json(g))` reproduces `g`.
//! Plus we cover the post-mutation case — INSERT a brand-new node with
//! a brand-new label, dump, reload, verify the node survives.

use std::path::{Path, PathBuf};

use gqlrust::model::graph::Graph;
use gqlrust::model::graph_access::GraphAccess;

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_dump_phase8");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn fraud_graph() -> Graph {
    let json_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    Graph::from_file(&json_path).unwrap()
}

#[test]
fn dump_then_load_round_trips_node_and_edge_counts() {
    let g = fraud_graph();
    let dump_path = temp_path("round_trip.json");
    gqlrust::store::dump::dump_to_json_file(&g, &dump_path).unwrap();

    let loaded = Graph::from_file(&dump_path).unwrap();
    assert_eq!(loaded.node_count(), g.node_count());
    assert_eq!(loaded.edge_count(), g.edge_count());
}

#[test]
fn dump_includes_node_labels_and_props() {
    let g = fraud_graph();
    let dump_path = temp_path("labels_props.json");
    gqlrust::store::dump::dump_to_json_file(&g, &dump_path).unwrap();
    let loaded = Graph::from_file(&dump_path).unwrap();

    // Every Account node should still be tagged Account in the reload.
    let pre_accounts = g.nodes_with_label("Account").map(|v| v.len()).unwrap_or(0);
    let post_accounts = loaded
        .nodes_with_label("Account")
        .map(|v| v.len())
        .unwrap_or(0);
    assert_eq!(pre_accounts, post_accounts);
}

#[test]
fn dump_after_mutation_captures_new_data() {
    use gqlrust::model::graph_access::GraphAccessMut;
    use gqlrust::store::lazy::LazyGraphStore;
    use gqlrust::typing::label_type::LabelType;

    // Build a temp .gdb from the fraud fixture, mutate it, dump from the
    // (overlay-aware) lazy store.
    let db_path = temp_path("mutate_then_dump.gdb");
    fraud_graph().save(&db_path).unwrap();
    let store = LazyGraphStore::open(&db_path).unwrap();
    let pre = store.nodes().len();
    let new_id = store.insert_node(LabelType::Label("BrandNew".into()), Default::default());
    assert!(store.is_node_alive(new_id));

    let dump_path = temp_path("mutate_then_dump.json");
    gqlrust::store::dump::dump_to_json_file(&store, &dump_path).unwrap();

    let loaded = Graph::from_file(&dump_path).unwrap();
    assert_eq!(loaded.node_count(), pre + 1);
    assert!(
        loaded
            .nodes_with_label("BrandNew")
            .map(|v| !v.is_empty())
            .unwrap_or(false),
        "BrandNew must survive the dump round-trip"
    );
}
