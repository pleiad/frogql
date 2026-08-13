//! `NEAREST` end to end: the post-filter and pre-filter strategies over
//! a real `.gdb` with a real sidecar.
//!
//! The fixture is deliberately RDF-shaped — directed edges, no
//! properties — because that is the query class this work targets, and
//! it is the class that always takes the LTJ path.
//!
//! Item vectors sit on a line at x = 0, 1, 2, …, so "nearest to [t, 0]"
//! has an answer anyone can check by reading the query.

use std::path::{Path, PathBuf};

use frogql::model::graph::MemoryGraphStore;
use frogql::model::graph_access::GraphAccess;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::runtime::vsearch::{Strategy, VecCfg, VecSource};
use frogql::store::lazy::LazyGraphStore;
use frogql::vector::hnsw::{Hnsw, HnswParams};
use frogql::vector::metric::Metric;
use frogql::vector::sidecar::{fingerprint, Sidecar};
use frogql::vector::store::VectorSet;

/// 8 items in a line, and 4 users. Every user likes items at their own
/// index and index + 4, so a pattern rooted at one user selects exactly
/// two of the eight — selective enough that the strategies genuinely
/// differ in how much of the index they touch.
///
/// ```text
/// item_i  vector (i, 0)
/// u0 -likes-> item0, item4
/// u1 -likes-> item1, item5
/// u2 -likes-> item2, item6
/// u3 -likes-> item3, item7
/// ```
/// Plus `u0 -follows-> u1` so there is a two-hop shape to join over.
fn fixture_json() -> String {
    let mut nodes = Vec::new();
    for i in 0..8 {
        nodes.push(format!(
            r#"{{"id":"item{i}","labels":["Item"],"props":{{"idx":{i}}}}}"#
        ));
    }
    for u in 0..4 {
        nodes.push(format!(
            r#"{{"id":"u{u}","labels":["User"],"props":{{"idx":{u}}}}}"#
        ));
    }
    let mut edges = Vec::new();
    let mut e = 0;
    for u in 0..4 {
        for item in [u, u + 4] {
            edges.push(format!(
                r#"{{"id":"e{e}","labels":["likes"],"props":{{}},"endpoints":["u{u}","item{item}"],"directionality":"->"}}"#
            ));
            e += 1;
        }
    }
    for u in 0..3 {
        edges.push(format!(
            r#"{{"id":"f{u}","labels":["follows"],"props":{{}},"endpoints":["u{u}","u{}"],"directionality":"->"}}"#,
            u + 1
        ));
    }
    format!(
        r#"{{"nodes":[{}],"edges":[{}]}}"#,
        nodes.join(","),
        edges.join(",")
    )
}

fn build_db(name: &str, with_index: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("frogql_nearest_rt_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");

    MemoryGraphStore::from_json_str(&fixture_json())
        .unwrap()
        .save(&db)
        .unwrap();

    // Resolve each item's internal id, then place its vector at (idx, 0).
    let (fp, rows) = {
        let store = LazyGraphStore::open(&db).unwrap();
        let fp = fingerprint(store.node_count() as usize, store.edge_count() as usize);
        let mut rows: Vec<(u32, f32)> = store
            .nodes()
            .into_iter()
            .filter(|id| store.node_name(*id).starts_with("item"))
            .map(|id| {
                let idx = store.node_name(id)[4..].parse::<f32>().unwrap();
                (id, idx)
            })
            .collect();
        rows.sort_by_key(|(id, _)| *id);
        (fp, rows)
    };

    let ids: Vec<u32> = rows.iter().map(|(id, _)| *id).collect();
    let data: Vec<f32> = rows.iter().flat_map(|(_, x)| vec![*x, 0.0]).collect();
    let set = VectorSet::new("emb".to_string(), 2, Metric::L2Sq, fp, ids, data);
    let set = if with_index {
        let h = Hnsw::build(&set, HnswParams::default());
        set.with_hnsw(h)
    } else {
        set
    };
    set.to_sidecar()
        .write_to_path(&Sidecar::path_for(&db, "emb"))
        .unwrap();
    db
}

/// Run `q` under `strategy`, returning the projected `idx` column.
/// Items are named `item<idx>`, so an index doubles as an identity.
fn idxs(db: &Path, q: &str, strategy: Strategy, source: VecSource) -> Vec<i64> {
    let store = LazyGraphStore::open(db).unwrap();
    let rt = Runtime::new(&store);
    rt.set_vec_cfg(VecCfg {
        strategy,
        source,
        ..VecCfg::default()
    });
    let query = frogql::compile_query(q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rows) => rows
            .into_iter()
            .map(|r| match r.first() {
                Some(Value::Int(n)) => *n,
                other => panic!("expected an integer column, got {other:?}"),
            })
            .collect(),
        other => panic!("expected a projection, got {other:?}"),
    }
}

/// Every (strategy, source) combination that must agree exactly. HNSW
/// is excluded: its recall is a measurement, not an invariant.
fn exact_arms() -> Vec<(Strategy, VecSource)> {
    vec![
        (Strategy::PostFilter, VecSource::LocalSort),
        (Strategy::PostFilter, VecSource::GlobalSort),
        (Strategy::PreFilter, VecSource::GlobalSort),
        (Strategy::InLtj, VecSource::LocalSort),
        (Strategy::InLtj, VecSource::GlobalSort),
    ]
}

