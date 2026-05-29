//! Phase 8 tests: pg_dump-style JSON snapshot of a graph.
//!
//! Round-trip property: `import_json(dump_json(g))` reproduces `g`.
//! Plus we cover the post-mutation case — INSERT a brand-new node with
//! a brand-new label, dump, reload, verify the node survives.

use std::path::{Path, PathBuf};

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccess;

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_dump_phase8");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn fraud_graph() -> MemoryGraphStore {
    let json_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    MemoryGraphStore::from_file(&json_path).unwrap()
}

#[test]
fn dump_then_load_round_trips_node_and_edge_counts() {
    let g = fraud_graph();
    let dump_path = temp_path("round_trip.json");
    gqlrust::store::dump::dump_to_json_file(&g, &dump_path).unwrap();

    let loaded = MemoryGraphStore::from_file(&dump_path).unwrap();
    assert_eq!(loaded.node_count(), g.node_count());
    assert_eq!(loaded.edge_count(), g.edge_count());
}

#[test]
fn dump_includes_node_labels_and_props() {
    let g = fraud_graph();
    let dump_path = temp_path("labels_props.json");
    gqlrust::store::dump::dump_to_json_file(&g, &dump_path).unwrap();
    let loaded = MemoryGraphStore::from_file(&dump_path).unwrap();

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

    let loaded = MemoryGraphStore::from_file(&dump_path).unwrap();
    assert_eq!(loaded.node_count(), pre + 1);
    assert!(
        loaded
            .nodes_with_label("BrandNew")
            .map(|v| !v.is_empty())
            .unwrap_or(false),
        "BrandNew must survive the dump round-trip"
    );
}

// --- MVP-1.F: .dump-gql round-trip --------------------------------------

fn execute_gql_script(store: &gqlrust::store::lazy::LazyGraphStore, script: &str) {
    use gqlrust::parser::parse_statement;
    use gqlrust::runtime::dm::run_dm;
    use gqlrust::syntax::statement::Statement;

    // A naive splitter: `;` followed by newline ends one statement.
    // Comment lines (`-- ...`) are stripped before parsing because the
    // lexer accepts them but `parse_statement` rejects an input that
    // contains nothing but a comment ("expected ... got Eof").
    for raw in script.split(";\n") {
        let stripped: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            continue;
        }
        match parse_statement(trimmed)
            .unwrap_or_else(|e| panic!("dump-gql parse error on {trimmed:?}: {e}"))
        {
            Statement::DataModification(dm) => {
                run_dm(store, &dm, None)
                    .unwrap_or_else(|e| panic!("dump-gql exec error on {trimmed:?}: {e}"));
            }
            other => panic!("dump-gql produced non-DM statement: {other:?}"),
        }
    }
}

#[test]
fn dump_gql_round_trip_reproduces_graph_shape() {
    use gqlrust::store::lazy::LazyGraphStore;

    // Build a temp .gdb from the fraud fixture and dump-gql.
    let src_path = temp_path("dump_gql_src.gdb");
    fraud_graph().save(&src_path).unwrap();
    let src = LazyGraphStore::open(&src_path).unwrap();

    let script = gqlrust::store::dump::dump_to_gql_string(&src).unwrap();
    assert!(script.contains("INSERT"));
    assert!(script.contains("MATCH"));
    assert!(script.contains("REMOVE n._dump_id"));

    // Reload into a fresh empty database by executing the script.
    let dst_path = temp_path("dump_gql_dst.gdb");
    let dst = LazyGraphStore::open_or_create(&dst_path).unwrap();
    execute_gql_script(&dst, &script);

    // Node and edge counts match.
    let src_node_ids = src.nodes();
    let dst_node_ids = dst.nodes();
    assert_eq!(
        src_node_ids.len(),
        dst_node_ids.len(),
        "node count mismatch after round-trip"
    );
    let src_edge_count = src.edges_directed().len() + src.edges_undirected().len();
    let dst_edge_count = dst.edges_directed().len() + dst.edges_undirected().len();
    assert_eq!(
        src_edge_count, dst_edge_count,
        "edge count mismatch after round-trip"
    );

    // Per-label node counts match.
    for label in ["Account", "Person", "Dummy"] {
        let pre = src.nodes_with_label(label).map(|v| v.len()).unwrap_or(0);
        let post = dst.nodes_with_label(label).map(|v| v.len()).unwrap_or(0);
        assert_eq!(pre, post, "label '{label}' count diverged");
    }
    // Per-label edge counts match.
    let pre_t = src
        .directed_edges_with_label("Transfer")
        .map(|v| v.len())
        .unwrap_or(0);
    let post_t = dst
        .directed_edges_with_label("Transfer")
        .map(|v| v.len())
        .unwrap_or(0);
    assert_eq!(pre_t, post_t, "Transfer edge count diverged");

    // The synthetic _dump_id property must be gone after the cleanup.
    for nid in dst.nodes() {
        let props = dst.node_props(nid);
        assert!(
            !props.contains_key("_dump_id"),
            "node {nid} still carries _dump_id after cleanup: {props:?}"
        );
    }
}

#[test]
fn dump_gql_avoids_dump_id_collision() {
    use gqlrust::model::graph_access::GraphAccessMut;
    use gqlrust::model::value::Value;
    use gqlrust::store::lazy::LazyGraphStore;
    use gqlrust::typing::label_type::LabelType;

    // A graph that already has a `_dump_id` property: the dumper must
    // pick `__dump_id_v1` (or later) instead.
    let db_path = temp_path("dump_gql_collide.gdb");
    fraud_graph().save(&db_path).unwrap();
    let store = LazyGraphStore::open(&db_path).unwrap();
    let mut props = std::collections::HashMap::new();
    props.insert("_dump_id".to_string(), Value::Str("preexisting".into()));
    store.insert_node(LabelType::Label("Tag".into()), props);

    let script = gqlrust::store::dump::dump_to_gql_string(&store).unwrap();
    assert!(
        !script.contains("INSERT (:Tag {_dump_id: 'preexisting', _dump_id:"),
        "dumper must not double-use the prop name"
    );
    // The fallback should be `__dump_id_v1`.
    assert!(
        script.contains("__dump_id_v1"),
        "expected fallback '__dump_id_v1' in script:\n{script}"
    );
    // And REMOVE should target the same fallback.
    assert!(
        script.contains("REMOVE n.__dump_id_v1"),
        "cleanup must REMOVE the fallback prop:\n{script}"
    );
}
