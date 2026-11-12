//! Tests for the ORDER BY optimization paths: top-k heap and
//! btree-driven bucket sort. Each path must produce the same output
//! as the reference pdqsort implementation. `RuntimeCfg::order_by`
//! (`FROGQL_ORDERBY_FORCE`) pins one path so the test exercises it
//! deterministically.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::graph_access::GraphAccess;
use frogql::model::value::Value;
use frogql::runtime::cfg::{OrderByForce, RuntimeCfg};
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::store::lazy::LazyGraphStore;

fn build_users(n: usize) -> MemoryGraphStore {
    let mut nodes = String::new();
    for i in 0..n {
        if i > 0 {
            nodes.push(',');
        }
        // Reverse-correlated age so insertion order ≠ sort order;
        // sprinkle a few duplicates and one node with no age to
        // exercise the null path.
        let age = if i % 17 == 0 {
            "null".to_string()
        } else {
            ((n - i) % 50).to_string()
        };
        let age_part = if age == "null" {
            String::new()
        } else {
            format!(", \"age\": {age}")
        };
        nodes.push_str(&format!(
            "{{\"id\":\"u{i}\",\"labels\":[\"User\"],\"props\":{{\"name\":\"u{i}\"{age_part}}}}}"
        ));
    }
    let json = format!("{{\"nodes\":[{nodes}],\"edges\":[]}}");
    MemoryGraphStore::from_json_str(&json).expect("graph json")
}

/// `force` is spelled as the environment variable is.
fn run_with_force<G: GraphAccess>(g: &G, q: &str, force: Option<&str>) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    rt.set_cfg(RuntimeCfg {
        order_by: force.map_or(OrderByForce::Auto, OrderByForce::parse),
        ..RuntimeCfg::default()
    });
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    }
}

#[test]
fn topk_matches_pdqsort_for_small_limit() {
    let g = build_users(200);
    let q = "MATCH (x: User) RETURN x.name, x.age ORDER BY x.age, x.name LIMIT 10";
    let pdq = run_with_force(&g, q, Some("pdqsort"));
    let topk = run_with_force(&g, q, Some("topk"));
    assert_eq!(pdq, topk, "top-k must match pdqsort row-for-row");
    assert_eq!(pdq.len(), 10);
}

#[test]
fn topk_handles_limit_larger_than_n() {
    let g = build_users(20);
    let q = "MATCH (x: User) RETURN x.name ORDER BY x.name LIMIT 9999";
    let pdq = run_with_force(&g, q, Some("pdqsort"));
    let topk = run_with_force(&g, q, Some("topk"));
    assert_eq!(pdq, topk);
    assert_eq!(pdq.len(), 20);
}

#[test]
fn topk_handles_desc_with_nulls_last() {
    let g = build_users(60);
    let q = "MATCH (x: User) RETURN x.name, x.age ORDER BY x.age DESC LIMIT 15";
    let pdq = run_with_force(&g, q, Some("pdqsort"));
    let topk = run_with_force(&g, q, Some("topk"));
    assert_eq!(pdq, topk);
}

#[test]
fn btree_path_matches_pdqsort_when_index_present() {
    // Auto-build only indexes unique-valued columns. We use MemoryGraphStore (no auto
    // build) but populate a unique-valued column anyway so the actual
    // output is unambiguous; we then force `btree-ltj` and check it
    // *gracefully falls back* to pdqsort because in-memory `MemoryGraphStore`
    // returns None from `lookup_node_ordered`. The interesting wiring
    // tested elsewhere is via LazyGraphStore.
    let mut nodes = String::new();
    for i in 0..30u32 {
        if i > 0 {
            nodes.push(',');
        }
        nodes.push_str(&format!(
            "{{\"id\":\"u{i}\",\"labels\":[\"User\"],\"props\":{{\"name\":\"u{i}\",\"age\":{}}}}}",
            (30 - i)
        ));
    }
    let json = format!("{{\"nodes\":[{nodes}],\"edges\":[]}}");
    let g = MemoryGraphStore::from_json_str(&json).unwrap();
    let q = "MATCH (x: User) RETURN x.age ORDER BY x.age LIMIT 5";
    let pdq = run_with_force(&g, q, Some("pdqsort"));
    let bt = run_with_force(&g, q, Some("btree-ltj"));
    assert_eq!(pdq, bt, "fallback to pdqsort when index absent");
}

