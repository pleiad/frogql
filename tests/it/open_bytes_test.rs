//! `LazyGraphStore::from_bytes`: a `.gdb` opened from a buffer instead of
//! a file, with its `.ltj` handed in rather than found beside it.
//!
//! This is the browser's entry point. There is no filesystem there, so
//! the page fetches both files and passes the bytes; everything below is
//! the same engine the native build runs.

use frogql::model::graph_access::GraphAccess;
use frogql::runtime::engine::Runtime;
use frogql::store::lazy::LazyGraphStore;

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/fraud_detection.gdb")
}

#[test]
fn bytes_and_file_see_the_same_graph() {
    let path = fixture();
    let from_file = LazyGraphStore::open(&path).expect("open the file");
    let bytes = std::fs::read(&path).expect("read the image");
    let from_bytes = LazyGraphStore::from_bytes(bytes, None).expect("open the bytes");

    assert_eq!(from_file.node_count(), from_bytes.node_count());
    assert_eq!(from_file.edge_count(), from_bytes.edge_count());

    // And the same answers, not merely the same counts.
    let q = "MATCH (a)-[e]->(b) RETURN a, b LIMIT 25";
    let compiled = frogql::compile_query(q).expect("compile");
    let rows_file = format!("{:?}", Runtime::new(&from_file).run_query(&compiled, 25));
    let rows_bytes = format!("{:?}", Runtime::new(&from_bytes).run_query(&compiled, 25));
    assert_eq!(rows_file, rows_bytes, "bytes and file must answer alike");
}

#[test]
fn a_truncated_image_is_refused_at_open() {
    let mut bytes = std::fs::read(fixture()).expect("read");
    bytes.truncate(bytes.len() / 2);
    let msg = match LazyGraphStore::from_bytes(bytes, None) {
        Ok(_) => panic!("a half download is not a database"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("truncated"),
        "the error must name the cause, got: {msg}"
    );
}

/// The sidecar is the reason to fetch a second file: without it the six
/// LTJ trie orderings are rebuilt at open, `O(E log E)`.
#[test]
fn the_ltj_sidecar_bytes_are_used() {
    let path = fixture();
    // Produce a sidecar the normal way: open the file once, which writes
    // `<db>.ltj` if it is missing.
    let side = path.with_extension("gdb.ltj");
    if !side.exists() {
        let store = LazyGraphStore::open(&path).expect("open");
        let _ = Runtime::new(&store).warm_triple_index();
    }
    let ltj = std::fs::read(&side).expect("the sidecar the open just wrote");
    let bytes = std::fs::read(&path).expect("read");

    let store = LazyGraphStore::from_bytes(bytes, Some(ltj)).expect("open the bytes");
    let handed = store
        .index_sidecar_bytes()
        .expect("the bytes must reach the trait hook the runtime consults");

    // And that they *decode*, which is the part a correctness check on
    // the finished index cannot see: every rejection is a silent rebuild,
    // so a sidecar that is never usable looks exactly like one that
    // works, only slower. That is precisely how it would go unnoticed.
    let key = store
        .index_sidecar_key()
        .expect("a store with no pending mutation has a key");
    let want_compact = frogql::runtime::ltj::triple_index::TripleIndex::compact_selected();
    frogql::runtime::ltj::persist::decode(handed, &key, want_compact)
        .expect("the sidecar must be accepted, not silently rebuilt");

    // Built from the sidecar or rebuilt, the index must describe the same
    // graph; this is the check that a silently-rejected sidecar cannot
    // pass as a working one.
    let idx = Runtime::new(&store).warm_triple_index();
    assert_eq!(
        idx.len(),
        store.edge_count() as usize,
        "the index must hold one triple per edge"
    );
}
