//! Seeded repetition traversal (`Runtime::try_concat_with_edge_repetition`).
//!
//! `Concat(left, (edge){lb,ub})` expands level-by-level from the left
//! rows instead of materializing every walk in the graph and joining
//! (issue #57). The seeded path must produce results identical to the
//! legacy route it shortcuts; these tests pin known answers and
//! differentially compare the two paths by toggling
//! `FROGQL_DISABLE_SEEDED_REPEAT`.
//!
//! Note the anonymous directed / `~`-undirected repeats are unrolled by
//! the optimizer before either runtime path sees them; the seeded path
//! serves named-variable repeats (all orientations) and anonymous
//! any-direction repeats (excluded from unroll — LTJ cannot decompose
//! the unrolled arms).

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// Small graph exercising the corners the traversal must preserve:
/// a directed chain with a branch, a reciprocal pair (a↔b), a directed
/// self-loop (c→c), and an undirected edge (d~u).
/// ids: a=0, b=1, c=2, d=3, x=4, y=5, u=6.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"id": 0}},
        {"id": "b", "labels": ["N"], "props": {"id": 1}},
        {"id": "c", "labels": ["N"], "props": {"id": 2}},
        {"id": "d", "labels": ["N"], "props": {"id": 3}},
        {"id": "x", "labels": ["N"], "props": {"id": 4}},
        {"id": "y", "labels": ["N"], "props": {"id": 5}},
        {"id": "u", "labels": ["N"], "props": {"id": 6}}
      ],
      "edges": [
        {"id": "ab", "labels": ["R"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "ba", "labels": ["R"], "props": {}, "endpoints": ["b", "a"], "directionality": "->"},
        {"id": "bc", "labels": ["R"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"},
        {"id": "cc", "labels": ["R"], "props": {}, "endpoints": ["c", "c"], "directionality": "->"},
        {"id": "cd", "labels": ["R"], "props": {}, "endpoints": ["c", "d"], "directionality": "->"},
        {"id": "ax", "labels": ["R"], "props": {}, "endpoints": ["a", "x"], "directionality": "->"},
        {"id": "xy", "labels": ["R"], "props": {}, "endpoints": ["x", "y"], "directionality": "->"},
        {"id": "du", "labels": ["R"], "props": {}, "endpoints": ["d", "u"], "directionality": "~~"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// Serializes the `FROGQL_DISABLE_SEEDED_REPEAT` toggle: the var is
/// process-global, so concurrent tests would otherwise clobber each other.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run_proj(g: &MemoryGraphStore, q: &str, seeded: bool, limit: usize) -> Vec<Vec<Value>> {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if seeded {
        std::env::remove_var("FROGQL_DISABLE_SEEDED_REPEAT");
    } else {
        std::env::set_var("FROGQL_DISABLE_SEEDED_REPEAT", "1");
    }
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    let out = match rt.run_query(&query, limit) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected for {q:?}"),
    };
    std::env::remove_var("FROGQL_DISABLE_SEEDED_REPEAT");
    out
}

/// Multiset comparison: both paths may produce rows in different orders.
fn canonical(rows: &[Vec<Value>]) -> Vec<String> {
    let mut keys: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
    keys.sort();
    keys
}

fn assert_differential(q: &str) {
    let g = graph();
    let seeded = run_proj(&g, q, true, 0);
    let legacy = run_proj(&g, q, false, 0);
    assert_eq!(
        canonical(&seeded),
        canonical(&legacy),
        "seeded ≠ legacy for {q}"
    );
}

// ===== Differential: seeded ≡ legacy =====

#[test]
fn differential_any_direction_named() {
    // The issue-#57 shape: named edge var, any-direction, bounded repeat,
    // selective left filter.
    assert_differential("MATCH (n1:N)-[e]-{1,3}(n2:N) WHERE n1.id = 0 RETURN n1.id, e, n2.id");
}

#[test]
fn differential_any_direction_anonymous() {
    // An anonymous (unused-edge) any-direction repeat now UNROLLS into a
    // Union whose arms run through the mirrored LTJ (issue #71), so it no
    // longer reaches the seeded traversal — both toggle settings agree
    // trivially. Kept as a smoke test that the query still executes; the
    // seeded path proper is covered by the named-edge cases below.
    assert_differential("MATCH (n1:N)-[]-{1,2}(n2:N) WHERE n1.id = 0 RETURN n1.id, n2.id");
}

#[test]
fn differential_directed_right_named() {
    assert_differential("MATCH (n1:N)-[e]->{1,2}(n2:N) WHERE n1.id = 0 RETURN n1.id, e, n2.id");
}

#[test]
fn differential_directed_left_named() {
    assert_differential("MATCH (n1:N)<-[e]-{1,2}(n2:N) WHERE n1.id = 2 RETURN n1.id, e, n2.id");
}

#[test]
fn differential_undirected_named() {
    assert_differential("MATCH (n1:N)~[e]~{1,2}(n2:N) WHERE n1.id = 3 RETURN n1.id, e, n2.id");
}

#[test]
fn differential_lower_bound_zero() {
    // lb == 0: each seed row must appear once with an empty Group binding.
    assert_differential("MATCH (n1:N)-[e]->{0,2}(n2:N) WHERE n1.id = 0 RETURN n1.id, e, n2.id");
}

#[test]
fn differential_lower_bound_two() {
    // lb > 1: levels below lb feed the frontier but not the output.
    assert_differential("MATCH (n1:N)-[e]-{2,3}(n2:N) WHERE n1.id = 0 RETURN n1.id, e, n2.id");
}

#[test]
fn differential_self_loop_heavy() {
    // Seeds sitting on the directed self-loop c→c: every level revisits
    // the loop edge (WALK semantics allow repeated edges).
    assert_differential("MATCH (n1:N)-[e]-{1,3}(n2:N) WHERE n1.id = 2 RETURN n1.id, e, n2.id");
}

#[test]
fn differential_no_seed_filter() {
    // No WHERE: every node seeds. Same multiset either way.
    assert_differential("MATCH (n1:N)-[e]->{1,2}(n2:N) RETURN n1.id, e, n2.id");
}

#[test]
fn differential_with_limit() {
    // A limit smaller than the total result count: both paths must
    // return exactly `limit` rows (the specific rows may differ — only
    // the count is comparable).
    let g = graph();
    let q = "MATCH (n1:N)-[e]-{1,3}(n2:N) WHERE n1.id = 0 RETURN n1.id, n2.id LIMIT 4";
    let seeded = run_proj(&g, q, true, 0);
    let legacy = run_proj(&g, q, false, 0);
    assert_eq!(seeded.len(), 4);
    assert_eq!(legacy.len(), 4);
}

// ===== Pinned answers (independent of the legacy path) =====

#[test]
fn pinned_issue_57_shape() {
    // From a=0, any-direction, lengths 1..3, WALK semantics.
    // Length 1: a-b (ab fwd), a-b (ba bwd), a-x (ax fwd).
    let g = graph();
    let rows = run_proj(
        &g,
        "MATCH (n1:N)-[e]-{1,1}(n2:N) WHERE n1.id = 0 RETURN n2.id",
        true,
        0,
    );
    let mut got: Vec<i64> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Int(v) => *v,
            other => panic!("expected Int, got {other:?}"),
        })
        .collect();
    got.sort();
    assert_eq!(got, vec![1, 1, 4]);
}

