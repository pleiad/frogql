//! End-to-end tests: import JSON → .gql file → reopen → parse queries → run against GraphStore.
//! These must produce identical results to the in-memory Graph tests.

use std::path::{Path, PathBuf};

use gqlrust::compile;
use gqlrust::runtime::engine::Runtime;
use gqlrust::store::graph_store::GraphStore;

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

fn fraud_store_run(query: &str) -> usize {
    let db_path = temp_path(&format!("fraud_rt_{}.gql", query_hash(query)));
    cleanup(&db_path);

    let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    let store = GraphStore::from_json_file(&db_path, &json_path).unwrap();
    let r = Runtime::new(&store);
    let pattern = compile(query).unwrap();
    let result = r.run(&pattern).rows.len();

    cleanup(&db_path);
    result
}

fn fraud_store_reopen_run(query: &str) -> usize {
    let db_path = temp_path(&format!("fraud_reopen_{}.gql", query_hash(query)));
    cleanup(&db_path);

    let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    // Import
    { let _store = GraphStore::from_json_file(&db_path, &json_path).unwrap(); }
    // Reopen from disk
    let store = GraphStore::open(&db_path).unwrap();
    let r = Runtime::new(&store);
    let pattern = compile(query).unwrap();
    let result = r.run(&pattern).rows.len();

    cleanup(&db_path);
    result
}

fn social_store_run(query: &str) -> usize {
    let db_path = temp_path(&format!("social_rt_{}.gql", query_hash(query)));
    cleanup(&db_path);

    let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/social-network.json");
    let store = GraphStore::from_json_file(&db_path, &json_path).unwrap();
    let r = Runtime::new(&store);
    let pattern = compile(query).unwrap();
    let result = r.run(&pattern).rows.len();

    cleanup(&db_path);
    result
}

// ===== Tests against freshly imported GraphStore =====

#[test] fn test_node_empty() { assert_eq!(fraud_store_run("()"), 5); }
#[test] fn test_node_capturing() { assert_eq!(fraud_store_run("(x)"), 5); }
#[test] fn test_node_filter_by_label() { assert_eq!(fraud_store_run("(x: Account)"), 4); }
#[test] fn test_edge_empty() { assert_eq!(fraud_store_run("-[]->"), 5); }
#[test] fn test_edge_nondirectional() { assert_eq!(fraud_store_run("~~"), 0); }
#[test] fn test_edge_filter_by_label() { assert_eq!(fraud_store_run("-[x: Transfer]->"), 4); }
#[test] fn test_concat() { assert_eq!(fraud_store_run("()-[]->"), 5); }
#[test] fn test_concat_label() { assert_eq!(fraud_store_run("(x)-[:Foo]->"), 1); }
#[test] fn test_size_2() { assert_eq!(fraud_store_run("()-[]->()-[]->()"), 5); }
#[test] fn test_selector() { assert_eq!(fraud_store_run("(x WHERE x.isDummy is bool)"), 1); }
#[test] fn test_concat_selector() { assert_eq!(fraud_store_run("()(x:{isDummy: bool})"), 1); }
#[test] fn test_union() { assert_eq!(fraud_store_run("(x: Dummy) | (y: Account)"), 5); }
#[test] fn test_filter_true() { assert_eq!(fraud_store_run("(y WHERE y.isBlocked=true)"), 1); }
#[test] fn test_filter_false() { assert_eq!(fraud_store_run("(y WHERE y.isBlocked=false)"), 4); }
#[test] fn test_filter_int() { assert_eq!(fraud_store_run("(y WHERE y.isBlocked=1)"), 0); }
#[test] fn test_filter_2() { assert_eq!(fraud_store_run("-[y WHERE y.amount>=3500000 and y.amount>1]->"), 1); }
#[test] fn test_filter_4() { assert_eq!(fraud_store_run("-[y WHERE y.bambino > 0]->"), 0); }
#[test] fn test_union_fail() { assert_eq!(fraud_store_run("(x: NoExists) | (x: NoExists)"), 0); }
#[test] fn test_any_direction() { assert_eq!(fraud_store_run("-"), 10); }
#[test] fn test_repetition() { assert_eq!(fraud_store_run("-->{1,2}"), 23); }
#[test] fn test_repetition_desc() { assert_eq!(fraud_store_run("-[x]->{2,3}"), 10); }
#[test] fn test_repetition_rep() { assert_eq!(fraud_store_run("(-[x]->{1,2}){2,3}"), 60); }
#[test] fn test_digest_p4() {
    assert_eq!(fraud_store_run("(x) -[z:Transfer WHERE z.amount>1000000]-> (y WHERE y.isBlocked=true)"), 1);
}
#[test] fn test_is_bool() { assert_eq!(fraud_store_run("(x WHERE x.isBlocked is bool)"), 5); }
#[test] fn test_is_str() { assert_eq!(fraud_store_run("(x WHERE x.isBlocked is str)"), 0); }
#[test] fn test_as_bool() { assert_eq!(fraud_store_run("(x WHERE x.isBlocked as bool)"), 1); }
#[test] fn test_unop_not() { assert_eq!(fraud_store_run("(x WHERE not x.isBlocked)"), 4); }
#[test] fn test_unop_neg() { assert_eq!(fraud_store_run("-[x WHERE -x.amount < 0]->"), 5); }
#[test] fn test_social_where() { assert_eq!(social_store_run("(x: {status: bool})"), 1); }

// ===== Tests against reopened GraphStore (persistence validation) =====

#[test] fn test_reopen_node_empty() { assert_eq!(fraud_store_reopen_run("()"), 5); }
#[test] fn test_reopen_edge_empty() { assert_eq!(fraud_store_reopen_run("-[]->"), 5); }
#[test] fn test_reopen_filter_label() { assert_eq!(fraud_store_reopen_run("(x: Account)"), 4); }
#[test] fn test_reopen_concat() { assert_eq!(fraud_store_reopen_run("()-[]->"), 5); }
#[test] fn test_reopen_filter() { assert_eq!(fraud_store_reopen_run("(y WHERE y.isBlocked=true)"), 1); }
#[test] fn test_reopen_digest_p4() {
    assert_eq!(fraud_store_reopen_run("(x) -[z:Transfer WHERE z.amount>1000000]-> (y WHERE y.isBlocked=true)"), 1);
}
#[test] fn test_reopen_repetition() { assert_eq!(fraud_store_reopen_run("-->{1,2}"), 23); }
