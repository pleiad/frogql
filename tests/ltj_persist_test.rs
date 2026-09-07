//! A loaded LTJ index must be indistinguishable from a built one.
//!
//! Building the index is a pure function of the graph, and it lands
//! entirely at open: 670 ms at LDBC SF0.1, and 252 seconds — measured — on
//! a 617 M-edge RDF dump, once per session, every session. `ltj_build`
//! computes it once into `<db>.ltj` and the engine reads it back.
//!
//! That is only a saving if the two are the same index, which is what this
//! file pins. It is the same discipline as `compact_ltj_test.rs`: one
//! optimisation, one kill switch (`FROGQL_LTJ_SOURCE=build`), one test
//! asserting "optimised ≡ baseline".
//!
//! The other half is the rejections. A stale index is worse than no index
//! — it answers with edges that are gone and misses edges that are new,
//! and nothing downstream re-checks it — so every way a sidecar can fail
//! to describe the graph in front of it has to end in a rebuild rather
//! than in a wrong answer or a crash.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use frogql::model::graph::MemoryGraphStore;
use frogql::model::graph_access::GraphAccess;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::ltj::persist::{self, Reject};
use frogql::runtime::ltj::triple_index::TripleIndex;
use frogql::runtime::result::QueryResult;
use frogql::store::lazy::LazyGraphStore;

/// `FROGQL_LTJ_COMPACT` is process-global; these cases all read it, so
/// they must not run concurrently with one that sets it.
static ENV: Mutex<()> = Mutex::new(());

const NODES: usize = 60;

/// An RDF-shaped graph: several labels, several edges per node, and a
/// couple of parallel edges so the bag-multiplicity side table is not
/// empty.
fn fixture_json() -> String {
    let mut nodes = Vec::new();
    for i in 0..NODES {
        nodes.push(format!(
            r#"{{"id":"n{i}","labels":["Img"],"props":{{"id":{i}}}}}"#
        ));
    }
    let mut edges = Vec::new();
    let mut e = 0usize;
    let push = |edges: &mut Vec<String>, label: &str, a: usize, b: usize, e: &mut usize| {
        edges.push(format!(
            r#"{{"id":"e{}","labels":["{label}"],"props":{{}},"endpoints":["n{a}","n{b}"],"directionality":"->"}}"#,
            *e
        ));
        *e += 1;
    };
    for i in 0..NODES {
        push(&mut edges, "P69", i, (i * 7 + 1) % NODES, &mut e);
        push(&mut edges, "P6", i, (i * 13 + 5) % NODES, &mut e);
        if i % 5 == 0 {
            push(&mut edges, "P926", i, (i * 3 + 2) % NODES, &mut e);
            // A parallel edge: same (src, label, tgt), a distinct element.
            push(&mut edges, "P926", i, (i * 3 + 2) % NODES, &mut e);
        }
    }
    format!(
        r#"{{"nodes":[{}],"edges":[{}]}}"#,
        nodes.join(","),
        edges.join(",")
    )
}

fn build_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("frogql_ltj_persist_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");
    MemoryGraphStore::from_json_str(&fixture_json())
        .unwrap()
        .save(&db)
        .unwrap();
    db
}

fn counts(db: &Path) -> (usize, usize) {
    let store = LazyGraphStore::open(db).unwrap();
    store.index_sidecar_key().map(|(_, n, e)| (n, e)).unwrap()
}

/// Write the sidecar for `db` in the representation this process is
/// configured for, the way `ltj_build` does.
fn write_sidecar(db: &Path) {
    let store = LazyGraphStore::open(db).unwrap();
    let (n, e) = store.index_sidecar_key().map(|(_, n, e)| (n, e)).unwrap();
    let idx = TripleIndex::from_graph(&store);
    persist::write_to_path(&idx, &persist::path_for(db), n, e).unwrap();
}

