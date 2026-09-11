//! Differential suite: adaptive VEO ≡ simple VEO (issue #101).
//!
//! `FROGQL_VEO=adaptive` re-picks the variable order one variable at a
//! time, from the subtree sizes the iterators report under the current
//! partial binding, instead of fixing the whole order up front from a
//! syntactic guess. Leapfrog is order-agnostic, so the *bag* of rows must
//! be identical; the order rows arrive in is not, which is why every
//! comparison here sorts.
//!
//! The battery crosses the two axes that can disagree: the order itself,
//! and the physical representation whose `subtree_size` feeds it (the
//! array reports index entries, the compact trie distinct leaves).

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Rich multigraph: reciprocal pair, parallel edges, self-loop, undirected
/// edge, second label, and a third label carrying a distinct property so a
/// pushed-down predicate has something to narrow. ids a=0..f=5.
fn rich() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"id": 0, "city": "X"}},
        {"id": "b", "labels": ["N"], "props": {"id": 1, "city": "Y"}},
        {"id": "c", "labels": ["N"], "props": {"id": 2, "city": "X"}},
        {"id": "d", "labels": ["N"], "props": {"id": 3, "city": "Y"}},
        {"id": "e", "labels": ["N"], "props": {"id": 4, "city": "Z"}},
        {"id": "f", "labels": ["N", "M"], "props": {"id": 5, "city": "X"}}
      ],
      "edges": [
        {"id": "ab", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "ba", "labels": ["R"], "props": {}, "endpoints": ["b", "a"], "directionality": "->"},
        {"id": "bc", "labels": ["R"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"},
        {"id": "cc", "labels": ["R"], "props": {}, "endpoints": ["c", "c"], "directionality": "->"},
        {"id": "ad1", "labels": ["R"], "props": {}, "endpoints": ["a", "d"], "directionality": "->"},
        {"id": "ad2", "labels": ["R"], "props": {}, "endpoints": ["a", "d"], "directionality": "->"},
        {"id": "de", "labels": ["R"], "props": {}, "endpoints": ["d", "e"], "directionality": "~~"},
        {"id": "ef", "labels": ["R"], "props": {}, "endpoints": ["e", "f"], "directionality": "->"},
        {"id": "fa", "labels": ["S"], "props": {}, "endpoints": ["f", "a"], "directionality": "->"},
        {"id": "bcS", "labels": ["S"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run(g: &MemoryGraphStore, q: &str, adaptive: bool, compact: bool) -> Vec<String> {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Both sides of both axes are set explicitly. The adaptive order and
    // the compact representation are the defaults now, so *removing* a
    // variable would run the same side twice and the comparison would
    // pass for the wrong reason.
    std::env::set_var("FROGQL_VEO", if adaptive { "adaptive" } else { "simple" });
    std::env::set_var("FROGQL_LTJ_REPR", if compact { "compact" } else { "array" });
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    let out = match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("expected projected, got {other:?}"),
    };
    std::env::remove_var("FROGQL_VEO");
    std::env::remove_var("FROGQL_LTJ_REPR");
    let mut keys: Vec<String> = out.iter().map(|r| format!("{r:?}")).collect();
    keys.sort();
    keys
}

fn assert_veos_agree_on(g: &MemoryGraphStore, q: &str) {
    for compact in [false, true] {
        let simple = run(g, q, false, compact);
        let adaptive = run(g, q, true, compact);
        assert_eq!(
            simple, adaptive,
            "simple ≠ adaptive multiset for {q} (compact={compact})\
             \n  simple={simple:?}\n  adaptive={adaptive:?}"
        );
    }
}

fn assert_veos_agree(q: &str) {
    let g = rich();
    assert_veos_agree_on(&g, q);
}

#[test]
fn agree_one_hop() {
    assert_veos_agree("MATCH (x:N)-[:R]->(y:N) RETURN x.id, y.id");
    assert_veos_agree("MATCH (x:N)-[]->(y:N) RETURN x.id, y.id");
}

#[test]
fn agree_chains() {
    assert_veos_agree("MATCH (x:N)-[:R]->(y:N)-[:R]->(z:N) RETURN x.id, y.id, z.id");
    assert_veos_agree("MATCH (w:N)-[]->(x:N)-[]->(y:N)-[]->(z:N) RETURN w.id, z.id");
}

#[test]
fn agree_directions() {
    assert_veos_agree("MATCH (x:N)<-[:R]-(y:N) RETURN x.id, y.id");
    assert_veos_agree("MATCH (x:N)~[:R]~(y:N) RETURN x.id, y.id");
    assert_veos_agree("MATCH (x:N)-[e]-(y:N) RETURN x.id, y.id");
    assert_veos_agree("MATCH (x:N)-[:R]->(y:N)-[]-(z:N) RETURN x.id, z.id");
}

#[test]
fn agree_joins() {
    assert_veos_agree("MATCH (x:N)-[]->(y:N), (y:N)-[]->(z:N) RETURN x.id, y.id, z.id");
    assert_veos_agree("MATCH (x)-[]->(y), (y)-[]->(z), (x)-[]->(z) RETURN x.id, y.id, z.id");
}

/// The clique is the shape the adaptive order exists for: every variable
/// is related to every other, so each binding re-weighs two neighbours.
#[test]
fn agree_triangle_with_labels() {
    assert_veos_agree("MATCH (x)-[:R]->(y), (y)-[:S]->(z), (x)-[:R]->(z) RETURN x.id, z.id");
}

/// Filters are what the adaptive path resolves per binding rather than per
/// level, so every filter kind gets an entry: label, property comparison,
/// equality (index-foldable), and a predicate over two variables.
#[test]
fn agree_with_pushed_down_filters() {
    assert_veos_agree("MATCH (x:N)-[e:R]->(y:N) WHERE y.id = 3 RETURN x.id, y.id");
    assert_veos_agree("MATCH (x:N)-[:R]->(y:N) WHERE y.id > 2 RETURN x.id, y.id");
    assert_veos_agree("MATCH (x:N)-[]->(y:M) RETURN x.id, y.id");
    assert_veos_agree(
        "MATCH (x:N)-[:R]->(y:N)-[:R]->(z:N) WHERE x.city = 'X' AND z.id > 1 RETURN x.id, z.id",
    );
}

/// A filter whose only dependency is bound at a deeper level than the
/// variable it names would be evaluated at the wrong time; the dynamic
/// placement has to find the level where the *last* dependency binds.
#[test]
fn agree_with_filters_on_every_variable() {
    assert_veos_agree(
        "MATCH (x:N)-[:R]->(y:N)-[:R]->(z:N) \
         WHERE x.id >= 0 AND y.city = 'Y' AND z.id < 5 RETURN x.id, y.id, z.id",
    );
}

#[test]
fn agree_repeated_variable_and_self_loop() {
    assert_veos_agree("MATCH (x:N)-[:R]->(x) RETURN x.id");
    assert_veos_agree("MATCH (x:N)-[:R]->(y:N)-[:R]->(x) RETURN x.id, y.id");
}

#[test]
fn agree_anonymous_and_union_labels() {
    assert_veos_agree("MATCH (x:N)-[]->()-[]->(z:N) RETURN x.id, z.id");
    assert_veos_agree("MATCH (x)-[:R|S]->(y) RETURN x.id, y.id");
}

#[test]
fn agree_through_projection_clauses() {
    assert_veos_agree("MATCH (x:N)-[]->(y:N) RETURN DISTINCT x.id");
    assert_veos_agree("MATCH (x:N)-[]->(y:N) RETURN COUNT(y) AS c GROUP BY x");
    assert_veos_agree("MATCH (x:N)-[:R]->(y:N) RETURN x.id, y.id ORDER BY y.id, x.id");
}

/// Shapes the LTJ declines: the switch must be inert outside it.
#[test]
fn agree_on_fallback_shapes() {
    assert_veos_agree("MATCH (x:N)(-[:R]->|<-[:R]-)(y:N) RETURN x.id, y.id");
    assert_veos_agree("MATCH (x:N)-[:R]->{1,2}(y:N) RETURN x.id, y.id");
    assert_veos_agree("MATCH (x:N) RETURN x.id");
}

/// Bag multiplicity is decided at the base case, below the order; parallel
/// edges must still produce one row each under either VEO.
#[test]
fn agree_on_parallel_edge_multiplicity() {
    assert_veos_agree("MATCH (x:N)-[e:R]->(y:N) WHERE x.id = 0 AND y.id = 3 RETURN x.id, y.id");
    assert_veos_agree("MATCH (x:N)-[:R]->(y:N) WHERE x.id = 0 RETURN x.id, y.id");
}

/// An empty result is a result: the order must not invent rows when the
/// pattern has none.
#[test]
fn agree_on_empty_results() {
    assert_veos_agree("MATCH (x:N)-[:R]->(y:N) WHERE y.id = 99 RETURN x.id, y.id");
    assert_veos_agree("MATCH (x:M)-[:R]->(y:M) RETURN x.id, y.id");
}
