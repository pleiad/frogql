//! A DM statement must not rebuild the LTJ index.
//!
//! The `TripleIndex` is cached on the **`Runtime`**
//! (`RefCell<Option<Arc<TripleIndex>>>`), not on the store, and
//! `Runtime::new` starts it empty. `run_dm` built a `Runtime` of its own,
//! so it started cold — and a DM's MATCH chain is an ordinary pattern, so a
//! comma-join in it takes the ordinary LTJ path and asks for the index.
//! Every such statement therefore rebuilt all six orderings from the graph.
//!
//! The `<db>.ltj` sidecar does not rescue it: `index_sidecar_key` returns
//! `None` once the overlay holds a triple-affecting mutation, so from the
//! first insert onwards the file is refused for the rest of the session and
//! the cold `Runtime` has to build from the graph. That is what made the
//! cost **flat** — proportional to the base graph, not to what had been
//! inserted.
//!
//! Measured on 26 178 nodes / 90 132 edges, one
//! `MATCH (f:Fpl {…}), (a:Aerodromo {…}) INSERT (f)-[:R]->(a)` cost
//! **118 ms**, against 0.0 ms with a single-pattern MATCH and 0.02 ms for
//! the same comma-join as a plain query. The edge was incidental: a
//! comma-join with a *node* insert cost the same, and a standalone
//! `INSERT (:A)-[:R]->(:B)` cost nothing.
//!
//! The guard is a **ratio against the same join run as a query**, not a
//! wall-clock threshold, so it does not depend on how loaded the machine
//! is. With the index shared the two are within noise of each other; with
//! a rebuild per statement the DM is three orders of magnitude slower, so
//! a 25× bound has room to spare and still fails loudly on a regression.

use std::time::Instant;

use frogql::model::graph::MemoryGraphStore;
use frogql::parser::parse_statement;
use frogql::runtime::dm::run_dm_with_index;
use frogql::runtime::engine::Runtime;
use frogql::store::lazy::LazyGraphStore;
use frogql::syntax::statement::Statement;

/// Enough edges that a full six-ordering rebuild is unmistakably more
/// expensive than a point-looked-up join, and few enough to stay fast.
const NODES: usize = 4_000;

fn fixture_json() -> String {
    let mut nodes = Vec::with_capacity(NODES * 2);
    let mut edges = Vec::with_capacity(NODES);
    for i in 0..NODES {
        nodes.push(format!(
            r#"{{"id":"f{i}","labels":["Fpl"],"props":{{"fplId":{i}}}}}"#
        ));
        nodes.push(format!(
            r#"{{"id":"a{i}","labels":["Aero"],"props":{{"oaci":"A{i}"}}}}"#
        ));
        edges.push(format!(
            r#"{{"id":"e{i}","labels":["SALE_DE"],"props":{{}},"endpoints":["f{i}","a{i}"],"directionality":"->"}}"#
        ));
    }
    format!(
        r#"{{"nodes":[{}],"edges":[{}]}}"#,
        nodes.join(","),
        edges.join(",")
    )
}

fn open_db(name: &str) -> (LazyGraphStore, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("frogql_dm_shares_index_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");
    MemoryGraphStore::from_json_str(&fixture_json())
        .unwrap()
        .save(&db)
        .unwrap();
    (LazyGraphStore::open(&db).unwrap(), db)
}

const JOIN: &str = "MATCH (f:Fpl {fplId: 7}), (a:Aero {oaci: 'A9'})";

#[test]
fn a_dm_comma_join_reuses_the_session_index() {
    let (store, _p) = open_db("ratio");
    let rt = Runtime::new(&store);
    let idx = rt.warm_triple_index();

    // Baseline: the identical join, as a query, against the warm index.
    let q = frogql::compile_query_unchecked(&format!("{JOIN} RETURN f.fplId")).unwrap();
    let warm = Runtime::with_triple_index(&store, idx.clone());
    let _ = warm.run_query(&q, 0); // pay any one-off cost first
    let t = Instant::now();
    for _ in 0..5 {
        let _ = warm.run_query(&q, 0);
    }
    let query_ns = t.elapsed().as_nanos().max(1);

    // The same join inside a DM, handed the same index.
    let dm = match parse_statement(&format!("{JOIN} INSERT (:Tmp {{n: 1}})")).unwrap() {
        Statement::DataModification(dm) => dm,
        other => panic!("expected a DM, got {other:?}"),
    };
    let t = Instant::now();
    for _ in 0..5 {
        let exec = run_dm_with_index(&store, &dm, None, Some(idx.clone())).expect("runs");
        assert_eq!(exec.nodes_inserted, 1, "the join must bind exactly one row");
    }
    let dm_ns = t.elapsed().as_nanos().max(1);

    let ratio = dm_ns as f64 / query_ns as f64;
    assert!(
        ratio < 25.0,
        "a DM comma-join cost {ratio:.1}× the same join as a query \
         ({dm_ns} ns vs {query_ns} ns over 5 runs each). That is the signature \
         of run_dm building its own TripleIndex instead of using the one it \
         was handed — see run_dm_with_index."
    );
}

/// Sharing the index must not change what the DM does.
#[test]
fn sharing_the_index_does_not_change_the_result() {
    let (store, _p) = open_db("same");
    let rt = Runtime::new(&store);
    let idx = rt.warm_triple_index();

    let dm = match parse_statement(&format!("{JOIN} INSERT (:Tmp {{n: 1}})")).unwrap() {
        Statement::DataModification(dm) => dm,
        other => panic!("expected a DM, got {other:?}"),
    };

    let shared = run_dm_with_index(&store, &dm, None, Some(idx)).expect("runs");
    let cold = run_dm_with_index(&store, &dm, None, None).expect("runs");
    assert_eq!(shared.nodes_inserted, cold.nodes_inserted);
    assert_eq!(shared.edges_inserted, cold.edges_inserted);
    assert_eq!(shared.rows.len(), cold.rows.len());
}
