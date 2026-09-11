//! Differential: LTJ must honour edge direction the way the scan does.
//!
//! `run_edge_pattern` is the reference semantics, and it is explicit:
//! `-[]->` / `<-[]-` take candidates from `edges_directed` only, `~[]~`
//! from `edges_undirected` only, and `-[]-` is the union of all three.
//! That union would be redundant if a directed pattern edge already
//! matched an undirected stored one.
//!
//! LTJ decomposes every pattern edge to a forward triple and queries one
//! index that holds directed edges in their physical sense and undirected
//! edges in **both** senses (`triple_index.rs`, `push_both`). Nothing in
//! that triple says which kind it came from, so the join matched any
//! pattern edge against any stored edge — in both directions of the
//! mistake: `-[:L]->` found undirected edges, and `~[:L]~` found directed
//! ones.
//!
//! Found on LDBC, where `knows` is stored undirected: with the unroll
//! optimizer on, `(p)-[:knows]->{1,2}(f)` answered 188; with it off, 0.
//! The optimizer is documented as performance-preserving, and no test
//! A/B'd it — `FROGQL_DISABLE_REPEAT_UNROLL` had no coverage at all.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Two directed edges and two undirected ones, all labelled `R`, so the
/// label cannot stand in for the direction: a-D->b, b-D->c (directed),
/// c~U~d, d~U~e (undirected). Both chains are two hops long, so a
/// repetition over either has something to find.
fn mixed() -> MemoryGraphStore {
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
        {"id": "bc", "labels": ["R"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"},
        {"id": "cd", "labels": ["R"], "props": {}, "endpoints": ["c", "d"], "directionality": "~~"},
        {"id": "de", "labels": ["R"], "props": {}, "endpoints": ["d", "e"], "directionality": "~~"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run(q: &str, unroll: bool) -> Vec<String> {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if unroll {
        std::env::remove_var("FROGQL_DISABLE_REPEAT_UNROLL");
    } else {
        std::env::set_var("FROGQL_DISABLE_REPEAT_UNROLL", "1");
    }
    let g = mixed();
    let rt = Runtime::new(&g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
    let out = match rt.run_query(&query, 0) {
        QueryResult::Projected(rows) => rows,
        other => panic!("expected a projection, got {other:?}"),
    };
    std::env::remove_var("FROGQL_DISABLE_REPEAT_UNROLL");
    let mut keys: Vec<String> = out.iter().map(|r| format!("{r:?}")).collect();
    keys.sort();
    keys
}

/// `{1,1}` is the smallest repetition, and the unroll flag makes the same
/// query take the LTJ path or the scan path. Anything they disagree on is
/// a semantic divergence, not an optimization.
fn assert_paths_agree(q: &str) {
    let ltj = run(q, true);
    let scan = run(q, false);
    assert_eq!(
        ltj, scan,
        "LTJ ≠ scan for {q}\n  ltj={ltj:?}\n  scan={scan:?}"
    );
}

#[test]
fn a_directed_pattern_edge_does_not_match_an_undirected_one() {
    // c~R~d is undirected: `->` must not see it.
    assert_paths_agree("MATCH (x:N)-[:R]->{1,1}(y:N) WHERE x.id = 2 RETURN y.id");
    assert_paths_agree("MATCH (x:N)-[:R]->{1,1}(y:N) RETURN x.id, y.id");
    assert_paths_agree("MATCH (x:N)<-[:R]-{1,1}(y:N) RETURN x.id, y.id");
}

#[test]
fn an_undirected_pattern_edge_does_not_match_a_directed_one() {
    // a-R->b is directed: `~` must not see it.
    assert_paths_agree("MATCH (x:N)~[:R]~{1,1}(y:N) WHERE x.id = 0 RETURN y.id");
    assert_paths_agree("MATCH (x:N)~[:R]~{1,1}(y:N) RETURN x.id, y.id");
}

/// `-[]-` is the union of all three, so it is the one kind that *should*
/// see everything. A fix that filtered too eagerly would break it.
#[test]
fn an_any_direction_edge_still_matches_both_kinds() {
    assert_paths_agree("MATCH (x:N)-[:R]-{1,1}(y:N) RETURN x.id, y.id");
    assert_paths_agree("MATCH (x:N)-[:R]-{1,1}(y:N) WHERE x.id = 2 RETURN y.id");
}

/// The shape the bug was found on: a bounded repetition, where the
/// unrolled arms go through LTJ and the fallback does not.
#[test]
fn a_bounded_repetition_agrees_across_the_unroll_switch() {
    for q in [
        "MATCH (x:N)-[:R]->{1,2}(y:N) RETURN x.id, y.id",
        "MATCH (x:N)~[:R]~{1,2}(y:N) RETURN x.id, y.id",
        "MATCH (x:N)-[:R]-{1,2}(y:N) RETURN x.id, y.id",
        "MATCH (x:N)-[:R]->{1,3}(y:N) WHERE x.id = 0 RETURN y.id",
    ] {
        assert_paths_agree(q);
    }
}

/// Unlabelled, so the only thing separating the arms is the direction.
#[test]
fn the_same_holds_with_no_label() {
    assert_paths_agree("MATCH (x:N)-[]->{1,1}(y:N) RETURN x.id, y.id");
    assert_paths_agree("MATCH (x:N)~[]~{1,1}(y:N) RETURN x.id, y.id");
    assert_paths_agree("MATCH (x:N)-[]-{1,1}(y:N) RETURN x.id, y.id");
}