#[test]
fn topk_zero_limit_short_circuits() {
    let g = build_users(10);
    let q = "MATCH (x: User) RETURN x.name ORDER BY x.name LIMIT 0";
    let topk = run_with_force(&g, q, Some("topk"));
    assert!(topk.is_empty());
}

#[test]
fn btree_path_emits_same_rows_as_pdqsort_via_lazy_store() {
    // LazyGraphStore auto-builds a btree index for any (label, prop)
    // whose values are unique within the label. We feed exactly that
    // shape so the btree path actually runs.
    let mut nodes = String::new();
    for i in 0..40u32 {
        if i > 0 {
            nodes.push(',');
        }
        nodes.push_str(&format!(
            "{{\"id\":\"u{i}\",\"labels\":[\"User\"],\"props\":{{\"name\":\"u{i}\",\"id\":{}}}}}",
            (40 - i)
        ));
    }
    let json = format!("{{\"nodes\":[{nodes}],\"edges\":[]}}");
    let g = MemoryGraphStore::from_json_str(&json).unwrap();
    let tmp = std::env::temp_dir().join("gqlite_orderby_optim.gdb");
    let _ = std::fs::remove_file(&tmp);
    g.save(&tmp).unwrap();
    let store = LazyGraphStore::open(&tmp).unwrap();

    let q_str = "MATCH (x: User) RETURN x.id ORDER BY x.id LIMIT 8";
    let pdq = run_with_force(&store, q_str, Some("pdqsort"));
    let bt = run_with_force(&store, q_str, Some("btree-ltj"));
    let _ = std::fs::remove_file(&tmp);

    assert_eq!(pdq, bt);
    assert_eq!(pdq.len(), 8);
    let first = match &pdq[0][0] {
        Value::Int(n) => *n,
        v => panic!("expected int, got {v:?}"),
    };
    assert_eq!(
        first, 1,
        "ASC sort starts at the smallest id (1, since ids run 1..=40)"
    );
}

#[test]
fn btree_ltj_real_matches_pdqsort_on_join_query() {
    // Build a small graph with Knows edges, unique User.id, btree
    // available. The btree-ltj-real path drives `u` from the btree
    // and expands to (u, f) pairs per pinned u via `try_ltj_with_pin`.
    // Output must match pdqsort row-for-row.
    let n: u32 = 30;
    let avg_degree: u32 = 3;
    let mut nodes = String::new();
    for i in 0..n {
        if i > 0 {
            nodes.push(',');
        }
        nodes.push_str(&format!(
            "{{\"id\":\"u{i}\",\"labels\":[\"User\"],\"props\":{{\"id\":{}}}}}",
            n - i
        ));
    }
    let mut edges = String::new();
    let mut first = true;
    for i in 0..n {
        for j in 1..=avg_degree {
            if !first {
                edges.push(',');
            }
            first = false;
            let tgt = (i + j) % n;
            edges.push_str(&format!(
                "{{\"id\":\"k{i}_{j}\",\"labels\":[\"Knows\"],\"props\":{{}},\
                  \"endpoints\":[\"u{i}\",\"u{tgt}\"],\"directionality\":\"->\"}}"
            ));
        }
    }
    let json = format!("{{\"nodes\":[{nodes}],\"edges\":[{edges}]}}");
    let g = MemoryGraphStore::from_json_str(&json).unwrap();
    let tmp = std::env::temp_dir().join("gqlite_orderby_real_join.gdb");
    let _ = std::fs::remove_file(&tmp);
    g.save(&tmp).unwrap();
    let store = LazyGraphStore::open(&tmp).unwrap();

    let q_str = "MATCH (u: User)-[:Knows]->(f: User) RETURN u.id, f.id ORDER BY u.id LIMIT 10";
    let pdq = run_with_force(&store, q_str, Some("pdqsort"));
    let real = run_with_force(&store, q_str, Some("btree-ltj-real"));
    let _ = std::fs::remove_file(&tmp);

    assert_eq!(pdq.len(), real.len(), "row counts differ");
    // Per-row: u.id (col0) must appear in non-decreasing order in both,
    // and the SET of (u.id, f.id) tuples must coincide. Within a single
    // u.id the order of f.id is implementation-dependent (peer order
    // per ISO §16.17 GR 1k), so compare as sets per u.id group.
    let mut pdq_groups: std::collections::BTreeMap<i64, std::collections::BTreeSet<i64>> =
        Default::default();
    let mut real_groups: std::collections::BTreeMap<i64, std::collections::BTreeSet<i64>> =
        Default::default();
    for row in &pdq {
        let u_id = match &row[0] {
            Value::Int(n) => *n,
            v => panic!("expected int, got {v:?}"),
        };
        let f_id = match &row[1] {
            Value::Int(n) => *n,
            v => panic!("expected int, got {v:?}"),
        };
        pdq_groups.entry(u_id).or_default().insert(f_id);
    }
    for row in &real {
        let u_id = match &row[0] {
            Value::Int(n) => *n,
            v => panic!("expected int, got {v:?}"),
        };
        let f_id = match &row[1] {
            Value::Int(n) => *n,
            v => panic!("expected int, got {v:?}"),
        };
        real_groups.entry(u_id).or_default().insert(f_id);
    }
    // Both engines return the same u.id keys with the same per-group counts.
    // ORDER BY is on u.id only, so `LIMIT 10` may cut *within* a peer group
    // (equal u.id): which f.id peers land in that truncated group is
    // implementation-defined (ISO §16.17 GR 1k), and pdqsort vs btree-ltj-real
    // legitimately pick different peers there. Only the boundary group — the
    // largest u.id present, the one LIMIT can truncate — is allowed to differ,
    // and only in *which* peers, not how many. Every fully-included group must
    // match exactly.
    assert_eq!(
        pdq_groups.keys().collect::<Vec<_>>(),
        real_groups.keys().collect::<Vec<_>>(),
        "u.id keys differ"
    );
    let boundary = pdq_groups.keys().next_back().copied();
    for (u_id, pdq_fs) in &pdq_groups {
        let real_fs = &real_groups[u_id];
        if Some(*u_id) == boundary {
            assert_eq!(
                pdq_fs.len(),
                real_fs.len(),
                "boundary group u.id={u_id} size differs (pdq={pdq_fs:?} real={real_fs:?})"
            );
        } else {
            assert_eq!(pdq_fs, real_fs, "u.id group {u_id} differs");
        }
    }
}

