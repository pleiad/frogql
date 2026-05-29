//! End-to-end tests: save to .gql file → reopen → compile queries → run.
//! These must produce identical results to the in-memory JSON-loaded MemoryGraphStore tests.

use std::path::{Path, PathBuf};

use gqlrust::compile;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::runtime::engine::Runtime;

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_test");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn query_hash(q: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    q.hash(&mut h);
    h.finish()
}

/// Load JSON → save to .gql → run query on the saved MemoryGraphStore
fn fraud_store_run(query: &str) -> usize {
    let db_path = temp_path(&format!("fraud_rt_{}.gql", query_hash(query)));
    cleanup(&db_path);

    let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    let graph = MemoryGraphStore::from_file(&json_path).unwrap();
    graph.save(&db_path).unwrap();

    // Reopen from .gql
    let graph = MemoryGraphStore::open(&db_path).unwrap();
    let r = Runtime::new(&graph);
    let pattern = compile(query).unwrap();
    let result = r.run(&pattern).rows.len();

    cleanup(&db_path);
    result
}

fn social_store_run(query: &str) -> usize {
    let db_path = temp_path(&format!("social_rt_{}.gql", query_hash(query)));
    cleanup(&db_path);

    let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/social-network.json");
    let graph = MemoryGraphStore::from_file(&json_path).unwrap();
    graph.save(&db_path).unwrap();

    let graph = MemoryGraphStore::open(&db_path).unwrap();
    let r = Runtime::new(&graph);
    let pattern = compile(query).unwrap();
    let result = r.run(&pattern).rows.len();

    cleanup(&db_path);
    result
}

// ===== Tests against save → reopen cycle =====

#[test]
fn test_node_empty() {
    assert_eq!(fraud_store_run("()"), 5);
}
#[test]
fn test_node_capturing() {
    assert_eq!(fraud_store_run("(x)"), 5);
}
#[test]
fn test_node_filter_by_label() {
    assert_eq!(fraud_store_run("(x: Account)"), 4);
}
#[test]
fn test_edge_empty() {
    assert_eq!(fraud_store_run("-[]->"), 5);
}
#[test]
fn test_edge_nondirectional() {
    assert_eq!(fraud_store_run("~~"), 0);
}
#[test]
fn test_edge_filter_by_label() {
    assert_eq!(fraud_store_run("-[x: Transfer]->"), 4);
}
#[test]
fn test_concat() {
    assert_eq!(fraud_store_run("()-[]->"), 5);
}
#[test]
fn test_concat_label() {
    assert_eq!(fraud_store_run("(x)-[:Foo]->"), 1);
}
#[test]
fn test_size_2() {
    assert_eq!(fraud_store_run("()-[]->()-[]->()"), 5);
}
#[test]
fn test_selector() {
    assert_eq!(fraud_store_run("(x WHERE x.isDummy bool)"), 1);
}
#[test]
fn test_concat_selector() {
    assert_eq!(fraud_store_run("()(x:{isDummy bool})"), 1);
}
#[test]
fn test_union() {
    assert_eq!(fraud_store_run("(x: Dummy) | (y: Account)"), 5);
}
#[test]
fn test_filter_true() {
    assert_eq!(fraud_store_run("(y WHERE y.isBlocked=true)"), 1);
}
#[test]
fn test_filter_false() {
    assert_eq!(fraud_store_run("(y WHERE y.isBlocked=false)"), 4);
}
#[test]
fn test_filter_int() {
    assert_eq!(fraud_store_run("(y WHERE y.isBlocked=1)"), 0);
}
#[test]
fn test_filter_2() {
    assert_eq!(
        fraud_store_run("-[y WHERE y.amount>=3500000 and y.amount>1]->"),
        1
    );
}
#[test]
fn test_filter_4() {
    assert_eq!(fraud_store_run("-[y WHERE y.bambino > 0]->"), 0);
}
#[test]
fn test_union_fail() {
    assert_eq!(fraud_store_run("(x: NoExists) | (x: NoExists)"), 0);
}
#[test]
fn test_any_direction() {
    assert_eq!(fraud_store_run("-"), 10);
}
#[test]
fn test_repetition() {
    assert_eq!(fraud_store_run("-->{1,2}"), 23);
}
#[test]
fn test_repetition_desc() {
    assert_eq!(fraud_store_run("-[x]->{2,3}"), 10);
}
#[test]
fn test_repetition_rep() {
    assert_eq!(fraud_store_run("(-[x]->{1,2}){2,3}"), 60);
}
#[test]
fn test_digest_p4() {
    assert_eq!(
        fraud_store_run("(x) -[z:Transfer WHERE z.amount>1000000]-> (y WHERE y.isBlocked=true)"),
        1
    );
}
#[test]
fn test_is_bool() {
    assert_eq!(fraud_store_run("(x WHERE x.isBlocked bool)"), 5);
}
#[test]
fn test_is_str() {
    assert_eq!(fraud_store_run("(x WHERE x.isBlocked str)"), 0);
}
#[test]
fn test_as_bool() {
    assert_eq!(fraud_store_run("(x WHERE x.isBlocked as bool)"), 1);
}
#[test]
fn test_unop_not() {
    assert_eq!(fraud_store_run("(x WHERE not x.isBlocked)"), 4);
}
#[test]
fn test_unop_neg() {
    assert_eq!(fraud_store_run("-[x WHERE -x.amount < 0]->"), 5);
}
#[test]
fn test_social_where() {
    assert_eq!(social_store_run("(x: {status bool})"), 1);
}
#[test]
fn test_social_undirected() {
    assert_eq!(social_store_run("~[:Knows]~"), 2);
}
#[test]
fn test_multi_label() {
    assert_eq!(fraud_store_run("(x: Dummy & Person)"), 1);
}
