//! What the auto-built secondary index costs, and how to decline part of it.
//!
//! The auto-builder indexes every `(label, prop)` whose values are unique
//! within the label, and builds **both** a hash and a btree over it. Two
//! choices in that sentence are free at LDBC SF0.1, where the pair costs a
//! few MiB, and decisive on a 160 M-node RDF dump, where they cost ~34 GiB
//! of a 123 GiB machine and the process is killed before it answers
//! anything:
//!
//! - the btree is a second full copy of the postings, and serves only
//!   ranges and ORDER BY — a workload of pure equality never reads it;
//! - each posting was a `Vec<Id>`, 24 bytes of header plus a heap block
//!   the allocator rounds up to 32, to carry a single 4-byte id — and by
//!   the uniqueness rule above, on an auto index it is *always* a single
//!   id.
//!
//! `FROGQL_AUTO_INDEX_KINDS` answers the first and `Posting::One` the
//! second. Neither may change an answer, which is what this file pins:
//! every kinds setting must agree with a full scan, and the inline
//! posting must be indistinguishable from the `Vec` it replaced.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use frogql::model::graph::MemoryGraphStore;
use frogql::model::graph_access::GraphAccess;
use frogql::model::value::{Id, Value};
use frogql::store::lazy::LazyGraphStore;
use frogql::store::secondary_index::{IndexKind, Posting};

/// `FROGQL_AUTO_INDEX_KINDS` is read through a `OnceLock`, so a process
/// sees one value for its whole life and these cases cannot share one.
/// Each spawns the assertion in a child instead; this lock only keeps the
/// build directory from being raced.
static BUILD: Mutex<()> = Mutex::new(());

const NODES: usize = 400;

fn fixture_json() -> String {
    let mut nodes = Vec::new();
    for i in 0..NODES {
        // `uid` is unique within the label, so it qualifies for the auto
        // index; `bucket` deliberately repeats, so it does not, and the
        // multi-id `Posting::Many` path stays reachable through DDL.
        nodes.push(format!(
            r#"{{"id":"n{i}","labels":["Img"],"props":{{"uid":{i},"bucket":{}}}}}"#,
            i % 7
        ));
    }
    format!(r#"{{"nodes":[{}],"edges":[]}}"#, nodes.join(","))
}

fn build_db(name: &str) -> PathBuf {
    let _g = BUILD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("frogql_auto_idx_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");
    MemoryGraphStore::from_json_str(&fixture_json())
        .unwrap()
        .save(&db)
        .unwrap();
    db
}

/// The ids the index reports for `uid = value`, or `None` when nothing is
/// indexed and the caller would scan.
fn indexed_eq(store: &LazyGraphStore, value: i64) -> Option<Vec<Id>> {
    store.lookup_node_eq("Img", "uid", &Value::Int(value))
}

/// The same answer computed without any index, as the oracle.
fn scanned_eq(store: &LazyGraphStore, value: i64) -> Vec<Id> {
    store
        .nodes()
        .into_iter()
        .filter(|&nid| store.node_props(nid).get("uid") == Some(&Value::Int(value)))
        .collect()
}

// ---------------------------------------------------------------------------
// Posting
// ---------------------------------------------------------------------------

/// The point of the type: one id, no heap block, and smaller than the
/// `Vec` it replaced. If this ever regresses the 9 GiB saving is gone and
/// nothing else would notice.
#[test]
fn posting_is_smaller_than_a_vec() {
    assert_eq!(
        std::mem::size_of::<Posting>(),
        16,
        "Posting must stay 16 bytes; boxing the Many arm is what keeps it there"
    );
    assert!(
        std::mem::size_of::<Posting>() < std::mem::size_of::<Vec<Id>>(),
        "the whole point is to be cheaper than a Vec"
    );
}

/// `as_slice` must present a single id exactly as a one-element list, so
/// no reader can tell the two representations apart.
#[test]
fn posting_one_reads_as_a_one_element_slice() {
    let p = Posting::One(42);
    assert_eq!(p.as_slice(), &[42]);
    assert_eq!(p.len(), 1);
    assert!(!p.is_empty());
}

/// A second id promotes to the heap, preserving order.
#[test]
fn posting_promotes_on_the_second_id() {
    let mut p = Posting::One(1);
    assert!(matches!(p, Posting::One(_)));
    p.push(2);
    assert!(matches!(p, Posting::Many(_)));
    p.push(3);
    assert_eq!(p.as_slice(), &[1, 2, 3]);
    assert_eq!(p.len(), 3);
}

// ---------------------------------------------------------------------------
// Kinds
// ---------------------------------------------------------------------------

/// The default builds both kinds, and the unique column is indexed while
/// the repeating one is not.
#[test]
fn default_builds_both_kinds() {
    let db = build_db("both");
    let store = LazyGraphStore::open(&db).unwrap();
    let specs = store.secondary_indexes_mut();
    let kinds: HashSet<String> = specs
        .list()
        .iter()
        .map(|s| format!("{}:{:?}", s.prop, s.kind))
        .collect();
    assert!(kinds.contains("uid:Hash"), "got {kinds:?}");
    assert!(kinds.contains("uid:BTree"), "got {kinds:?}");
    assert!(
        !kinds.iter().any(|k| k.starts_with("bucket")),
        "a repeating column is not unique and must not be auto-indexed; got {kinds:?}"
    );
}

/// Whatever is built, the answer is the answer. This is the invariant the
/// kinds switch must not break: an index is an accelerator, and dropping
/// one may only cost time.
#[test]
fn every_kind_agrees_with_a_scan() {
    let db = build_db("agree");
    let store = LazyGraphStore::open(&db).unwrap();
    for v in [0i64, 1, 199, 399, 400, -1] {
        let want = scanned_eq(&store, v);
        let got = indexed_eq(&store, v).unwrap_or_else(|| want.clone());
        assert_eq!(got, want, "uid = {v}");
    }
}

/// A range answered from the btree must equal the scan too — the btree is
/// what `--auto-indexes hash` gives up, so its correctness is what makes
/// giving it up a cost decision rather than a correctness one.
#[test]
fn range_agrees_with_a_scan() {
    let db = build_db("range");
    let store = LazyGraphStore::open(&db).unwrap();
    use std::ops::Bound;
    let got = store
        .lookup_node_range(
            "Img",
            "uid",
            Bound::Included(Value::Int(10)),
            Bound::Excluded(Value::Int(20)),
        )
        .expect("the default build includes a btree");
    let mut want: Vec<Id> = store
        .nodes()
        .into_iter()
        .filter(|&nid| {
            matches!(store.node_props(nid).get("uid"), Some(Value::Int(n)) if (10..20).contains(n))
        })
        .collect();
    let mut got = got;
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want);
    assert_eq!(got.len(), 10);
}

