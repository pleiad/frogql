//! The wall-clock budget: what it bounds, what it does not, and the fact
//! that its output is a partial result rather than an answer.
//!
//! The fixture is a deliberately expensive shape — a corpus-walking
//! vector search over a pattern that is a sliver of it, which is the
//! combination that motivated the budget: on the IMGpedia dump
//! `interleave+hnsw` at a deep level never finished, and there was no way
//! to tell that apart from "slow".

use std::path::{Path, PathBuf};
use std::time::Duration;

use frogql::model::graph::MemoryGraphStore;
use frogql::model::graph_access::GraphAccess;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::runtime::vsearch::{Strategy, VecCfg, VecSource};
use frogql::store::lazy::LazyGraphStore;
use frogql::vector::metric::Metric;
use frogql::vector::sidecar::{fingerprint, Sidecar};
use frogql::vector::store::VectorSet;

const DIM: usize = 8;
const ANCHORS: usize = 12;
const MIDS: usize = 20;
const IMGS: usize = 200;
const ORPHANS: usize = 20000;

fn spread(seed: u64, d: usize) -> f32 {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(d as u64 * 1442695040888963407);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    ((x % 100_000) as f32) / 1000.0
}

fn build(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("frogql_budget_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");

    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut e = 0usize;
    for a in 0..ANCHORS {
        nodes.push(format!(
            r#"{{"id":"a{a}","labels":["Anchor","Img"],"props":{{"idx":{a}}}}}"#
        ));
    }
    for m in 0..MIDS {
        nodes.push(format!(
            r#"{{"id":"m{m}","labels":["Mid"],"props":{{"idx":{m}}}}}"#
        ));
    }
    for i in 0..IMGS {
        nodes.push(format!(
            r#"{{"id":"v{i}","labels":["Img"],"props":{{"idx":{i}}}}}"#
        ));
    }
    for o in 0..ORPHANS {
        nodes.push(format!(
            r#"{{"id":"orphan{o}","labels":["Img"],"props":{{"idx":{o}}}}}"#
        ));
    }
    for a in 0..ANCHORS {
        for m in 0..MIDS {
            edges.push(format!(
                r#"{{"id":"e{e}","labels":["R"],"props":{{}},"endpoints":["a{a}","m{m}"],"directionality":"->"}}"#
            ));
            e += 1;
        }
    }
    for m in 0..MIDS {
        for i in 0..IMGS {
            if (i + m) % 3 == 0 {
                edges.push(format!(
                    r#"{{"id":"e{e}","labels":["R"],"props":{{}},"endpoints":["m{m}","v{i}"],"directionality":"->"}}"#
                ));
                e += 1;
            }
        }
    }
    let json = format!(
        "{{\"nodes\":[{}],\"edges\":[{}]}}",
        nodes.join(","),
        edges.join(",")
    );
    MemoryGraphStore::from_json_str(&json)
        .unwrap()
        .save(&db)
        .unwrap();

    let store = LazyGraphStore::open(&db).unwrap();
    let fp = fingerprint(store.node_count() as usize, store.edge_count() as usize);
    let mut rows: Vec<(u32, u64)> = Vec::new();
    for id in store.nodes() {
        let n = store.node_name(id).to_string();
        let seed = if let Some(i) = n.strip_prefix("a").and_then(|s| s.parse::<u64>().ok()) {
            Some(i)
        } else if let Some(i) = n.strip_prefix("v").and_then(|s| s.parse::<u64>().ok()) {
            Some(1_000 + i)
        } else {
            n.strip_prefix("orphan")
                .and_then(|s| s.parse::<u64>().ok())
                .map(|i| 1_000_000 + i)
        };
        if let Some(s) = seed {
            rows.push((id, s));
        }
    }
    rows.sort_by_key(|(id, _)| *id);
    let ids: Vec<u32> = rows.iter().map(|(id, _)| *id).collect();
    let data: Vec<f32> = rows
        .iter()
        .flat_map(|(_, s)| (0..DIM).map(move |d| spread(*s, d)))
        .collect();
    VectorSet::new("emb".to_string(), DIM, Metric::L2Sq, fp, ids, data)
        .to_sidecar()
        .write_to_path(&Sidecar::path_for(&db, "emb"))
        .unwrap();
    db
}

