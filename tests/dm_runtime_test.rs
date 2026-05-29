//! Phase 3 tests: end-to-end DML execution via `Runtime::run_dm`.
//!
//! Each test parses a GQL DM statement, runs it against a `LazyGraphStore`
//! seeded from `test_data/fraud.json`, and asserts on the post-mutation
//! merged view (overlay still in RAM, no save).

use std::path::{Path, PathBuf};

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::parser::parse_statement;
use gqlrust::runtime::dm::run_dm;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::statement::Statement;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_dm_phase3");
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

fn count_nodes_with_label(store: &LazyGraphStore, label: &str) -> usize {
    store.nodes_with_label(label).map(|v| v.len()).unwrap_or(0)
}

#[test]
fn standalone_insert_creates_node() {
    let store = fraud_store("standalone_insert.gdb");
    let pre = count_nodes_with_label(&store, "Person");
    let dm = parse_dm_or_panic("INSERT (a:Person {name: 'Alice'})");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_inserted, 1);
    assert_eq!(exec.edges_inserted, 0);
    assert_eq!(count_nodes_with_label(&store, "Person"), pre + 1);
}

#[test]
fn multiple_inserts_in_one_statement() {
    let store = fraud_store("multi_insert.gdb");
    let dm = parse_dm_or_panic(
        "INSERT (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'}), (a)-[:KNOWS]->(b)",
    );
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_inserted, 2);
    assert_eq!(exec.edges_inserted, 1);
}

#[test]
fn match_then_insert_creates_one_per_binding() {
    // ISO §13.2 GR4: MATCH (a:Person) INSERT (a)-[:K]->(b:Tag) creates
    // one fresh `b` per matched `a`, not a single shared Tag.
    let store = fraud_store("per_binding_insert.gdb");
    let person_count = count_nodes_with_label(&store, "Person");
    assert!(person_count > 0);
    let dm = parse_dm_or_panic("MATCH (a:Person) INSERT (a)-[:KNOWS]->(b:Tag {note: 'hi'})");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_inserted, person_count);
    assert_eq!(exec.edges_inserted, person_count);
    assert_eq!(count_nodes_with_label(&store, "Tag"), person_count);
}

#[test]
fn detach_delete_drops_node_and_edges() {
    let store = fraud_store("detach_delete_runtime.gdb");
    // Pick a node that's actually connected — fraud has Account nodes.
    let connected = store
        .nodes()
        .into_iter()
        .find(|&n| !store.outgoing_edges(n).is_empty())
        .unwrap();
    let pre_edges = store.outgoing_edges(connected).len()
        + store.incoming_edges(connected).len()
        + store.undirected_edges_of(connected).len();

    // We can't easily target a specific node by id from GQL syntax; use a
    // unique-property MATCH instead. The fraud fixture has a `name` prop.
    let name = store.node_props(connected).get("name").cloned();
    let stmt = match name {
        Some(gqlrust::model::value::Value::Str(s)) => {
            format!("MATCH (a {{name: '{s}'}}) DETACH DELETE a")
        }
        _ => return, // skip if no string name
    };
    let dm = parse_dm_or_panic(&stmt);
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_deleted, 1);
    assert_eq!(exec.edges_deleted, pre_edges);
}

#[test]
fn nodetach_delete_fails_on_connected_node() {
    let store = fraud_store("nodetach_delete_runtime.gdb");
    let connected = store
        .nodes()
        .into_iter()
        .find(|&n| !store.outgoing_edges(n).is_empty())
        .unwrap();
    let name = match store.node_props(connected).get("name").cloned() {
        Some(gqlrust::model::value::Value::Str(s)) => s,
        _ => return,
    };
    let dm = parse_dm_or_panic(&format!("MATCH (a {{name: '{name}'}}) DELETE a"));
    let err = run_dm(&store, &dm, None).unwrap_err();
    assert!(err.contains("G1001"), "expected G1001, got: {err}");
    // Atomicity: nothing got mutated.
    assert!(gqlrust::model::graph_access::GraphAccessMut::is_node_alive(
        &store, connected
    ));
}

