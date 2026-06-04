//! BFS fast-path for `[ANY|ALL] SHORTEST` over a single repeated edge.
//!
//! The fast-path (`Runtime::try_shortest_bfs`) must produce results
//! identical to the generic walk-enumeration selection it shortcuts. These
//! tests pin the known answers and differentially compare the two paths by
//! toggling `GQLITE_DISABLE_SHORTEST_BFS`.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

/// Undirected graph with two equal-length shortest routes 0→3 and a cycle.
/// ids: a=0, b=1, c=2, d=3, e=4. Edges (undirected `~~`, label R):
///   0~1, 1~2, 2~3, 0~4, 4~3, 1~3.
/// Shortest 0→3 has length 2 via 0-1-3 and 0-4-3.
fn g_undirected() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"id": 0}},
        {"id": "b", "labels": ["N"], "props": {"id": 1}},
        {"id": "c", "labels": ["N"], "props": {"id": 2}},
        {"id": "d", "labels": ["N"], "props": {"id": 3}},
        {"id": "e", "labels": ["N"], "props": {"id": 4}}
      ],
      "edges": [
        {"id": "ab", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "~~"},
        {"id": "bc", "labels": ["R"], "props": {}, "endpoints": ["b", "c"], "directionality": "~~"},
        {"id": "cd", "labels": ["R"], "props": {}, "endpoints": ["c", "d"], "directionality": "~~"},
        {"id": "ae", "labels": ["R"], "props": {}, "endpoints": ["a", "e"], "directionality": "~~"},
        {"id": "ed", "labels": ["R"], "props": {}, "endpoints": ["e", "d"], "directionality": "~~"},
        {"id": "bd", "labels": ["R"], "props": {}, "endpoints": ["b", "d"], "directionality": "~~"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// Directed diamond: 0→3 via 0-1-3 and 0-4-3 (both length 2), plus a longer
/// 0-2-...; ids a=0,b=1,c=2,d=3,e=4.
fn g_directed() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"id": 0}},
        {"id": "b", "labels": ["N"], "props": {"id": 1}},
        {"id": "c", "labels": ["N"], "props": {"id": 2}},
        {"id": "d", "labels": ["N"], "props": {"id": 3}},
        {"id": "e", "labels": ["N"], "props": {"id": 4}}
      ],
      "edges": [
        {"id": "ab", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "bd", "labels": ["R"], "props": {}, "endpoints": ["b", "d"], "directionality": "->"},
        {"id": "ae", "labels": ["R"], "props": {}, "endpoints": ["a", "e"], "directionality": "->"},
        {"id": "ed", "labels": ["R"], "props": {}, "endpoints": ["e", "d"], "directionality": "->"},
        {"id": "ac", "labels": ["R"], "props": {}, "endpoints": ["a", "c"], "directionality": "->"},
        {"id": "cd", "labels": ["R"], "props": {}, "endpoints": ["c", "d"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// Serializes the `GQLITE_DISABLE_SHORTEST_BFS` toggle: the var is
/// process-global, so concurrent tests would otherwise clobber each other.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run_proj(g: &MemoryGraphStore, q: &str, bfs: bool) -> Vec<Vec<Value>> {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if bfs {
        std::env::remove_var("GQLITE_DISABLE_SHORTEST_BFS");
    } else {
        std::env::set_var("GQLITE_DISABLE_SHORTEST_BFS", "1");
    }
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    let out = match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected for {q:?}"),
    };
    std::env::remove_var("GQLITE_DISABLE_SHORTEST_BFS");
    out
}

fn canon(rows: &[Vec<Value>]) -> Vec<String> {
    let mut v: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
    v.sort();
    v
}

/// BFS fast-path and generic selection must agree row-for-row.
fn assert_same(g: &MemoryGraphStore, q: &str) {
    let bfs = run_proj(g, q, true);
    let gen = run_proj(g, q, false);
    assert_eq!(
        canon(&bfs),
        canon(&gen),
        "BFS vs generic mismatch for query: {q}\n  bfs={bfs:?}\n  gen={gen:?}"
    );
}

#[test]
fn any_shortest_undirected_pinned_length() {
    let g = g_undirected();
    let q = "MATCH path = ANY SHORTEST (s:N {id: 0})~[:R]~*(t:N {id: 3}) \
             RETURN PATH_LENGTH(path) AS len";
    let rows = run_proj(&g, q, true);
    assert_eq!(rows.len(), 1, "one shortest path for one (s,t) pair");
    assert_eq!(rows[0][0], Value::Int(2), "0→3 shortest length is 2");
}

#[test]
fn all_shortest_undirected_returns_every_min_path() {
    let g = g_undirected();
    let q = "MATCH path = ALL SHORTEST (s:N {id: 0})~[:R]~*(t:N {id: 3}) \
             RETURN PATH_LENGTH(path) AS len, [n IN NODES(path) | n.id] AS ns";
    let rows = run_proj(&g, q, true);
    assert_eq!(rows.len(), 2, "two length-2 shortest paths 0→3");
    for r in &rows {
        assert_eq!(r[0], Value::Int(2));
    }
    let node_lists = canon(&rows);
    // 0-1-3 and 0-4-3
    assert!(node_lists
        .iter()
        .any(|s| s.contains("Int(0), Int(1), Int(3)")));
    assert!(node_lists
        .iter()
        .any(|s| s.contains("Int(0), Int(4), Int(3)")));
}