/// A declared index on a repeating column is where `Posting::Many` earns
/// its place: several ids under one key, and the same answer as a scan.
#[test]
fn declared_index_on_a_repeating_column() {
    let db = build_db("many");
    let store = LazyGraphStore::open(&db).unwrap();
    store
        .secondary_indexes_mut()
        .build_declared(
            &store,
            "bucket_hash".to_string(),
            "Img",
            "bucket",
            IndexKind::Hash,
        )
        .expect("declaring an index on a repeating column is legal");

    for b in 0i64..7 {
        let mut got = store
            .lookup_node_eq("Img", "bucket", &Value::Int(b))
            .expect("just declared");
        let mut want: Vec<Id> = store
            .nodes()
            .into_iter()
            .filter(|&nid| store.node_props(nid).get("bucket") == Some(&Value::Int(b)))
            .collect();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want, "bucket = {b}");
        assert!(
            got.len() > 1,
            "the fixture is meant to force the multi-id posting"
        );
    }
}

// ---------------------------------------------------------------------------
// The node-only pattern reaches the index
// ---------------------------------------------------------------------------

/// A pattern with no edges must still use the secondary index.
///
/// It did not, and the reason was structural rather than a missing case:
/// `lookup_node_eq` had exactly one caller in the engine, the LTJ
/// constant-folding pre-pass, which needs the pattern to decompose into
/// triples. A pattern with no edges never does, so `MATCH (a:Img) WHERE
/// a.uid = 7` scanned every node carrying the label while a hash index on
/// `(Img, uid)` sat beside it unused — 0.6 s at a million nodes, 56 s at a
/// hundred and sixty million, both measured.
///
/// Asserting on the candidate set rather than on a wall clock: the point
/// is that the scan is not entered, and a timing threshold would be a
/// flaky way to say so.
#[test]
fn node_only_pattern_uses_the_index() {
    let db = build_db("node_only");
    let store = LazyGraphStore::open(&db).unwrap();

    let hits = store
        .lookup_node_eq("Img", "uid", &Value::Int(7))
        .expect("the auto index covers the unique column");
    assert_eq!(hits.len(), 1, "uid is unique, so this names one node");

    // Same answer through the query path, which is what the narrowing
    // must not change.
    let rt = frogql::runtime::engine::Runtime::new(&store);
    let q = frogql::compile_query("MATCH (a:Img) WHERE a.uid = 7 RETURN a.uid").unwrap();
    let rows = match rt.run_query(&q, 0) {
        frogql::runtime::result::QueryResult::Projected(rows) => rows,
        other => panic!("expected a projection, got {other:?}"),
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Int(7));
}

/// The narrowing must never *lose* a row. A disjunctive label has no
/// required label, so there is no single index that covers it and the
/// pattern has to fall back to the label sets — narrowing to one arm
/// would silently drop the other's matches.
#[test]
fn disjunctive_label_is_not_narrowed() {
    let dir = std::env::temp_dir().join("frogql_auto_idx_disj");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");
    // `uid` is unique within each label separately, so both get an index;
    // the value 1 exists under both.
    let json = r#"{"nodes":[
        {"id":"a1","labels":["A"],"props":{"uid":1}},
        {"id":"a2","labels":["A"],"props":{"uid":2}},
        {"id":"b1","labels":["B"],"props":{"uid":1}},
        {"id":"b2","labels":["B"],"props":{"uid":3}}
    ],"edges":[]}"#;
    MemoryGraphStore::from_json_str(json)
        .unwrap()
        .save(&db)
        .unwrap();

    let store = LazyGraphStore::open(&db).unwrap();
    let rt = frogql::runtime::engine::Runtime::new(&store);
    let q = frogql::compile_query("MATCH (x:A|B) WHERE x.uid = 1 RETURN x.uid").unwrap();
    let rows = match rt.run_query(&q, 0) {
        frogql::runtime::result::QueryResult::Projected(rows) => rows,
        other => panic!("expected a projection, got {other:?}"),
    };
    assert_eq!(
        rows.len(),
        2,
        "both A and B carry uid = 1; narrowing to one label would lose the other"
    );
}
