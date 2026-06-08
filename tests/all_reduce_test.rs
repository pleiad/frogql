//! Tests for Cypher 25 `allReduce` function and early path pruning.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["Station"], "props": {"id": "a", "cost_limit": 6}},
        {"id": "b", "labels": ["Station"], "props": {"id": "b"}},
        {"id": "c", "labels": ["Station"], "props": {"id": "c"}},
        {"id": "d", "labels": ["Station"], "props": {"id": "d"}},
        {"id": "x", "labels": ["Station"], "props": {"id": "x"}},
        {"id": "y", "labels": ["Station"], "props": {"id": "y"}}
      ],
      "edges": [
        {"id": "e1", "labels": ["LINK"], "props": {"cost": 3}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "e2", "labels": ["LINK"], "props": {"cost": 4}, "endpoints": ["b", "c"], "directionality": "->"},
        {"id": "e3", "labels": ["LINK"], "props": {"cost": 5}, "endpoints": ["c", "d"], "directionality": "->"},
        {"id": "e4", "labels": ["LINK"], "props": {"cost": 10}, "endpoints": ["a", "x"], "directionality": "->"},
        {"id": "e5", "labels": ["LINK"], "props": {"cost": 10}, "endpoints": ["x", "y"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn proj(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let query = compile_query(q).unwrap();
    let rt = Runtime::new(g);
    match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("expected projected rows, got {other:?}"),
    }
}

#[test]
fn all_reduce_as_pure_expression() {
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (p:Station {id: 'a'}) RETURN allReduce(sum = 0, x IN [1, 2, 3] | sum + x, sum < 10) AS r",
    );
    // 0 + 1 = 1 (<10, true)
    // 1 + 2 = 3 (<10, true)
    // 3 + 3 = 6 (<10, true)
    // Entire list passes.
    assert_eq!(rows, vec![vec![Value::Bool(true)]]);

    let rows_fail = proj(
        &g,
        "MATCH (p:Station {id: 'a'}) RETURN allReduce(sum = 0, x IN [1, 5, 5] | sum + x, sum < 10) AS r",
    );
    // 0 + 1 = 1 (<10, true)
    // 1 + 5 = 6 (<10, true)
    // 6 + 5 = 11 (not <10, false)
    // List fails at the last step.
    assert_eq!(rows_fail, vec![vec![Value::Bool(false)]]);
}

#[test]
fn all_reduce_path_pruning_under_6() {
    let g = graph();
    // With threshold < 6:
    // a -> b (cost 3, valid)
    // a -> b -> c (cost 7, pruned)
    // a -> x (cost 10, pruned)
    let rows = proj(
        &g,
        "MATCH (a:Station {id: 'a'})-[e:LINK]->{1,3}(n) \
         WHERE allReduce(dist = 0, edge IN e | dist + edge.cost, dist < 6) \
         RETURN n.id AS nid ORDER BY nid ASC",
    );
    assert_eq!(rows, vec![vec![Value::Str("b".into())]]);
}

#[test]
fn all_reduce_path_pruning_under_8() {
    let g = graph();
    // With threshold < 8:
    // a -> b (cost 3, valid)
    // a -> b -> c (cost 7, valid)
    // a -> b -> c -> d (cost 12, pruned)
    // a -> x (cost 10, pruned)
    let rows = proj(
        &g,
        "MATCH (a:Station {id: 'a'})-[e:LINK]->{1,3}(n) \
         WHERE allReduce(dist = 0, edge IN e | dist + edge.cost, dist < 8) \
         RETURN n.id AS nid ORDER BY nid ASC",
    );
    assert_eq!(
        rows,
        vec![vec![Value::Str("b".into())], vec![Value::Str("c".into())]]
    );
}

#[test]
fn all_reduce_path_pruning_with_global_constraint() {
    let g = graph();
    // With threshold < a.cost_limit (which is 6):
    // a -> b (cost 3, valid)
    // a -> b -> c (cost 7, pruned)
    // a -> x (cost 10, pruned)
    let rows = proj(
        &g,
        "MATCH (a:Station {id: 'a'})-[e:LINK]->{1,3}(n) \
         WHERE allReduce(dist = 0, edge IN e | dist + edge.cost, dist < a.cost_limit) \
         RETURN n.id AS nid ORDER BY nid ASC",
    );
    assert_eq!(rows, vec![vec![Value::Str("b".into())]]);
}

#[test]
fn typecheck_all_reduce() {
    // Valid query compiles
    assert!(compile_query(
        "MATCH (a:Station)-[e:LINK]->{1,3}(n) \
         WHERE allReduce(dist = 0, edge IN e | dist + edge.cost, dist < 100) \
         RETURN n"
    )
    .is_ok());

    // Invalid list source should warn/fail typecheck depending on exact compiler strictness
    // (We put a warning in check_expr if source is not a list/star).
    let q = compile_query(
        "MATCH (a:Station) \
         RETURN allReduce(dist = 0, edge IN 123 | dist + edge, dist < 100) AS r"
    );
    assert!(q.is_ok()); // Warnings are not errors, query still compiles.
}

#[test]
fn all_reduce_deep_repetition_pruning() {
    let g = graph();
    // With threshold < 15 and repetition {1,8} (which exceeds MAX_UNROLL (4) and triggers try_concat_with_edge_repetition):
    // a -> b (cost 3, valid)
    // a -> x (cost 10, valid)
    // a -> b -> c (cost 7, valid)
    // a -> x -> y (cost 20, pruned at step 2)
    // a -> b -> c -> d (cost 12, valid)
    let rows = proj(
        &g,
        "MATCH (a:Station {id: 'a'})-[e:LINK]->{1,8}(n) \
         WHERE allReduce(dist = 0, edge IN e | dist + edge.cost, dist < 15) \
         RETURN n.id AS nid ORDER BY nid ASC",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("b".into())],
            vec![Value::Str("c".into())],
            vec![Value::Str("d".into())],
            vec![Value::Str("x".into())],
        ]
    );
}