#[test]
fn star_admits_zero_length_self_path() {
    let g = g_undirected();
    // `*` (lb 0): the (0,0) pair is the length-0 path.
    let q = "MATCH path = ANY SHORTEST (s:N {id: 0})~[:R]~*(t:N {id: 0}) \
             RETURN PATH_LENGTH(path) AS len";
    let rows = run_proj(&g, q, true);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Int(0));
}

#[test]
fn plus_self_pair_is_shortest_closed_walk() {
    let g = g_undirected();
    // `+` (lb 1): no zero-length match. Under WALK the shortest (0,0)
    // closed walk reuses an incident edge (length 2), matching the generic
    // enumerator.
    let q = "MATCH path = ANY SHORTEST (s:N {id: 0})~[:R]~+(t:N {id: 0}) \
             RETURN PATH_LENGTH(path) AS len";
    let rows = run_proj(&g, q, true);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Int(2));
}

// ---- Differential: BFS == generic ----

#[test]
fn differential_undirected() {
    let g = g_undirected();
    assert_same(&g, "MATCH path = ANY SHORTEST (s:N {id:0})~[:R]~*(t:N {id:3}) RETURN s.id, t.id, PATH_LENGTH(path) AS len");
    assert_same(&g, "MATCH path = ALL SHORTEST (s:N {id:0})~[:R]~*(t:N {id:3}) RETURN PATH_LENGTH(path) AS len, [n IN NODES(path) | n.id] AS ns");
    // Unpinned target: every reachable node at its shortest distance.
    assert_same(
        &g,
        "MATCH path = ANY SHORTEST (s:N {id:0})~[:R]~*(t:N) RETURN t.id, PATH_LENGTH(path) AS len",
    );
    assert_same(
        &g,
        "MATCH path = ANY SHORTEST (s:N {id:0})~[:R]~+(t:N) RETURN t.id, PATH_LENGTH(path) AS len",
    );
    // Unpinned source.
    assert_same(
        &g,
        "MATCH path = ANY SHORTEST (s:N)~[:R]~*(t:N {id:3}) RETURN s.id, PATH_LENGTH(path) AS len",
    );
    // ALL SHORTEST, unpinned target.
    assert_same(&g, "MATCH path = ALL SHORTEST (s:N {id:0})~[:R]~*(t:N) RETURN t.id, PATH_LENGTH(path) AS len, [n IN NODES(path) | n.id] AS ns");
    // `+` exercises the coincident-endpoint closed-walk handling, with and
    // without enumerating all minimum-length walks.
    assert_same(
        &g,
        "MATCH path = ANY SHORTEST (s:N)~[:R]~+(t:N) RETURN s.id, t.id, PATH_LENGTH(path) AS len",
    );
    assert_same(&g, "MATCH path = ALL SHORTEST (s:N)~[:R]~+(t:N) RETURN s.id, t.id, PATH_LENGTH(path) AS len, [n IN NODES(path) | n.id] AS ns");
}

#[test]
fn differential_directed() {
    let g = g_directed();
    assert_same(&g, "MATCH path = ANY SHORTEST (s:N {id:0})-[:R]->*(t:N {id:3}) RETURN s.id, t.id, PATH_LENGTH(path) AS len");
    assert_same(&g, "MATCH path = ALL SHORTEST (s:N {id:0})-[:R]->*(t:N {id:3}) RETURN PATH_LENGTH(path) AS len, [n IN NODES(path) | n.id] AS ns");
    assert_same(
        &g,
        "MATCH path = ANY SHORTEST (s:N {id:0})-[:R]->*(t:N) RETURN t.id, PATH_LENGTH(path) AS len",
    );
    // Reverse edge direction.
    assert_same(&g, "MATCH path = ANY SHORTEST (s:N {id:3})<-[:R]-*(t:N {id:0}) RETURN s.id, t.id, PATH_LENGTH(path) AS len");
}

#[test]
fn differential_bounded_range() {
    // The `{1,3}` shape is exactly LDBC IC1's. SHORTEST over a bounded
    // range keeps only pairs reachable within the bound, ranked shortest.
    let g = g_undirected();
    assert_same(&g, "MATCH path = ANY SHORTEST (s:N {id:0})~[:R]~{1,3}(t:N) RETURN s.id, t.id, PATH_LENGTH(path) AS len");
    assert_same(&g, "MATCH path = ALL SHORTEST (s:N {id:0})~[:R]~{1,2}(t:N) RETURN t.id, PATH_LENGTH(path) AS len, [n IN NODES(path) | n.id] AS ns");
    // A tight bound that excludes the farther node entirely.
    assert_same(
        &g,
        "MATCH path = ANY SHORTEST (s:N {id:0})~[:R]~{1,1}(t:N) RETURN t.id, PATH_LENGTH(path) AS len",
    );
    let g2 = g_directed();
    assert_same(&g2, "MATCH path = ANY SHORTEST (s:N {id:0})-[:R]->{1,3}(t:N) RETURN t.id, PATH_LENGTH(path) AS len");
}

#[test]
fn differential_unreachable() {
    // Two disconnected components: nothing connects 0 to a missing target.
    let g = g_directed();
    // 3 has no outgoing R edges, so 3→0 is unreachable forward.
    assert_same(&g, "MATCH path = ANY SHORTEST (s:N {id:3})-[:R]->*(t:N {id:0}) RETURN s.id, t.id, PATH_LENGTH(path) AS len");
}
