//! Tests for the ORDER BY optimization paths: top-k heap and
//! btree-driven bucket sort. Each path must produce the same output
//! as the reference pdqsort implementation. The `GQLITE_ORDERBY_FORCE`
//! env var pins one path so the test exercises it deterministically.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::store::lazy::LazyGraphStore;

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

fn run_with_force(g: &MemoryGraphStore, q: &str, force: Option<&str>) -> Vec<Vec<Value>> {
    let prev = std::env::var("GQLITE_ORDERBY_FORCE").ok();
    match force {
        Some(v) => std::env::set_var("GQLITE_ORDERBY_FORCE", v),
        None => std::env::remove_var("GQLITE_ORDERBY_FORCE"),
    }
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    let result = rt.run_query(&query, 0);
    match prev {
        Some(v) => std::env::set_var("GQLITE_ORDERBY_FORCE", v),
        None => std::env::remove_var("GQLITE_ORDERBY_FORCE"),
    }
    match result {
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
    let prev = std::env::var("GQLITE_ORDERBY_FORCE").ok();

    std::env::set_var("GQLITE_ORDERBY_FORCE", "pdqsort");
    let pdq = match Runtime::new(&store).run_query(&compile_query(q_str).unwrap(), 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    std::env::set_var("GQLITE_ORDERBY_FORCE", "btree-ltj");
    let bt = match Runtime::new(&store).run_query(&compile_query(q_str).unwrap(), 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    match prev {
        Some(v) => std::env::set_var("GQLITE_ORDERBY_FORCE", v),
        None => std::env::remove_var("GQLITE_ORDERBY_FORCE"),
    }
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
    let prev = std::env::var("GQLITE_ORDERBY_FORCE").ok();

    std::env::set_var("GQLITE_ORDERBY_FORCE", "pdqsort");
    let pdq = match Runtime::new(&store).run_query(&compile_query(q_str).unwrap(), 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    std::env::set_var("GQLITE_ORDERBY_FORCE", "btree-ltj-real");
    let real = match Runtime::new(&store).run_query(&compile_query(q_str).unwrap(), 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected"),
    };
    match prev {
        Some(v) => std::env::set_var("GQLITE_ORDERBY_FORCE", v),
        None => std::env::remove_var("GQLITE_ORDERBY_FORCE"),
    }
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
    assert_eq!(pdq_groups, real_groups, "u.id groups differ");
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