#[test]
fn btree_ltj_real_bails_on_single_node_query() {
    // No edges → LTJ doesn't fire → btree-ltj-real precondition fails →
    // routes to pdqsort. Output must match pdqsort exactly (no
    // regression even though force=btree-ltj-real is set).
    let g = build_users(20);
    let q = "MATCH (x: User) RETURN x.name ORDER BY x.name LIMIT 5";
    let pdq = run_with_force(&g, q, Some("pdqsort"));
    let real = run_with_force(&g, q, Some("btree-ltj-real"));
    assert_eq!(pdq, real);
}

/// Ten `:Person {id}` and 200 `:Comment {date}`, comment `i` created by
/// person `i % 10`. Both properties are unique within their label, so the
/// lazy store auto-builds a hash and a btree on each: `(:Person {id: _})`
/// is an anchor and `c.date` can drive the btree walk.
fn creators_store(file: &str) -> (LazyGraphStore, std::path::PathBuf) {
    let mut nodes = Vec::new();
    for i in 0..10 {
        nodes.push(format!(
            "{{\"id\":\"p{i}\",\"labels\":[\"Person\"],\"props\":{{\"id\":{i}}}}}"
        ));
    }
    let mut edges = Vec::new();
    for i in 0..200 {
        nodes.push(format!(
            "{{\"id\":\"c{i}\",\"labels\":[\"Comment\"],\"props\":{{\"date\":{}}}}}",
            1000 + i
        ));
        edges.push(format!(
            "{{\"id\":\"h{i}\",\"labels\":[\"hasCreator\"],\"props\":{{}},\
              \"endpoints\":[\"c{i}\",\"p{}\"],\"directionality\":\"->\"}}",
            i % 10
        ));
    }
    let json = format!(
        "{{\"nodes\":[{}],\"edges\":[{}]}}",
        nodes.join(","),
        edges.join(",")
    );
    let g = MemoryGraphStore::from_json_str(&json).unwrap();
    let tmp = std::env::temp_dir().join(file);
    let _ = std::fs::remove_file(&tmp);
    g.save(&tmp).unwrap();
    (LazyGraphStore::open(&tmp).unwrap(), tmp)
}