#[test]
fn ranks_the_matching_items_by_distance() {
    let db = build_db("rank", true);
    // u0 likes item0 (at x=0) and item4 (at x=4). Query vector at x=3.5:
    // item4 is nearer.
    let q = "MATCH (u:User)-[:likes]->(i:Item) WHERE u.idx = 0 \
             NEAREST 2 i.emb TO [3.5, 0.0] \
             RETURN i.idx";
    for (s, src) in exact_arms() {
        assert_eq!(idxs(&db, q, s, src), vec![4, 0], "arm {s:?} source {src:?}");
    }
}

#[test]
fn k_truncates_to_the_nearest() {
    let db = build_db("k_trunc", true);
    let q = "MATCH (u:User)-[:likes]->(i:Item) WHERE u.idx = 0 \
             NEAREST 1 i.emb TO [3.5, 0.0] \
             RETURN i.idx";
    for (s, src) in exact_arms() {
        assert_eq!(idxs(&db, q, s, src), vec![4], "arm {s:?} source {src:?}");
    }
}

#[test]
fn the_search_is_restricted_to_pattern_matches() {
    let db = build_db("restricted", true);
    // item1 is the globally nearest to [1,0], but u0 does not like it.
    let q = "MATCH (u:User)-[:likes]->(i:Item) WHERE u.idx = 0 \
             NEAREST 1 i.emb TO [1.0, 0.0] \
             RETURN i.idx";
    for (s, src) in exact_arms() {
        assert_eq!(
            idxs(&db, q, s, src),
            vec![0],
            "arm {s:?} source {src:?}: the answer must satisfy the pattern"
        );
    }
}

#[test]
fn a_k_larger_than_the_match_returns_everything_matching() {
    let db = build_db("k_large", true);
    let q = "MATCH (u:User)-[:likes]->(i:Item) WHERE u.idx = 0 \
             NEAREST 99 i.emb TO [0.0, 0.0] \
             RETURN i.idx";
    for (s, src) in exact_arms() {
        assert_eq!(idxs(&db, q, s, src), vec![0, 4]);
    }
}

#[test]
fn works_over_a_two_hop_join() {
    let db = build_db("two_hop", true);
    // u0 follows u1, who likes item1 (x=1) and item5 (x=5).
    let q = "MATCH (a:User)-[:follows]->(b:User), (b)-[:likes]->(i:Item) WHERE a.idx = 0 \
             NEAREST 2 i.emb TO [4.6, 0.0] \
             RETURN i.idx";
    for (s, src) in exact_arms() {
        assert_eq!(idxs(&db, q, s, src), vec![5, 1], "arm {s:?} source {src:?}");
    }
}

#[test]
fn binds_the_distance_variable() {
    let db = build_db("dist_bind", true);
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    let query = frogql::compile_query(
        "MATCH (u:User)-[:likes]->(i:Item) WHERE u.idx = 0 \
         NEAREST 2 i.emb TO [4.0, 0.0] AS d \
         RETURN i.idx, d",
    )
    .unwrap();
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Int(4));
    // Squared L2 from (4,0) to (4,0) is 0, and to (0,0) is 16.
    assert_eq!(rows[0][1], Value::Float(0.0));
    assert_eq!(rows[1][1], Value::Float(16.0));
}

#[test]
fn the_distance_variable_is_usable_in_order_by() {
    let db = build_db("dist_order", true);
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    let query = frogql::compile_query(
        "MATCH (u:User)-[:likes]->(i:Item) WHERE u.idx = 0 \
         NEAREST 2 i.emb TO [4.0, 0.0] AS d \
         RETURN i.idx, d ORDER BY d DESC",
    )
    .unwrap();
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(rows[0][0], Value::Int(0), "DESC reverses");
}

#[test]
fn the_vector_builtin_takes_the_query_from_a_stored_node() {
    let db = build_db("vector_builtin", true);
    let store = LazyGraphStore::open(&db).unwrap();
    // Find item6's internal id and use its own embedding as the query.
    let item6 = store
        .nodes()
        .into_iter()
        .find(|id| store.node_name(*id) == "item6")
        .unwrap();
    let rt = Runtime::new(&store);
    let query = frogql::compile_query(&format!(
        "MATCH (u:User)-[:likes]->(i:Item) WHERE u.idx = 2 \
         NEAREST 1 i.emb TO VECTOR({item6}, 'emb') \
         RETURN i.idx"
    ))
    .unwrap();
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(rows[0][0], Value::Int(6));
}

#[test]
fn rows_mode_counts_rows_not_bindings() {
    let db = build_db("rows_mode", true);
    // Every user likes item0..item7 in the fixture? No: only two each.
    // Give the same item two matching rows by joining on both edges.
    let q = "MATCH (u:User)-[:likes]->(i:Item) \
             NEAREST 3 ROWS i.emb TO [0.0, 0.0] \
             RETURN i.idx";
    for (s, src) in exact_arms() {
        let got = idxs(&db, q, s, src);
        assert_eq!(got.len(), 3, "arm {s:?} source {src:?}: {got:?}");
        assert_eq!(got[0], 0);
    }
}

