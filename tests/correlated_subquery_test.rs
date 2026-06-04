//! Parameter-correlated subquery bodies: an `EXISTS` / `NOT EXISTS` /
//! `VALUE { ... }` body whose WHERE references an *outer* variable that the
//! body's own pattern does not bind. The typechecker threads the outer
//! environment into the body's `Filter` predicates; the runtime evaluates
//! the body per outer row with those parameters bound as an ambient scope.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

/// Three people with ids 1, 2, 3 and no edges.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["P"], "props": {"id": 1}},
        {"id": "b", "labels": ["P"], "props": {"id": 2}},
        {"id": "c", "labels": ["P"], "props": {"id": 3}}
      ],
      "edges": []
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
fn exists_correlated_where_compiles_and_runs() {
    // For every `a` there is an `x` with `x.id = a.id` (namely x = a), so
    // EXISTS holds for all three rows.
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (a:P) WHERE EXISTS { MATCH (x:P) WHERE x.id = a.id } \
         RETURN a.id AS id ORDER BY id",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Int(1)],
            vec![Value::Int(2)],
            vec![Value::Int(3)]
        ]
    );
}

#[test]
fn not_exists_correlated_selects_the_maximum() {
    // `NOT EXISTS { x.id > a.id }` holds only for the largest id.
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (a:P) WHERE NOT EXISTS { MATCH (x:P) WHERE x.id > a.id } \
         RETURN a.id AS id ORDER BY id",
    );
    assert_eq!(rows, vec![vec![Value::Int(3)]]);
}

#[test]
fn value_subquery_correlated_count() {
    // Per `a`, count peers with a strictly greater id: 2, 1, 0.
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (a:P) RETURN a.id AS id, \
         VALUE { MATCH (x:P) WHERE x.id > a.id RETURN COUNT(x) } AS greater \
         ORDER BY id",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Int(1), Value::Int(2)],
            vec![Value::Int(2), Value::Int(1)],
            vec![Value::Int(3), Value::Int(0)],
        ]
    );
}

#[test]
fn value_subquery_correlated_scalar() {
    // Non-aggregate correlated body: the single `x` with `x.id = a.id`
    // projects its id, which equals a.id.
    let g = graph();
    let rows = proj(
        &g,
        "MATCH (a:P) RETURN a.id AS id, \
         VALUE { MATCH (x:P) WHERE x.id = a.id RETURN x.id } AS hi \
         ORDER BY id",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Int(1), Value::Int(1)],
            vec![Value::Int(2), Value::Int(2)],
            vec![Value::Int(3), Value::Int(3)],
        ]
    );
}

#[test]
fn correlated_body_typechecks() {
    // The body's WHERE references the outer `a` — must typecheck (was
    // "Variable a not found in context" before the ambient-env fix).
    assert!(compile_query(
        "MATCH (a:P) WHERE EXISTS { MATCH (x:P) WHERE x.id = a.id } RETURN a.id"
    )
    .is_ok());
}

#[test]
fn uncorrelated_cross_clause_where_still_rejected() {
    // A top-level cross-MATCH-clause WHERE is *not* in scope (the runtime
    // does not evaluate it either), so it must still be a type error.
    assert!(compile_query("MATCH (a:P) MATCH (b:P) WHERE a.id = b.id RETURN a.id").is_err());
}

/// p1 ~knows~ p3 ~knows~ p2; p1 authored a comment replying to a post by
/// p3. Used for the IC14 shape (named path + list comprehension + a VALUE
/// subquery whose WHERE correlates on the outer `path`).
fn ic14_graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "p1", "labels": ["Person"], "props": {"id": 1}},
        {"id": "p2", "labels": ["Person"], "props": {"id": 2}},
        {"id": "p3", "labels": ["Person"], "props": {"id": 3}},
        {"id": "po1", "labels": ["Post"], "props": {"id": 100}},
        {"id": "c1", "labels": ["Comment"], "props": {"id": 200}}
      ],
      "edges": [
        {"id": "k1", "labels": ["knows"], "props": {}, "endpoints": ["p1", "p3"], "directionality": "~~"},
        {"id": "k2", "labels": ["knows"], "props": {}, "endpoints": ["p3", "p2"], "directionality": "~~"},
        {"id": "hc1", "labels": ["hasCreator"], "props": {}, "endpoints": ["po1", "p3"], "directionality": "->"},
        {"id": "hc2", "labels": ["hasCreator"], "props": {}, "endpoints": ["c1", "p1"], "directionality": "->"},
        {"id": "ro1", "labels": ["replyOf"], "props": {}, "endpoints": ["c1", "po1"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

#[test]
fn ic14_shape_runs_end_to_end() {
    // Named path + `[n IN NODES(path) | n.id]` + a parameter-correlated
    // VALUE subquery (`WHERE pe1 IN NODES(path) ...`) summed per path.
    let g = ic14_graph();
    let rows = proj(
        &g,
        "MATCH path = ALL SHORTEST (p1:Person {id: 1})~[:knows]~*(p2:Person {id: 2}) \
         RETURN [n IN NODES(path) | n.id] AS personIdsInPath, \
            SUM( VALUE { \
                MATCH (pe1:Person)~[:knows]~(pe2:Person) \
                WHERE pe1 IN NODES(path) AND pe2 IN NODES(path) AND pe1 <> pe2 \
                OPTIONAL MATCH (pe1)<-[:hasCreator]-(c:Comment)-[:replyOf]->(po:Post)-[:hasCreator]->(pe2) \
                RETURN COUNT(c) * 1.0 } ) AS pathWeight \
         GROUP BY path ORDER BY pathWeight DESC",
    );
    // One shortest path p1->p3->p2; weight 1.0 (the p1->p3 comment reply).
    assert_eq!(
        rows,
        vec![vec![
            Value::List(vec![Value::Int(1), Value::Int(3), Value::Int(2)]),
            Value::Float(1.0)
        ]]
    );
}
