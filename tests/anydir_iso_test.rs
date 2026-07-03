//! Any-direction LTJ (`try_ltj_anydir` over the mirrored index) must
//! produce ISO bag multiplicity (issue #71), now that the base case fans
//! out per physical edge. Counts are pinned by hand (ISO ground truth),
//! NOT compared to the scan fallback — the fallback is still non-ISO for
//! the cases LTJ doesn't cover (mixed direction, repetition) and aligning
//! it is separate follow-up work. `GQLITE_DISABLE_ANYDIR_LTJ=1` forces the
//! fallback; here we exercise the LTJ path (default on).

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// Minimal graph, hand-verifiable. ids: a=0, b=1, c=2.
/// Edges (label R): a→b, b→a (reciprocal), a→b again (parallel to the
/// first), c→c (self-loop).
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"id": 0}},
        {"id": "b", "labels": ["N"], "props": {"id": 1}},
        {"id": "c", "labels": ["N"], "props": {"id": 2}}
      ],
      "edges": [
        {"id": "ab1", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "ba", "labels": ["R"], "props": {}, "endpoints": ["b", "a"], "directionality": "->"},
        {"id": "ab2", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "cc", "labels": ["R"], "props": {}, "endpoints": ["c", "c"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn proj(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("expected projected, got {other:?}"),
    }
}

fn sorted_pairs(rows: &[Vec<Value>]) -> Vec<(i64, i64)> {
    let mut v: Vec<(i64, i64)> = rows
        .iter()
        .map(|r| match (&r[0], &r[1]) {
            (Value::Int(a), Value::Int(b)) => (*a, *b),
            other => panic!("expected (Int, Int), got {other:?}"),
        })
        .collect();
    v.sort();
    v
}

fn sorted_ints(rows: &[Vec<Value>], col: usize) -> Vec<i64> {
    let mut v: Vec<i64> = rows
        .iter()
        .map(|r| match &r[col] {
            Value::Int(n) => *n,
            other => panic!("expected Int, got {other:?}"),
        })
        .collect();
    v.sort();
    v
}

#[test]
fn one_hop_from_a() {
    // a's incident R edges, either direction: ab1 (a→b, fwd → b),
    // ab2 (a→b, fwd → b), ba (b→a, traversed back → b). Three rows to b.
    let g = graph();
    let rows = proj(&g, "MATCH (x:N)-[:R]-(y:N) WHERE x.id = 0 RETURN y.id");
    assert_eq!(sorted_ints(&rows, 0), vec![1, 1, 1]);
}

#[test]
fn full_one_hop_both_orientations() {
    // Every (x, y) over the three a↔b edges (ab1, ab2, ba), each traversable
    // both ways, plus the c→c self-loop counted from c in both senses.
    //   a↔b: (0,1) via ab1, (0,1) via ab2, (0,1) via ba-backward,
    //        (1,0) via ba-forward, (1,0) via ab1-backward, (1,0) via ab2-backward
    //   c self-loop: (2,2) twice (both mirror senses)
    let g = graph();
    let rows = proj(&g, "MATCH (x:N)-[:R]-(y:N) RETURN x.id, y.id");
    assert_eq!(
        sorted_pairs(&rows),
        vec![
            (0, 1),
            (0, 1),
            (0, 1),
            (1, 0),
            (1, 0),
            (1, 0),
            (2, 2),
            (2, 2),
        ]
    );
}

#[test]
fn distinct_collapses_to_endpoint_pairs() {
    let g = graph();
    let rows = proj(&g, "MATCH (x:N)-[:R]-(y:N) RETURN DISTINCT x.id, y.id");
    assert_eq!(sorted_pairs(&rows), vec![(0, 1), (1, 0), (2, 2)]);
}

#[test]
fn unused_edge_repetition_unrolls_to_mirrored_ltj() {
    // `(x)-[]-{1,2}(y)` with the edge unused unrolls into a Union whose
    // any-direction arms run through the mirrored LTJ (issue #71, #57).
    // From a, lengths 1..2, WALK semantics over the three a↔b edges and the
    // c-self-loop. Length 1 from a: b (×3, one per incident a↔b edge).
    // Length 2 from a: back to a (×9: 3 hops out to b × 3 hops back), and
    // b's only other neighbour is a, so no new nodes. So targets:
    //   len 1 → b three times
    //   len 2 → a nine times
    let g = graph();
    let rows = proj(&g, "MATCH (x:N)-[]-{1,2}(y:N) WHERE x.id = 0 RETURN y.id");
    let mut ys = sorted_ints(&rows, 0);
    ys.dedup();
    // The distinct reachable set is {a, b}; multiplicity is exercised in
    // the pinned counts below.
    assert_eq!(ys, vec![0, 1]);
    let all = sorted_ints(&rows, 0);
    assert_eq!(all.iter().filter(|&&v| v == 1).count(), 3, "len-1 → b ×3");
    assert_eq!(all.iter().filter(|&&v| v == 0).count(), 9, "len-2 → a ×9");
}

#[test]
fn comma_join_any_direction() {
    // Pure any-direction comma-join goes through try_ltj_anydir (run_join).
    // (x)-[:R]-(y), (y)-[:R]-(z) with x=a: y ∈ {b (×3)}, then from b:
    // z ∈ {a (×3)} — b's incident edges are the same three a↔b edges.
    // 3 × 3 = 9 rows, all (0, 1, 0).
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (x:N)-[:R]-(y:N), (y:N)-[:R]-(z:N) WHERE x.id = 0 RETURN x.id, z.id",
    );
    assert_eq!(rows.len(), 9);
    assert!(rows
        .iter()
        .all(|r| matches!((&r[0], &r[1]), (Value::Int(0), Value::Int(0)))));
}