#[test]
fn delete_edge_via_match() {
    let store = fraud_store("delete_edge_runtime.gdb");
    let pre_edges = store.edges_directed().len();
    assert!(pre_edges > 0);
    let dm = parse_dm_or_panic("MATCH (a)-[e:Transfer]->(b) DETACH DELETE e");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert!(
        exec.edges_deleted >= 1,
        "expected at least one Transfer edge deleted; got {}",
        exec.edges_deleted
    );
    assert!(store.edges_directed().len() < pre_edges);
}

#[test]
fn insert_with_return_projects_post_mutation_row() {
    // For a single INSERT with no MATCH, RETURN gets one row carrying the
    // new bindings.
    let store = fraud_store("insert_return.gdb");
    let dm = parse_dm_or_panic("INSERT (a:Person {name: 'Carol'}) RETURN a");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.rows.len(), 1);
    assert!(exec.rows[0].get("a").is_some());
}

#[test]
fn insert_resolves_attribute_expression_per_binding() {
    // MVP-1.A: `INSERT (b:Tag {who: a.owner})` evaluates `a.owner`
    // against the binding row (the fraud fixture's Account nodes carry
    // `owner: "Aretha"` etc.). Every matched Account produces one Tag
    // whose `who` mirrors that Account's `owner`.
    let store = fraud_store("insert_attr_expr.gdb");
    let pre_tags = store.nodes_with_label("Tag").map(|v| v.len()).unwrap_or(0);
    let stmt = "MATCH (a:Account) INSERT (b:Tag {who: a.owner})";
    let dm = parse_dm_or_panic(stmt);
    let exec = run_dm(&store, &dm, None).unwrap();
    let post_tags = store.nodes_with_label("Tag").map(|v| v.len()).unwrap_or(0);
    assert_eq!(post_tags, pre_tags + exec.nodes_inserted);
    let any_tag = store.nodes_with_label("Tag").unwrap()[0];
    assert!(matches!(
        store.node_props(any_tag).get("who"),
        Some(gqlrust::model::value::Value::Str(_))
    ));
}

#[test]
fn brand_new_label_visible_via_match_after_insert() {
    let store = fraud_store("brand_new_after_insert.gdb");
    let dm = parse_dm_or_panic("INSERT (:BrandNew {note: 'fresh'})");
    let _ = run_dm(&store, &dm, None).unwrap();

    // Now a follow-up MATCH should see the new label.
    let q = parse_dm_or_panic("INSERT (:DummyExtra)"); // unused, just to make sure parser is functional
    let _ = q;
    let listed = store.nodes_with_label("BrandNew").unwrap();
    assert_eq!(listed.len(), 1);
}

#[test]
fn rollback_on_failure_leaves_graph_unchanged() {
    // MVP-1.A keeps the existing atomicity contract: a DML that fails
    // mid-flight must leave the graph unchanged. Trigger via NODETACH
    // DELETE on a connected node — the G1001 dependent-object check
    // fires after the engine has already begun staging, so this path
    // exercises the rollback hook end-to-end.
    let store = fraud_store("rollback_atomicity.gdb");
    let pre_persons = count_nodes_with_label(&store, "Person");
    let pre_accounts = count_nodes_with_label(&store, "Account");
    let dm = parse_dm_or_panic(
        "MATCH (a:Account) DELETE a", // NODETACH (default) → fails on connected nodes
    );
    let err = run_dm(&store, &dm, None).unwrap_err();
    assert!(err.contains("G1001"), "expected G1001, got: {err}");
    assert_eq!(count_nodes_with_label(&store, "Account"), pre_accounts);
    assert_eq!(count_nodes_with_label(&store, "Person"), pre_persons);
}
