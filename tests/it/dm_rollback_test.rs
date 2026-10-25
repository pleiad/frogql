//! What a failed data-modifying statement leaves behind.
//!
//! A statement that fails (G2000 under a declared graph type, G1001 on a
//! `NODETACH DELETE`, ...) rolls the store back. The policy is coarse on
//! purpose: back to the last `save`, or to the opened image before the
//! first one, so earlier successful statements that were not saved are
//! undone with it. These tests pin that policy and the four bugs that sat
//! behind it, all found while building a Python demo notebook:
//!
//! - the rollback went back to the *opened* image, past a `save`, so the
//!   next `save` erased rows that had already been written to the file;
//! - every embedder refreshed its LTJ index only on success, so after a
//!   failure the index still held edges the rollback removed, and the next
//!   query walked into one and panicked (`index out of bounds`);
//! - the LTJ delta and the overlay secondary index read the overlay by
//!   offset, which a rollback invalidates by handing the same ids out
//!   again: a lookup missed a node inserted after it, `ORDER BY` came back
//!   out of order;
//! - `node_count` / `edge_count` reported the opened image, not what the
//!   connection's own queries see.

use std::path::PathBuf;

use frogql::model::graph::{MemoryGraphStore, Props};
use frogql::model::graph_access::GraphAccessMut;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::typing::label_type::LabelType;
use frogql::{Connection, Error, Value};

fn temp_db(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("frogql_dm_rollback_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.gdb");
    for ext in ["", ".ltj", ".tmp"] {
        let mut p = path.as_os_str().to_os_string();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
    path
}

/// A database with two `P` nodes (unique `k`, so the auto-builder indexes
/// `(P, k)`) and a declared type that rejects a string `k`.
fn open_typed(name: &str) -> (PathBuf, Connection) {
    let path = temp_db(name);
    frogql::connection::import_json_str(
        &path,
        r#"{"nodes":[
            {"id":"a","labels":["P"],"props":{"k":1}},
            {"id":"b","labels":["P"],"props":{"k":2}}],"edges":[]}"#,
    )
    .unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute("CREATE GRAPH TYPE g AS { (:P {k INT}), (:P)-[:R]->(:P) }")
        .unwrap();
    conn.execute("USE GRAPH TYPE g").unwrap();
    (path, conn)
}

fn ks(conn: &Connection, query: &str) -> Vec<i64> {
    conn.query(query)
        .unwrap()
        .iter()
        .map(|r| match r.values()[0] {
            Value::Int(i) => i,
            ref v => panic!("expected an int, got {v:?}"),
        })
        .collect()
}

fn rejected_insert(conn: &Connection) {
    match conn.execute("INSERT (:P {k: 'not an int'})") {
        Err(Error::Execution(msg)) => {
            assert!(msg.contains("G2000"), "{msg}");
            assert!(msg.contains("rolled back"), "the message says so: {msg}");
        }
        other => panic!("expected a G2000 rejection, got {other:?}"),
    }
}

#[test]
fn rollback_before_any_save_undoes_every_unsaved_statement() {
    // The policy, pinned: not only the failed statement.
    let (_path, conn) = open_typed("before_save");
    conn.execute("INSERT (:P {k: 3})").unwrap();
    conn.execute("INSERT (:P {k: 4})").unwrap();
    rejected_insert(&conn);
    assert_eq!(ks(&conn, "MATCH (x:P) RETURN x.k ORDER BY x.k"), vec![1, 2]);
}

#[test]
fn rollback_keeps_what_was_saved() {
    let (path, conn) = open_typed("after_save");
    conn.execute("INSERT (:P {k: 3})").unwrap();
    conn.save().unwrap();
    conn.execute("INSERT (:P {k: 4})").unwrap();
    rejected_insert(&conn);
    // `k: 3` was saved and stays; `k: 4` was not and goes.
    assert_eq!(
        ks(&conn, "MATCH (x:P) RETURN x.k ORDER BY x.k"),
        vec![1, 2, 3]
    );
    conn.execute("INSERT (:P {k: 5})").unwrap();
    conn.save().unwrap();
    drop(conn);

    // This is the data loss: the second save used to write the opened
    // image plus `k: 5`, and `k: 3` was gone from the file.
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        ks(&conn, "MATCH (x:P) RETURN x.k ORDER BY x.k"),
        vec![1, 2, 3, 5]
    );
}

