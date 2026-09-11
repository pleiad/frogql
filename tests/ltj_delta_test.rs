//! Differential suite: maintaining the LTJ index incrementally ≡
//! rebuilding it (the delta overlay).
//!
//! Every successful DML statement used to drop the cached `TripleIndex`,
//! so the next query rebuilt all six orderings from the whole graph —
//! `O(E log E)`, 670 ms at LDBC SF0.1 and 252 seconds, measured, at 617 M
//! edges, to account for a change of a handful of edges. The index now
//! keeps its static payload and carries a small delta beside it.
//!
//! One optimisation, one kill switch (`FROGQL_DISABLE_LTJ_DELTA=1`), one
//! test asserting "optimised ≡ baseline" — and the A/B has to be on the
//! *right* axis. The precedent is the edge-direction bug that survived
//! 1600 tests because `compact_ltj_test` compared two representations of
//! the same index, wrong in the same way. So each case here runs the same
//! mutations twice against two independent stores, one refreshing its
//! index and one rebuilding it, and compares the rows.
//!
//! And both representations: the delta is six sorted arrays whichever one
//! the base is, so the merging iterator has an array-over-array path and
//! an array-over-tries path, and only the second exercises the compact
//! iterator's `dead_from`.

use std::sync::Mutex;

use frogql::model::graph::{MemoryGraphStore, Props};
use frogql::model::graph_access::{GraphAccess, GraphAccessMut};
use frogql::model::value::{Id, Value};
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::typing::label_type::LabelType;

/// The representation and the kill switch are both process-global.
static ENV: Mutex<()> = Mutex::new(());

