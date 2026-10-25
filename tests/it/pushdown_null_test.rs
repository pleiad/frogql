//! Pushdown / 3VL consistency for a `null`/missing comparison operand.
//!
//! A top-level `attr op literal` predicate reaches the result down three
//! routes, which agree on the keep/drop decision for a row whose property is
//! absent:
//!
//!   1. plain node scan — `filter_node` -> `cmp_values` (no-edge pattern);
//!   2. LTJ in-loop filter — `NodeAttrCmp` -> `cmp_values` (edge pattern, a
//!      non-foldable predicate such as `<>`);
//!   3. secondary index — `lookup_node_eq` / `lookup_node_range`, folded in
//!      `pattern_extract` before the join (edge pattern, a foldable `=` /
//!      `>=` / `>` predicate).
//!
//! Against a null operand `cmp_values` yields `false`, the residual 3VL
//! `eval_binop` yields `Value::Null` (`get_bool` -> false), and the index
//! holds no entry for the row; every route drops it. Pushdown lifts only
//! top-level positive `attr op literal` AND-conjuncts, so `... OR true` and
//! `NOT (...)` run on the 3VL interpreter.
//!
//! Route 3's index needs `LazyGraphStore` (a real `.gdb`). The auto-index
//! covers a `(label, prop)` only when the prop is present on every node of the
//! label with unique values, so it skips the sparse `(N, a)`; the route-3 test
//! declares hash+btree on `(N, a)` directly.

use std::path::PathBuf;

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::store::lazy::LazyGraphStore;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("frogql_pushdown_null");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

/// Three N-nodes: a=5, a=6, and one with `a` absent (reads as null),
/// materialised through a `.gdb` and reopened as `LazyGraphStore`.
fn nodes(name: &str) -> LazyGraphStore {
    let g = MemoryGraphStore::from_json_str(
        r#"{"nodes":[
            {"id":"p5","labels":["N"],"props":{"name":"p5","a":5}},
            {"id":"p6","labels":["N"],"props":{"name":"p6","a":6}},
            {"id":"pN","labels":["N"],"props":{"name":"pN"}}
        ],"edges":[]}"#,
    )
    .unwrap();
    let path = temp_db(name);
    g.save(&path).unwrap();
    LazyGraphStore::open(&path).unwrap()
}

/// Same three nodes, each with one outgoing edge to a shared sink, so an
/// `x.a <op> literal` filter runs through the LTJ path — `NodeAttrCmp` in-loop,
/// or folded to the secondary index.
fn nodes_with_edges(name: &str) -> LazyGraphStore {
    let g = MemoryGraphStore::from_json_str(
        r#"{"nodes":[
            {"id":"p5","labels":["N"],"props":{"name":"p5","a":5}},
            {"id":"p6","labels":["N"],"props":{"name":"p6","a":6}},
            {"id":"pN","labels":["N"],"props":{"name":"pN"}},
            {"id":"s","labels":["S"],"props":{"name":"s"}}
        ],"edges":[
            {"id":"e5","labels":["E"],"props":{},"endpoints":["p5","s"],"directionality":"->"},
            {"id":"e6","labels":["E"],"props":{},"endpoints":["p6","s"],"directionality":"->"},
            {"id":"eN","labels":["E"],"props":{},"endpoints":["pN","s"],"directionality":"->"}
        ]}"#,
    )
    .unwrap();
    let path = temp_db(name);
    g.save(&path).unwrap();
    LazyGraphStore::open(&path).unwrap()
}

fn names(g: &LazyGraphStore, q: &str) -> Vec<String> {
    let query = compile_query(q).expect("compile");
    let mut ns: Vec<String> = match Runtime::new(g).run_query(&query, 0) {
        QueryResult::Projected(rs) => rs
            .into_iter()
            .map(|r| match &r[0] {
                Value::Str(s) => s.clone(),
                other => panic!("expected Str, got {other:?}"),
            })
            .collect(),
        other => panic!("expected projected rows, got {other:?}"),
    };
    ns.sort();
    ns
}

