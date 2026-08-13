//! `FROGQL_DISABLE_VECTORS`, the kill switch for vector search.
//!
//! This is its own integration target on purpose. An environment
//! variable is process-global while `cargo test` runs a file's tests
//! concurrently in one process, so setting it inside a file that also
//! opens databases expecting vectors to work is a race. One test binary,
//! one test.

use std::path::Path;

use frogql::model::graph::MemoryGraphStore;
use frogql::model::graph_access::GraphAccess;
use frogql::store::lazy::LazyGraphStore;
use frogql::vector::metric::Metric;
use frogql::vector::sidecar::{fingerprint, Sidecar};
use frogql::vector::store::VectorSet;

#[test]
fn the_kill_switch_hides_a_valid_sidecar() {
    let dir = std::env::temp_dir().join("frogql_vec_kill_switch");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");

    let json_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    MemoryGraphStore::from_file(&json_path)
        .unwrap()
        .save(&db)
        .unwrap();

    let fp = {
        let s = LazyGraphStore::open(&db).unwrap();
        fingerprint(s.node_count() as usize, s.edge_count() as usize)
    };
    VectorSet::new(
        "emb".to_string(),
        1,
        Metric::L2Sq,
        fp,
        vec![0, 1, 2],
        vec![0.0, 1.0, 2.0],
    )
    .to_sidecar()
    .write_to_path(&Sidecar::path_for(&db, "emb"))
    .unwrap();

    // Without the switch the sidecar is live.
    assert!(LazyGraphStore::open(&db).unwrap().vectors("emb").is_some());

    std::env::set_var("FROGQL_DISABLE_VECTORS", "1");
    let store = LazyGraphStore::open(&db).unwrap();
    std::env::remove_var("FROGQL_DISABLE_VECTORS");

    assert!(store.vectors("emb").is_none(), "queries must not see it");
    assert_eq!(
        store.vector_store().attrs(),
        vec!["emb"],
        "diagnostics still see the sidecar, so the switch is visible as a choice"
    );
}