/// Every query shape that goes through LTJ: a chain, a comma-join, a
/// label-free edge, and a parallel-edge pattern whose row count is the
/// bag multiplicity.
const QUERIES: [&str; 5] = [
    "MATCH (a:Img)-[:P69]->(b:Img) RETURN a.id, b.id ORDER BY a.id, b.id",
    "MATCH (a:Img)-[:P69]->(b:Img), (a)-[:P6]->(c:Img) RETURN a.id, b.id, c.id \
     ORDER BY a.id, b.id, c.id",
    "MATCH (a:Img)-[:P69]->(b:Img)-[:P6]->(c:Img) RETURN a.id, c.id ORDER BY a.id, c.id",
    "MATCH (a:Img)-[]->(b:Img) WHERE a.id = 5 RETURN b.id ORDER BY b.id",
    "MATCH (a:Img)-[:P926]->(b:Img) RETURN a.id, b.id ORDER BY a.id, b.id",
];

fn run_all(db: &Path) -> Vec<Vec<Vec<Value>>> {
    let store = LazyGraphStore::open(db).unwrap();
    let rt = Runtime::new(&store);
    QUERIES
        .iter()
        .map(|q| {
            let query = frogql::compile_query(q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
            match rt.run_query(&query, 0) {
                QueryResult::Projected(rows) => rows,
                other => panic!("expected a projection, got {other:?}"),
            }
        })
        .collect()
}

/// The headline invariant: what the engine answers must not depend on
/// whether the index was read or computed.
#[test]
fn loaded_index_answers_exactly_as_a_built_one() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let db = build_db("equiv");

    std::env::set_var("FROGQL_LTJ_SOURCE", "build");
    let built = run_all(&db);
    std::env::remove_var("FROGQL_LTJ_SOURCE");

    write_sidecar(&db);
    assert!(persist::path_for(&db).exists());
    let loaded = run_all(&db);

    assert_eq!(
        loaded, built,
        "a loaded index must answer exactly as a built one"
    );
    assert!(
        built.iter().any(|rows| !rows.is_empty()),
        "the fixture must actually produce rows, or this asserts nothing"
    );
}

/// The index itself round-trips: same triple count, same labels.
#[test]
fn encode_decode_round_trip() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let db = build_db("roundtrip");
    let store = LazyGraphStore::open(&db).unwrap();
    let (n, e) = counts(&db);
    let idx = TripleIndex::from_graph(&store);

    let bytes = persist::encode(&idx, n, e);
    let back = persist::decode(&bytes, n, e, TripleIndex::compact_selected()).unwrap();

    assert_eq!(back.len(), idx.len());
    assert!(!idx.is_empty(), "the fixture must have triples");
}

// ---------------------------------------------------------------------------
// Rejections. Each one must be a rebuild, never a wrong answer.
// ---------------------------------------------------------------------------

/// No sidecar is the ordinary case, and must be quiet.
#[test]
fn missing_sidecar_is_not_an_error() {
    let db = build_db("missing");
    let (n, e) = counts(&db);
    assert_eq!(
        persist::read_for(&db, n, e, false).err(),
        Some(Reject::Missing)
    );
}

/// A graph that changed under the sidecar must be refused. This is the
/// guard that matters: accepting it would answer with edges that no
/// longer exist.
#[test]
fn a_changed_graph_is_refused() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let db = build_db("stale");
    write_sidecar(&db);
    let (n, e) = counts(&db);

    // The real signal is the edge count; vary each half independently so
    // neither alone can be the thing being checked.
    assert!(matches!(
        persist::read_for(&db, n, e + 1, TripleIndex::compact_selected()),
        Err(Reject::Fingerprint { .. })
    ));
    assert!(matches!(
        persist::read_for(&db, n + 1, e, TripleIndex::compact_selected()),
        Err(Reject::Fingerprint { .. })
    ));
    assert!(persist::read_for(&db, n, e, TripleIndex::compact_selected()).is_ok());
}

/// A sidecar holding the other representation must be refused rather than
/// converted: loading it would silently change which algorithm runs.
#[test]
fn the_other_representation_is_refused() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let db = build_db("repr");
    let store = LazyGraphStore::open(&db).unwrap();
    let (n, e) = counts(&db);
    let idx = TripleIndex::from_graph(&store);
    // Whatever this process built, ask for the opposite.
    let bytes = persist::encode(&idx, n, e);
    let other = !TripleIndex::compact_selected();
    assert!(matches!(
        persist::decode(&bytes, n, e, other),
        Err(Reject::Repr { .. })
    ));
}