#[test]
fn pinned_group_binding_grows_per_level() {
    // Directed 2-hop from a: a→b→c (ab,bc), a→b→a (ab,ba), a→x→y (ax,xy),
    // a→b... reciprocal: also a→b via ba? ba is b→a, not usable forward
    // from a. So exactly: [ab,bc], [ab,ba], [ax,xy].
    let g = graph();
    let rows = run_proj(
        &g,
        "MATCH (n1:N)-[e]->{2,2}(n2:N) WHERE n1.id = 0 RETURN e, n2.id",
        true,
        0,
    );
    assert_eq!(rows.len(), 3);
    for r in &rows {
        match &r[0] {
            Value::List(edges) => assert_eq!(edges.len(), 2, "group must hold 2 edges"),
            other => panic!("expected List group, got {other:?}"),
        }
    }
    let mut targets: Vec<i64> = rows
        .iter()
        .map(|r| match &r[1] {
            Value::Int(v) => *v,
            other => panic!("expected Int, got {other:?}"),
        })
        .collect();
    targets.sort();
    assert_eq!(targets, vec![0, 2, 5]);
}

#[test]
fn pinned_zero_hop_binds_empty_group() {
    let g = graph();
    let rows = run_proj(
        &g,
        "MATCH (n1:N)-[e]->{0,1}(n2:N) WHERE n1.id = 5 RETURN e, n2.id",
        true,
        0,
    );
    // y=5 has no outgoing edges: only the zero-hop row (n2 = n1).
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::List(vec![]));
    assert_eq!(rows[0][1], Value::Int(5));
}
