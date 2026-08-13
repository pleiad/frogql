//! Vector sidecars end to end: build one next to a `.gdb`, reopen the
//! database, and reach the vectors through `GraphAccess`.
//!
//! The sidecar keys on graph-internal node ids, which are only stable
//! for the exact file they were built from. Most of what is asserted
//! here is that a sidecar which stopped matching its database is *not*
//! silently used.

use std::path::{Path, PathBuf};

use frogql::model::graph::MemoryGraphStore;
use frogql::model::graph_access::{GraphAccess, GraphAccessMut};
use frogql::store::lazy::LazyGraphStore;
use frogql::typing::label_type::LabelType;
use frogql::vector::hnsw::{Hnsw, HnswParams};
use frogql::vector::metric::Metric;
use frogql::vector::sidecar::{fingerprint, Sidecar};
use frogql::vector::store::VectorSet;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("frogql_vec_sidecar_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The fraud fixture (5 nodes, 5 edges) saved as a `.gdb`.
fn fraud_db(name: &str) -> PathBuf {
    let dir = temp_dir(name);
    let db_path = dir.join("t.gdb");
    let json_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    let g = MemoryGraphStore::from_file(&json_path).unwrap();
    g.save(&db_path).unwrap();
    db_path
}

/// Give nodes `ids` a 2-D vector each, at (i, 0).
fn write_sidecar(db_path: &Path, attr: &str, ids: Vec<u32>, fp: u64, with_index: bool) {
    let data: Vec<f32> = ids
        .iter()
        .enumerate()
        .flat_map(|(i, _)| vec![i as f32, 0.0])
        .collect();
    let set = VectorSet::new(attr.to_string(), 2, Metric::L2Sq, fp, ids, data);
    let set = if with_index {
        let h = Hnsw::build(&set, HnswParams::default());
        set.with_hnsw(h)
    } else {
        set
    };
    set.to_sidecar()
        .write_to_path(&Sidecar::path_for(db_path, attr))
        .unwrap();
}

fn current_fingerprint(db_path: &Path) -> u64 {
    let store = LazyGraphStore::open(db_path).unwrap();
    fingerprint(store.node_count() as usize, store.edge_count() as usize)
}

#[test]
fn a_matching_sidecar_is_reachable_through_graph_access() {
    let db = fraud_db("reachable");
    let fp = current_fingerprint(&db);
    write_sidecar(&db, "emb", vec![0, 1, 2, 3, 4], fp, false);

    let store = LazyGraphStore::open(&db).unwrap();
    let set = store.vectors("emb").expect("sidecar should load");
    assert_eq!(set.attr(), "emb");
    assert_eq!(set.len(), 5);
    assert_eq!(set.dim(), 2);
    assert_eq!(set.row(2), Some(&[2.0, 0.0][..]));
    assert_eq!(store.vector_store().attrs(), vec!["emb"]);
}

#[test]
fn an_unknown_attribute_is_none() {
    let db = fraud_db("unknown_attr");
    let fp = current_fingerprint(&db);
    write_sidecar(&db, "emb", vec![0, 1], fp, false);

    let store = LazyGraphStore::open(&db).unwrap();
    assert!(store.vectors("nope").is_none());
}

#[test]
fn a_database_without_sidecars_reports_none() {
    let db = fraud_db("no_sidecar");
    let store = LazyGraphStore::open(&db).unwrap();
    assert!(store.vectors("emb").is_none());
    assert!(store.vector_store().is_empty());
}

#[test]
fn a_stale_fingerprint_is_refused() {
    let db = fraud_db("stale_fp");
    // Built for some other graph shape.
    write_sidecar(&db, "emb", vec![0, 1, 2], fingerprint(999, 999), false);

    let store = LazyGraphStore::open(&db).unwrap();
    assert!(
        store.vectors("emb").is_none(),
        "a sidecar from another graph must never be used"
    );
    assert!(store.vector_store().is_empty());
}

#[test]
fn several_attributes_load_side_by_side() {
    let db = fraud_db("multi_attr");
    let fp = current_fingerprint(&db);
    write_sidecar(&db, "title", vec![0, 1, 2], fp, false);
    write_sidecar(&db, "image", vec![1, 3], fp, false);

    let store = LazyGraphStore::open(&db).unwrap();
    assert_eq!(store.vector_store().attrs(), vec!["image", "title"]);
    assert_eq!(store.vectors("title").unwrap().len(), 3);
    assert_eq!(store.vectors("image").unwrap().len(), 2);
    // A partial attribute is legitimate: not every node carries a vector.
    assert!(store.vectors("image").unwrap().row(0).is_none());
    assert!(store.vectors("image").unwrap().row(3).is_some());
}

#[test]
fn the_hnsw_graph_survives_the_round_trip() {
    let db = fraud_db("with_index");
    let fp = current_fingerprint(&db);
    write_sidecar(&db, "emb", vec![0, 1, 2, 3, 4], fp, true);

    let store = LazyGraphStore::open(&db).unwrap();
    let set = store.vectors("emb").expect("loads");
    assert!(set.has_index());

    // Points sit at (0,0)..(4,0); the nearest to (3.1, 0) is node 3.
    let mut c = set.cursor(&[3.1, 0.0], true);
    assert_eq!(c.next().map(|e| e.0), Some(3));
}

#[test]
fn an_unsaved_node_insert_suspends_vector_search() {
    let db = fraud_db("dml_insert");
    let fp = current_fingerprint(&db);
    write_sidecar(&db, "emb", vec![0, 1, 2, 3, 4], fp, false);

    let store = LazyGraphStore::open(&db).unwrap();
    assert!(store.vectors("emb").is_some());

    // The overlay hands out ids above the base watermark, which no
    // sidecar row covers.
    store.insert_node(LabelType::Label("Person".into()), Default::default());
    assert!(
        store.vectors("emb").is_none(),
        "vectors must be suspended while the id space is in flux"
    );
}

#[test]
fn an_unsaved_node_delete_suspends_vector_search() {
    let db = fraud_db("dml_delete");
    let fp = current_fingerprint(&db);
    write_sidecar(&db, "emb", vec![0, 1, 2, 3, 4], fp, false);

    let store = LazyGraphStore::open(&db).unwrap();
    assert!(store.vectors("emb").is_some());

    // Deleting makes the next save() renumber every surviving node.
    store.detach_delete_node(0);
    assert!(store.vectors("emb").is_none());
}

#[test]
fn a_property_change_does_not_suspend_vector_search() {
    let db = fraud_db("dml_setprop");
    let fp = current_fingerprint(&db);
    write_sidecar(&db, "emb", vec![0, 1, 2, 3, 4], fp, false);

    let store = LazyGraphStore::open(&db).unwrap();
    store.set_node_prop(0, "nickname", frogql::model::value::Value::Str("x".into()));
    assert!(
        store.vectors("emb").is_some(),
        "a property is not a vector and cannot move a node id"
    );
}

#[test]
fn the_kill_switch_hides_a_valid_sidecar() {
    let db = fraud_db("kill_switch");
    let fp = current_fingerprint(&db);
    write_sidecar(&db, "emb", vec![0, 1, 2], fp, false);

    // Scoped to this test's open() call. Tests share a process, so the
    // variable is removed immediately after.
    std::env::set_var("FROGQL_DISABLE_VECTORS", "1");
    let store = LazyGraphStore::open(&db).unwrap();
    std::env::remove_var("FROGQL_DISABLE_VECTORS");

    assert!(store.vectors("emb").is_none());
    assert_eq!(
        store.vector_store().attrs(),
        vec!["emb"],
        "diagnostics still see the sidecar, queries do not"
    );
}