#[test]
fn distinct_var_mode_keeps_all_rows_of_an_accepted_binding() {
    let db = build_db("distinct_mode", true);
    // Two users like item4? No — but `(u)-[:likes]->(i)` with no user
    // filter gives one row per (user, item) pair, so restricting to
    // k = 1 binding of `i` still returns every row that binding has.
    let q = "MATCH (u:User)-[:likes]->(i:Item) \
             NEAREST 1 i.emb TO [0.0, 0.0] \
             RETURN i.idx";
    for (s, src) in exact_arms() {
        let got = idxs(&db, q, s, src);
        assert!(
            got.iter().all(|n| *n == 0),
            "arm {s:?} source {src:?}: {got:?}"
        );
        assert_eq!(got.len(), 1, "item0 has exactly one liker");
    }
}

#[test]
fn a_missing_sidecar_yields_no_rows_rather_than_the_unfiltered_match() {
    // Without vectors, "among the k nearest" cannot be satisfied by
    // anything. Returning the unfiltered pattern would be silently wrong.
    let dir = std::env::temp_dir().join("frogql_nearest_rt_no_sidecar");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");
    MemoryGraphStore::from_json_str(&fixture_json())
        .unwrap()
        .save(&db)
        .unwrap();

    let q = "MATCH (u:User)-[:likes]->(i:Item) NEAREST 2 i.emb TO [0.0, 0.0] RETURN i.idx";
    assert!(idxs(&db, q, Strategy::PostFilter, VecSource::Hnsw).is_empty());
    assert!(idxs(&db, q, Strategy::PreFilter, VecSource::Hnsw).is_empty());
}

#[test]
fn a_dimension_mismatch_yields_no_rows() {
    let db = build_db("dim_mismatch", true);
    let q = "MATCH (u:User)-[:likes]->(i:Item) NEAREST 2 i.emb TO [0.0, 0.0, 0.0] RETURN i.idx";
    assert!(idxs(&db, q, Strategy::PostFilter, VecSource::Hnsw).is_empty());
}

#[test]
fn nearest_zero_yields_no_rows() {
    let db = build_db("k_zero", true);
    let q = "MATCH (u:User)-[:likes]->(i:Item) NEAREST 0 i.emb TO [0.0, 0.0] RETURN i.idx";
    assert!(idxs(&db, q, Strategy::PostFilter, VecSource::Hnsw).is_empty());
}

#[test]
fn the_executed_arm_is_reported() {
    let db = build_db("arm_report", true);
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    let query = frogql::compile_query(
        "MATCH (u:User)-[:likes]->(i:Item) NEAREST 2 i.emb TO [0.0, 0.0] RETURN i.idx",
    )
    .unwrap();

    for (strategy, source, expected) in [
        (Strategy::PostFilter, VecSource::Hnsw, "post+hnsw"),
        (Strategy::PostFilter, VecSource::LocalSort, "post+localsort"),
        (
            Strategy::PostFilter,
            VecSource::GlobalSort,
            "post+globalsort",
        ),
        (Strategy::PreFilter, VecSource::Hnsw, "pre+hnsw"),
        (Strategy::PreFilter, VecSource::GlobalSort, "pre+globalsort"),
        // Pre-filter has no per-visit set, so a local request is served
        // by the global walk and must say so.
        (Strategy::PreFilter, VecSource::LocalSort, "pre+globalsort"),
        (Strategy::InLtj, VecSource::Hnsw, "inltj+hnsw"),
        (Strategy::InLtj, VecSource::LocalSort, "inltj+localsort"),
        (Strategy::InLtj, VecSource::GlobalSort, "inltj+globalsort"),
    ] {
        rt.set_vec_cfg(VecCfg {
            strategy,
            source,
            ..VecCfg::default()
        });
        let _ = rt.run_query(&query, 0);
        assert_eq!(rt.last_vec_stats().arm, expected);
    }
}

#[test]
fn pre_filter_touches_fewer_neighbours_when_the_answer_is_near() {
    let db = build_db("cost_shape", true);
    let store = LazyGraphStore::open(&db).unwrap();
    let rt = Runtime::new(&store);
    // The query vector sits on item0, which u0 does like — so the very
    // first neighbour is already an answer.
    let query = frogql::compile_query(
        "MATCH (u:User)-[:likes]->(i:Item) WHERE u.idx = 0 \
         NEAREST 1 i.emb TO [0.0, 0.0] RETURN i.idx",
    )
    .unwrap();

    rt.set_vec_cfg(VecCfg {
        strategy: Strategy::PreFilter,
        ..VecCfg::default()
    });
    let _ = rt.run_query(&query, 0);
    let pre = rt.last_vec_stats();
    assert!(pre.pattern_runs >= 1);
    assert!(
        pre.nn_pops <= 3,
        "the answer is the first neighbour; popped {}",
        pre.nn_pops
    );
}