/// Truncation, corruption and foreign files all end in a rebuild, and
/// none of them panics. A sidecar is a cache; a damaged one is a cache
/// miss.
#[test]
fn damaged_sidecars_are_refused_without_panicking() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let db = build_db("damaged");
    write_sidecar(&db);
    let (n, e) = counts(&db);
    let path = persist::path_for(&db);
    let good = std::fs::read(&path).unwrap();
    let compact = TripleIndex::compact_selected();

    // Not one of ours.
    std::fs::write(&path, b"this is not an index at all, not even close").unwrap();
    assert_eq!(
        persist::read_for(&db, n, e, compact).err(),
        Some(Reject::BadMagic)
    );

    // Shorter than the header.
    std::fs::write(&path, &good[..8]).unwrap();
    assert_eq!(
        persist::read_for(&db, n, e, compact).err(),
        Some(Reject::TooShort)
    );

    // Header intact, payload cut off. The exact field it dies in is not
    // the point; that it reports a truncation instead of panicking is.
    std::fs::write(&path, &good[..good.len() / 2]).unwrap();
    assert!(matches!(
        persist::read_for(&db, n, e, compact),
        Err(Reject::Truncated(_))
    ));

    // A bumped version is a layout this build does not know.
    let mut wrong_version = good.clone();
    wrong_version[8] = 99;
    std::fs::write(&path, &wrong_version).unwrap();
    assert!(matches!(
        persist::read_for(&db, n, e, compact),
        Err(Reject::Version(_))
    ));

    // And the good file still loads, so the damage above was the cause.
    std::fs::write(&path, &good).unwrap();
    assert!(persist::read_for(&db, n, e, compact).is_ok());
}

/// A damaged sidecar must not stop the engine: the query still runs, off
/// a rebuilt index, with the right answer.
#[test]
fn the_engine_recovers_from_a_damaged_sidecar() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let db = build_db("recover");

    std::env::set_var("FROGQL_LTJ_SOURCE", "build");
    let want = run_all(&db);
    std::env::remove_var("FROGQL_LTJ_SOURCE");

    std::fs::write(persist::path_for(&db), b"garbage").unwrap();
    let got = run_all(&db);
    assert_eq!(got, want, "a damaged sidecar must cost a rebuild, not rows");
}

/// The sidecar sits beside the database, named after the whole file so a
/// `.gdb` and a `.gdb.vec.emb` cannot collide.
#[test]
fn sidecar_path_is_the_database_plus_a_suffix() {
    assert_eq!(
        persist::path_for(Path::new("/tmp/movies.gdb")),
        PathBuf::from("/tmp/movies.gdb.ltj")
    );
}

/// `ORDER BY` with no `LIMIT`, over a btree-indexed column, used to panic.
///
/// Found while writing the equivalence cases above, and pre-existing:
/// `try_btree_ltj_real` spells "no limit" as `cap = usize::MAX` and then
/// passed that same value to `Vec::with_capacity`, which is a capacity
/// overflow rather than an allocation. `cap` is an early-exit threshold,
/// not a size hint, and the two uses had to be separated.
///
/// It went unnoticed because the path needs all of: a btree on the sort
/// column, a query the top-k plan accepts, and no `LIMIT` — the benchmark
/// queries that exercise this plan all carry one.
#[test]
fn order_by_without_limit_does_not_overflow() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let db = build_db("orderby_nolimit");
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);

    let q = frogql::compile_query("MATCH (a:Img) RETURN a.id ORDER BY a.id").unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rows) => rows,
        other => panic!("expected a projection, got {other:?}"),
    };
    assert_eq!(rows.len(), NODES);
    // And it is actually sorted, so the fix did not trade a panic for a
    // wrong order.
    let ids: Vec<i64> = rows
        .iter()
        .map(|r| match r[0] {
            Value::Int(n) => n,
            ref other => panic!("expected an int id, got {other:?}"),
        })
        .collect();
    let mut want = ids.clone();
    want.sort_unstable();
    assert_eq!(ids, want);
}