#[test]
fn failed_dml_leaves_no_phantom_edge() {
    let (_path, conn) = open_typed("phantom_edge");
    conn.execute("MATCH (a:P {k: 1}), (b:P {k: 2}) INSERT (a)-[:R]->(b)")
        .unwrap();
    assert_eq!(ks(&conn, "MATCH (a)-[:R]->(b) RETURN a.k"), vec![1]);
    rejected_insert(&conn);
    // The index kept the edge in its delta: this panicked.
    assert!(ks(&conn, "MATCH (a)-[e]->(b) RETURN a.k").is_empty());
    assert!(ks(&conn, "MATCH (a)-[:R]->(b), (b)-[]->(z) RETURN a.k").is_empty());
    assert_eq!(conn.edge_count(), 0);
}

#[test]
fn overlay_index_sees_nodes_inserted_after_a_rollback() {
    let (_path, conn) = open_typed("overlay_index");
    conn.execute("INSERT (:P {k: 100})").unwrap();
    // A lookup absorbs the overlay into the index delta.
    assert_eq!(ks(&conn, "MATCH (x:P {k: 100}) RETURN x.k"), vec![100]);
    rejected_insert(&conn);
    // Two inserts before the next lookup reuse the freed id and grow the
    // log past the old absorbed offset, which was the case it skipped.
    conn.execute("INSERT (:P {k: 7}), (:P {k: 8})").unwrap();
    assert_eq!(ks(&conn, "MATCH (x:P {k: 7}) RETURN x.k"), vec![7]);
    assert_eq!(ks(&conn, "MATCH (x:P {k: 8}) RETURN x.k"), vec![8]);
    assert!(ks(&conn, "MATCH (x:P {k: 100}) RETURN x.k").is_empty());
    assert_eq!(
        ks(&conn, "MATCH (x:P) RETURN x.k ORDER BY x.k"),
        vec![1, 2, 7, 8]
    );
}

#[test]
fn counts_include_unsaved_changes() {
    let (_path, conn) = open_typed("counts");
    assert_eq!((conn.node_count(), conn.edge_count()), (2, 0));
    conn.execute("MATCH (a:P {k: 1}), (b:P {k: 2}) INSERT (a)-[:R]->(b), (:P {k: 3})")
        .unwrap();
    assert_eq!((conn.node_count(), conn.edge_count()), (3, 1));
    conn.execute("MATCH (x:P {k: 1}) DETACH DELETE x").unwrap();
    assert_eq!((conn.node_count(), conn.edge_count()), (2, 0));
    // The live counts equal the opened image's here by coincidence. One
    // more insert separates them, and `save` must not bring the opened
    // image's count back: the session keeps reading that image.
    conn.execute("INSERT (:P {k: 4})").unwrap();
    conn.save().unwrap();
    assert_eq!((conn.node_count(), conn.edge_count()), (3, 0));
    let rows = conn.query("MATCH (n) RETURN COUNT(n) AS n").unwrap();
    assert_eq!(rows.iter().next().unwrap().get("n"), Some(&Value::Int(3)));
}

/// The delta under an index built mid-session cannot survive a rollback:
/// the watermark it records is an id the rollback hands out again, to a
/// different edge. Before `MutationOverlay::epoch`, the refresh saw "no
/// edge past the watermark" and kept the stale index.
#[test]
fn delta_refuses_a_stamp_from_before_a_rollback() {
    let store = MemoryGraphStore::from_json_str(
        r#"{"nodes":[
            {"id":"a","labels":["N"],"props":{"id":0}},
            {"id":"b","labels":["N"],"props":{"id":1}}],"edges":[]}"#,
    )
    .unwrap();
    store.insert_edge(0, 1, true, LabelType::Label("R".into()), Props::new());
    let rt = Runtime::new(&store);
    // Built with the overlay edge in it: the stamp is not the opened image.
    let _ = rt.warm_triple_index();

    store.rollback_session();
    // Same id, other edge.
    store.insert_edge(1, 0, true, LabelType::Label("S".into()), Props::new());
    rt.invalidate_caches();

    let count = |q: &str| -> usize {
        let query = frogql::compile_query(q).unwrap();
        match rt.run_query(&query, 0) {
            QueryResult::Projected(rows) => rows.len(),
            QueryResult::Raw(ir) => ir.rows.len(),
        }
    };
    assert_eq!(count("MATCH (x)-[:R]->(y) RETURN x.id"), 0, "phantom R");
    assert_eq!(count("MATCH (x)-[:S]->(y) RETURN x.id"), 1, "missing S");
}