fn v(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

/// Route 1 — the plain node scan. A no-edge pattern skips LTJ pattern
/// extraction, so every predicate runs through `filter_node` (`cmp_values`).
#[test]
fn test_scan_route_is_3vl_consistent() {
    let g = nodes("scan.gdb");
    // pushed vs reversed (same logical comparison, possibly different path)
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a = 5 RETURN x.name"),
        v(&["p5"])
    );
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE 5 = x.a RETURN x.name"),
        v(&["p5"])
    );
    // <> drops the missing-property row (null <> 5 = unknown -> drop)
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a <> 5 RETURN x.name"),
        v(&["p6"])
    );
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE 5 <> x.a RETURN x.name"),
        v(&["p6"])
    );
    assert!(names(&g, "MATCH (x:N) WHERE x.a > 100 RETURN x.name").is_empty());
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a >= 5 RETURN x.name"),
        v(&["p5", "p6"])
    );
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a IS NULL RETURN x.name"),
        v(&["pN"])
    );
}

/// `... OR true` and `NOT (...)` keep rows a pushed `attr op literal` filter
/// would drop, so they run on the residual 3VL interpreter.
#[test]
fn test_keep_despite_null_stays_residual_not_pushed() {
    let g = nodes("residual.gdb");
    // `= 999` must NOT be lifted out of the OR: `_ OR true` keeps every row.
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a = 999 OR true RETURN x.name"),
        v(&["p5", "p6", "pN"]),
    );
    // NOT over a comparison stays residual: missing -> NOT(null=5)=null -> drop.
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE NOT (x.a = 5) RETURN x.name"),
        v(&["p6"])
    );
    // An OR of two pushable comparisons stays a single residual OR.
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a = 5 OR x.a = 6 RETURN x.name"),
        v(&["p5", "p6"]),
    );
}

/// Routes 2 and 3 — an edge routes the pattern through LTJ. `<>` is not
/// index-foldable and runs as an in-loop `NodeAttrCmp` (`cmp_values`); `=` /
/// `>=` / `>` fold to the secondary index (`lookup_node_eq` /
/// `lookup_node_range`) before the join. The auto-index skips `(N, a)` because
/// pN lacks `a`, so the index is declared explicitly.
#[test]
fn test_ltj_and_index_routes_are_3vl_consistent() {
    use frogql::store::secondary_index::IndexKind;
    let g = nodes_with_edges("ltj.gdb");

    // Route 2 — in-loop `NodeAttrCmp`: `<>` is not foldable; the missing-`a`
    // row drops.
    assert_eq!(
        names(&g, "MATCH (x:N)-[:E]->(s) WHERE x.a <> 5 RETURN x.name"),
        v(&["p6"]),
    );

    // Route 3 — with an index on `a` declared, the foldable predicates lower
    // to lookup_node_eq / lookup_node_range; pN has no entry and drops.
    g.secondary_indexes_mut()
        .build_declared(&g, "N_a_hash".to_string(), "N", "a", IndexKind::Hash)
        .unwrap();
    g.secondary_indexes_mut()
        .build_declared(&g, "N_a_btree".to_string(), "N", "a", IndexKind::BTree)
        .unwrap();
    // eq fold (hash):
    assert_eq!(
        names(&g, "MATCH (x:N)-[:E]->(s) WHERE x.a = 5 RETURN x.name"),
        v(&["p5"]),
    );
    // range fold (btree):
    assert_eq!(
        names(&g, "MATCH (x:N)-[:E]->(s) WHERE x.a >= 5 RETURN x.name"),
        v(&["p5", "p6"]),
    );
    assert!(names(&g, "MATCH (x:N)-[:E]->(s) WHERE x.a > 100 RETURN x.name").is_empty());
}