/// The rows, and whether the ORDER BY was answered by the btree walk.
fn run_and_walk(store: &LazyGraphStore, q: &str, force: OrderByForce) -> (Vec<Vec<Value>>, bool) {
    let rt = Runtime::new(store);
    rt.set_cfg(RuntimeCfg {
        order_by: force,
        ..RuntimeCfg::default()
    });
    let rows = match rt.run_query(&compile_query(q).unwrap(), 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    (rows, rt.last_query_walked_btree())
}

/// An anchored pattern is answered by the join, not the btree walk: the
/// anchor confines the answer to one person's comments, sparse among all
/// comments, which is where the walk probes longest (LDBC IC8: 674 ms
/// walking, 9 ms joining). The answer must not depend on which ran.
#[test]
fn btree_walk_declines_anchored_pattern() {
    let (store, tmp) = creators_store("frogql_orderby_anchored.gdb");
    let q = "MATCH (p:Person {id: 3})<-[:hasCreator]-(c:Comment) \
             RETURN c.date ORDER BY c.date DESC LIMIT 5";
    let (auto, auto_walked) = run_and_walk(&store, q, OrderByForce::Auto);
    let (btree, btree_walked) = run_and_walk(&store, q, OrderByForce::Btree);
    let (pdq, _) = run_and_walk(&store, q, OrderByForce::Pdqsort);
    let _ = std::fs::remove_file(&tmp);

    assert!(
        !auto_walked,
        "the anchored pattern must not take the btree walk"
    );
    assert!(
        btree_walked,
        "FROGQL_ORDERBY_FORCE=btree must still take it"
    );
    assert_eq!(auto, pdq);
    assert_eq!(btree, pdq);
    // Person 3 created comments 3, 13, …, 193: the five latest.
    let dates: Vec<Value> = [1193, 1183, 1173, 1163, 1153]
        .into_iter()
        .map(Value::Int)
        .collect();
    assert_eq!(
        auto.into_iter().map(|r| r[0].clone()).collect::<Vec<_>>(),
        dates
    );
}

/// Without an anchor the answer spreads over every comment, the walk
/// stops after about `k` probes, and it is the strategy that must run
/// (LDBC SF0.1: 12 ms walking, 803 ms joining and sorting).
#[test]
fn btree_walk_still_runs_without_anchor() {
    let (store, tmp) = creators_store("frogql_orderby_unanchored.gdb");
    let q = "MATCH (p:Person)<-[:hasCreator]-(c:Comment) \
             RETURN c.date, p.id ORDER BY c.date DESC LIMIT 5";
    let (auto, auto_walked) = run_and_walk(&store, q, OrderByForce::Auto);
    let (pdq, _) = run_and_walk(&store, q, OrderByForce::Pdqsort);
    let _ = std::fs::remove_file(&tmp);

    assert!(
        auto_walked,
        "an unanchored pattern must keep the btree walk"
    );
    assert_eq!(auto, pdq);
    assert_eq!(auto[0], vec![Value::Int(1199), Value::Int(9)]);
}

/// An equality no index answers is not an anchor: no person carries
/// `name`, so there is no index on `(Person, name)` to narrow `p`, and
/// the walk runs as before.
#[test]
fn unindexed_equality_is_not_an_anchor() {
    let (store, tmp) = creators_store("frogql_orderby_unindexed.gdb");
    let q = "MATCH (p:Person)<-[:hasCreator]-(c:Comment) WHERE p.name = 'x' \
             RETURN c.date ORDER BY c.date DESC LIMIT 5";
    let (auto, auto_walked) = run_and_walk(&store, q, OrderByForce::Auto);
    let (pdq, _) = run_and_walk(&store, q, OrderByForce::Pdqsort);
    let _ = std::fs::remove_file(&tmp);

    assert!(
        auto_walked,
        "no index answers p.name, so nothing anchors the pattern"
    );
    assert_eq!(auto, pdq);
    assert!(auto.is_empty());
}

#[test]
fn topk_with_distinct_keeps_full_sort() {
    // DISTINCT can shrink the post-projection set; if top-k cut at the
    // pre-projection level we'd miss rows that survive dedup. The
    // engine routes sort_limit = 0 in that case.
    let mut json = String::from("{\"nodes\":[");
    for i in 0..20 {
        if i > 0 {
            json.push(',');
        }
        // Half the rows duplicate `category` so DISTINCT halves them.
        json.push_str(&format!(
            "{{\"id\":\"u{i}\",\"labels\":[\"User\"],\"props\":{{\"category\":{}}}}}",
            i / 2
        ));
    }
    json.push_str("],\"edges\":[]}");
    let g = MemoryGraphStore::from_json_str(&json).unwrap();
    let q = "MATCH (x: User) RETURN DISTINCT x.category ORDER BY x.category LIMIT 5";
    let pdq = run_with_force(&g, q, Some("pdqsort"));
    let topk = run_with_force(&g, q, Some("topk"));
    assert_eq!(pdq, topk);
    assert_eq!(pdq.len(), 5);
}
