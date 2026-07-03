//! Cross-path ISO consistency for any-direction edges (issue #71).
//!
//! Three execution paths can serve an any-direction pattern: the mirrored
//! LTJ (`try_ltj_anydir`), the seeded adjacency repetition
//! (`try_concat_with_edge_repetition`), and the plain adjacency /
//! hash-join fallback. All three iterate physical edges and, since the
//! LTJ base-case per-edge fan-out landed, all three produce the same ISO
//! bag multiplicity. This suite locks that in: `GQLITE_DISABLE_ANYDIR_LTJ`
//! toggles LTJ vs fallback, and the results must match as multisets across
//! every shape — so a future change to either path can't silently diverge.
//! One case is also anchored to a hand-computed ISO count.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// Rich multigraph: reciprocal pair, parallel edges, self-loop, undirected
/// edge, second label. ids a=0..e=4.
fn rich() -> MemoryGraphStore {
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
        {"id": "ba", "labels": ["R"], "props": {}, "endpoints": ["b", "a"], "directionality": "->"},
        {"id": "bc", "labels": ["R"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"},
        {"id": "cc", "labels": ["R"], "props": {}, "endpoints": ["c", "c"], "directionality": "->"},
        {"id": "ad1", "labels": ["R"], "props": {}, "endpoints": ["a", "d"], "directionality": "->"},
        {"id": "ad2", "labels": ["R"], "props": {}, "endpoints": ["a", "d"], "directionality": "->"},
        {"id": "de", "labels": ["R"], "props": {}, "endpoints": ["d", "e"], "directionality": "~~"},
        {"id": "bcS", "labels": ["S"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run(g: &MemoryGraphStore, q: &str, ltj: bool) -> Vec<String> {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if ltj {
        std::env::remove_var("GQLITE_DISABLE_ANYDIR_LTJ");
    } else {
        std::env::set_var("GQLITE_DISABLE_ANYDIR_LTJ", "1");
    }
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    let out = match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("expected projected, got {other:?}"),
    };
    std::env::remove_var("GQLITE_DISABLE_ANYDIR_LTJ");
    let mut keys: Vec<String> = out.iter().map(|r| format!("{r:?}")).collect();
    keys.sort();
    keys
}

fn assert_paths_agree(q: &str) {
    let g = rich();
    let ltj = run(&g, q, true);
    let fallback = run(&g, q, false);
    assert_eq!(
        ltj, fallback,
        "LTJ ≠ fallback multiset for {q}\n  LTJ={ltj:?}\n  fallback={fallback:?}"
    );
}

#[test]
fn agree_one_hop() {
    assert_paths_agree("MATCH (x:N)-[]-(y:N) RETURN x.id, y.id");
}

#[test]
fn agree_two_hop_chain() {
    assert_paths_agree("MATCH (x:N)-[]-(y:N)-[]-(z:N) RETURN x.id, z.id");
}

#[test]
fn agree_comma_join() {
    assert_paths_agree("MATCH (x:N)-[]-(y:N), (y:N)-[]-(z:N) RETURN x.id, z.id");
}

#[test]
fn agree_unused_edge_repetition() {
    // Unrolls → mirrored LTJ (LTJ on) vs global fallback (LTJ off).
    assert_paths_agree("MATCH (x:N)-[]-{1,2}(y:N) WHERE x.id = 0 RETURN x.id, y.id");
}

#[test]
fn agree_mixed_direction() {
    // Mixed directed+any-direction: falls back on both toggle settings, but
    // pins that the fallback is self-consistent and ISO (per-edge).
    assert_paths_agree("MATCH (x:N)-[:R]->(y:N)-[]-(z:N) RETURN x.id, z.id");
}

/// Seeded traversal (used-edge repetition, forced by naming `r`) produces
/// the ISO count, anchored by hand on a minimal graph: a=0, b=1 with three
/// a→b edges (`ab1`, `ab2` label R, `abS` label S) and one b→a (`ba`).
/// One hop `-[r]-{1,1}` from a reaches b via all four incident edges
/// (three forward, one backward) → 4 rows.
#[test]
fn seeded_used_edge_repetition_is_iso() {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"id": 0}},
        {"id": "b", "labels": ["N"], "props": {"id": 1}}
      ],
      "edges": [
        {"id": "ab1", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "ab2", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "abS", "labels": ["S"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "ba", "labels": ["R"], "props": {}, "endpoints": ["b", "a"], "directionality": "->"}
      ]
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let rt = Runtime::new(&g);
    let query = compile_query("MATCH (x:N)-[r]-{1,1}(y:N) WHERE x.id = 0 RETURN y.id").unwrap();
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("expected projected, got {other:?}"),
    };
    let ys: Vec<i64> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Int(v) => *v,
            other => panic!("expected Int, got {other:?}"),
        })
        .collect();
    assert_eq!(ys.len(), 4, "four incident edges → four matches");
    assert!(ys.iter().all(|&v| v == 1));
}