/// Reciprocal pair, parallel edges, a self-loop, an undirected edge and a
/// second label — the shapes whose multiplicity the base case fans out,
/// so a delta that got any of them wrong shows up as a row count.
fn fixture() -> MemoryGraphStore {
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

/// Shapes that reach LTJ, plus a couple that do not, so the switch is
/// shown to be inert outside the join.
const QUERIES: [&str; 12] = [
    "MATCH (x:N)-[:R]->(y:N) RETURN x.id, y.id",
    "MATCH (x:N)-[]->(y:N) RETURN x.id, y.id",
    "MATCH (x:N)-[:T]->(y:N) RETURN x.id, y.id",
    "MATCH (x:N)-[:R]->(y:N)-[:R]->(z:N) RETURN x.id, y.id, z.id",
    "MATCH (x:N)<-[:R]-(y:N) RETURN x.id, y.id",
    "MATCH (x:N)~[:R]~(y:N) RETURN x.id, y.id",
    "MATCH (x:N)-[e]-(y:N) RETURN x.id, y.id",
    "MATCH (x:N)-[]->(y:N), (y:N)-[]->(z:N) RETURN x.id, y.id, z.id",
    "MATCH (x:N)-[e:R]->(y:N) RETURN x.id, y.id",
    "MATCH (x:N)-[]->(y:N) WHERE x.id = 0 RETURN y.id",
    "MATCH (x:N) RETURN x.id",
    "MATCH (x:N)-[:R]->(y:N) RETURN DISTINCT x.id",
];

fn rows(store: &MemoryGraphStore, rt: &Runtime<'_, MemoryGraphStore>) -> Vec<Vec<String>> {
    let _ = store;
    QUERIES
        .iter()
        .map(|q| {
            let query = frogql::compile_query(q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
            let out = match rt.run_query(&query, 0) {
                QueryResult::Projected(r) => r,
                other => panic!("expected a projection, got {other:?}"),
            };
            let mut keys: Vec<String> = out.iter().map(|r| format!("{r:?}")).collect();
            keys.sort();
            keys
        })
        .collect()
}

/// A mutation script, expressed against the store so both sides run the
/// identical edits on their own copy.
type Script = fn(&MemoryGraphStore);

fn node(store: &MemoryGraphStore, which: i64) -> Id {
    *store
        .nodes()
        .iter()
        .find(|&&n| {
            store
                .node_props(n)
                .get("id")
                .is_some_and(|v| *v == Value::Int(which))
        })
        .unwrap_or_else(|| panic!("no node with id {which}"))
}

fn insert_one_edge(s: &MemoryGraphStore) {
    s.insert_edge(
        node(s, 2),
        node(s, 4),
        true,
        LabelType::Label("R".into()),
        Props::new(),
    );
}

fn insert_new_label(s: &MemoryGraphStore) {
    // A label the base index has never seen: its id has to continue the
    // base's numbering, or a predicate constant would mean two things.
    s.insert_edge(
        node(s, 0),
        node(s, 2),
        true,
        LabelType::Label("T".into()),
        Props::new(),
    );
}

fn insert_parallel_edge(s: &MemoryGraphStore) {
    // Same (src, label, tgt) as an existing edge: the base case must fan
    // out across the base's entry *and* the delta's.
    s.insert_edge(
        node(s, 0),
        node(s, 3),
        true,
        LabelType::Label("R".into()),
        Props::new(),
    );
}

fn insert_undirected(s: &MemoryGraphStore) {
    s.insert_edge(
        node(s, 0),
        node(s, 4),
        false,
        LabelType::Label("R".into()),
        Props::new(),
    );
}

fn insert_self_loop(s: &MemoryGraphStore) {
    s.insert_edge(
        node(s, 1),
        node(s, 1),
        true,
        LabelType::Label("R".into()),
        Props::new(),
    );
}

fn delete_one_edge(s: &MemoryGraphStore) {
    s.delete_edge(s.edges_directed()[0]);
}

fn delete_one_of_a_parallel_pair(s: &MemoryGraphStore) {
    // `ad1` and `ad2` share (a, R, d): deleting one must leave exactly one
    // match, which is what a triple-level deletion would get wrong.
    let victim = *s
        .edges_directed()
        .iter()
        .find(|&&e| s.src(e) == node(s, 0) && s.tgt(e) == node(s, 3))
        .unwrap();
    s.delete_edge(victim);
}

fn delete_the_undirected_edge(s: &MemoryGraphStore) {
    s.delete_edge(s.edges_undirected()[0]);
}

fn delete_all_edges_of_a_triple(s: &MemoryGraphStore) {
    for e in s.edges_directed() {
        if s.src(e) == node(s, 0) && s.tgt(e) == node(s, 3) {
            s.delete_edge(e);
        }
    }
}

fn detach_delete_a_node(s: &MemoryGraphStore) {
    s.detach_delete_node(node(s, 3));
}

fn insert_then_delete_the_same_edge(s: &MemoryGraphStore) {
    let e = s.insert_edge(
        node(s, 4),
        node(s, 0),
        true,
        LabelType::Label("R".into()),
        Props::new(),
    );
    s.delete_edge(e);
}

fn insert_node_and_edges(s: &MemoryGraphStore) {
    let n = s.insert_node(LabelType::Label("N".into()), {
        let mut p = Props::new();
        p.insert("id".into(), Value::Int(9));
        p
    });
    s.insert_edge(
        n,
        node(s, 0),
        true,
        LabelType::Label("R".into()),
        Props::new(),
    );
    s.insert_edge(
        node(s, 1),
        n,
        false,
        LabelType::Label("S".into()),
        Props::new(),
    );
}

fn many_small_edits(s: &MemoryGraphStore) {
    // Several statements in a row, each one refreshing the delta. The
    // delta is recomputed from the stamp every time, so this is where a
    // refresh that accumulated instead would double its own triples.
    insert_one_edge(s);
    insert_new_label(s);
    delete_one_edge(s);
    insert_parallel_edge(s);
    delete_the_undirected_edge(s);
}

const SCRIPTS: [(&str, Script); 12] = [
    ("insert_one_edge", insert_one_edge),
    ("insert_new_label", insert_new_label),
    ("insert_parallel_edge", insert_parallel_edge),
    ("insert_undirected", insert_undirected),
    ("insert_self_loop", insert_self_loop),
    ("delete_one_edge", delete_one_edge),
    (
        "delete_one_of_a_parallel_pair",
        delete_one_of_a_parallel_pair,
    ),
    ("delete_the_undirected_edge", delete_the_undirected_edge),
    ("delete_all_edges_of_a_triple", delete_all_edges_of_a_triple),
    ("detach_delete_a_node", detach_delete_a_node),
    (
        "insert_then_delete_the_same_edge",
        insert_then_delete_the_same_edge,
    ),
    ("insert_node_and_edges", insert_node_and_edges),
];

/// Run `script` against a fresh store, warming the index first so there
/// is something to refresh, and calling `invalidate_caches` after each
/// statement exactly as the REPL and the bindings do.
fn run_script(script: Script, delta: bool, repr: &str) -> Vec<Vec<String>> {
    std::env::set_var("FROGQL_LTJ_REPR", repr);
    if delta {
        std::env::remove_var("FROGQL_DISABLE_LTJ_DELTA");
    } else {
        std::env::set_var("FROGQL_DISABLE_LTJ_DELTA", "1");
    }

    let store = fixture();
    let rt = Runtime::new(&store);
    // Warm it: without a cached index there is no delta to maintain, and
    // both sides would be the same rebuild.
    rt.warm_triple_index();
    let _ = rows(&store, &rt);

    script(&store);
    rt.invalidate_caches();
    let out = rows(&store, &rt);

    std::env::remove_var("FROGQL_DISABLE_LTJ_DELTA");
    std::env::remove_var("FROGQL_LTJ_REPR");
    out
}

fn assert_agrees(name: &str, script: Script, repr: &str) {
    let refreshed = run_script(script, true, repr);
    let rebuilt = run_script(script, false, repr);
    assert_eq!(
        refreshed, rebuilt,
        "delta ≠ rebuild after `{name}` on the {repr} index"
    );
}

#[test]
fn delta_equals_rebuild_on_the_compact_index() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    for (name, script) in SCRIPTS {
        assert_agrees(name, script, "compact");
    }
}

#[test]
fn delta_equals_rebuild_on_the_array_index() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    for (name, script) in SCRIPTS {
        assert_agrees(name, script, "array");
    }
}