const Q: &str = "MATCH (a:Anchor)-[:R]->(m:Mid), (m)-[:R]->(v:Img) \
     NEAREST 50 v.emb TO VECTOR(a, 'emb') AS d RETURN a.idx, v.idx, d";

fn run(db: &Path, strategy: Strategy, budget: Option<Duration>) -> (usize, bool) {
    let store = LazyGraphStore::open(db).unwrap();
    let rt = Runtime::new(&store);
    rt.set_query_budget(budget);
    rt.set_vec_cfg(VecCfg {
        strategy,
        source: VecSource::GlobalSort,
        level: 1,
        ..VecCfg::default()
    });
    let q = frogql::compile_query(Q).unwrap();
    let rows = match rt.run_query(&q, 0) {
        QueryResult::Projected(rows) => rows.len(),
        other => panic!("expected a projection, got {other:?}"),
    };
    (rows, rt.query_timed_out())
}

/// No budget, no report. The default must cost nothing and claim nothing.
#[test]
fn an_unbudgeted_query_never_reports_a_timeout() {
    let db = build("none");
    let (rows, timed_out) = run(&db, Strategy::Memo, None);
    assert!(rows > 0, "the fixture must produce rows");
    assert!(!timed_out, "no budget was set, so nothing can have expired");
}

/// A budget too small to finish in stops the search and says so. The rows
/// that come back are a partial result, which is why the flag exists —
/// without it they are indistinguishable from an answer.
#[test]
fn an_exhausted_budget_stops_the_search_and_reports_it() {
    let db = build("tiny");
    let full = run(&db, Strategy::Interleave, None);
    assert!(!full.1);

    let (rows, timed_out) = run(&db, Strategy::Interleave, Some(Duration::from_millis(1)));
    assert!(timed_out, "a 1 ms budget cannot cover this query");
    assert!(
        rows < full.0,
        "an abandoned search must return less than a finished one: {rows} against {}",
        full.0
    );
}

/// A budget larger than the query needs changes nothing at all — neither
/// the answer nor the verdict.
#[test]
fn a_generous_budget_leaves_the_answer_alone() {
    let db = build("generous");
    let (want, _) = run(&db, Strategy::Memo, None);
    let (got, timed_out) = run(&db, Strategy::Memo, Some(Duration::from_secs(600)));
    assert!(!timed_out, "ten minutes is not a bound on this query");
    assert_eq!(got, want, "a budget that never expires must not be visible");
}

/// The verdict is per execution, not per session: one query running out
/// must not condemn the next.
#[test]
fn the_verdict_resets_between_queries() {
    let db = build("reset");
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    rt.set_vec_cfg(VecCfg {
        strategy: Strategy::Interleave,
        source: VecSource::GlobalSort,
        level: 1,
        ..VecCfg::default()
    });
    let q = frogql::compile_query(Q).unwrap();

    rt.set_query_budget(Some(Duration::from_millis(1)));
    let _ = rt.run_query(&q, 0);
    assert!(rt.query_timed_out());

    rt.set_query_budget(Some(Duration::from_secs(600)));
    let _ = rt.run_query(&q, 0);
    assert!(
        !rt.query_timed_out(),
        "the previous query's verdict must not carry over"
    );
}

/// A trivial query under a generous budget must not be slowed by the
/// clock reads. Not a benchmark — just the guarantee that the check is
/// amortised rather than per candidate.
#[test]
fn the_budget_check_does_not_dominate_a_small_query() {
    let db = build("cheap");
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    let q = frogql::compile_query("MATCH (a:Anchor)-[:R]->(m:Mid) RETURN a.idx, m.idx").unwrap();

    rt.set_query_budget(None);
    let t = std::time::Instant::now();
    let _ = rt.run_query(&q, 0);
    let unbounded = t.elapsed();

    rt.set_query_budget(Some(Duration::from_secs(600)));
    let t = std::time::Instant::now();
    let _ = rt.run_query(&q, 0);
    let bounded = t.elapsed();

    assert!(!rt.query_timed_out());
    assert!(
        bounded < unbounded * 8 + Duration::from_millis(50),
        "the budget check must be amortised: {bounded:?} against {unbounded:?}"
    );
}