#[test]
fn delta_equals_rebuild_across_several_statements() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    for repr in ["compact", "array"] {
        assert_agrees("many_small_edits", many_small_edits, repr);
    }
}

/// The premise. If a mutation did not change any answer, the cases above
/// would agree no matter how broken the delta was.
///
/// `insert_then_delete_the_same_edge` is the deliberate exception: its
/// whole claim is that it leaves nothing behind, so it is asserted the
/// other way round.
#[test]
fn the_mutations_actually_move_the_answers() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let before = {
        let store = fixture();
        let rt = Runtime::new(&store);
        rows(&store, &rt)
    };
    for (name, script) in SCRIPTS {
        let after = run_script(script, true, "compact");
        if name == "insert_then_delete_the_same_edge" {
            assert_eq!(
                before, after,
                "an edge inserted and then removed must leave no trace"
            );
        } else {
            assert_ne!(before, after, "`{name}` changed nothing to compare");
        }
    }
}

/// The index must actually be maintained rather than quietly dropped —
/// otherwise the suite above compares a rebuild against a rebuild and
/// proves nothing.
#[test]
fn the_index_survives_a_mutation() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let store = fixture();
    let rt = Runtime::new(&store);
    let before = rt.warm_triple_index();
    let base_triples = before.len();

    insert_one_edge(&store);
    rt.invalidate_caches();

    let after = rt.peek_triple_index().expect(
        "the cached index must survive a mutation; a `None` here means \
         the refresh fell back to a rebuild and the differential cases \
         above are comparing a rebuild against a rebuild",
    );
    assert_eq!(
        after.len(),
        base_triples + 1,
        "the delta must carry exactly the inserted triple"
    );
}
