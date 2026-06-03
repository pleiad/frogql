//! Named path patterns (`MATCH p = ...`) and the §20.16 path functions.
//!
//! Covers the parse of the path-variable declaration, the materialized
//! `Value::Path`, and the path functions `ELEMENTS` / `PATH_LENGTH` /
//! `CARDINALITY` (ISO §20.16) plus the non-standard `NODES` / `EDGES`
//! translation helpers. Unblocks the LDBC IC1 / IC13 / IC14 path-function
//! cluster.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

/// A `knows` chain `Alice -> Bob -> Carol`, plus a side friend `Dave`
/// reachable only from Alice. Edge JSON mirrors the format used by the
/// other runtime tests: `{id, labels, props, endpoints, directionality}`.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["Person"], "props": {"name": "Alice"}},
        {"id": "b", "labels": ["Person"], "props": {"name": "Bob"}},
        {"id": "c", "labels": ["Person"], "props": {"name": "Carol"}},
        {"id": "d", "labels": ["Person"], "props": {"name": "Dave"}}
      ],
      "edges": [
        {"id": "e1", "labels": ["knows"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "e2", "labels": ["knows"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"},
        {"id": "e3", "labels": ["knows"], "props": {}, "endpoints": ["a", "d"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile failed: {e}\nquery: {q}"));
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected rows, got {other:?}"),
    }
}

#[test]
fn test_named_path_parses() {
    // The declaration must parse and typecheck under the star schema.
    assert!(
        compile_query("MATCH p = (a:Person)-[:knows]->(b:Person) RETURN PATH_LENGTH(p)").is_ok()
    );
}

#[test]
fn test_path_length_single_edge() {
    let g = graph();
    // Alice -> Dave is a one-edge path; PATH_LENGTH counts edges.
    let rows = run(
        &g,
        "MATCH p = (a:Person)-[:knows]->(b:Person) WHERE a.name = 'Alice' AND b.name = 'Dave' \
         RETURN PATH_LENGTH(p) AS len",
    );
    assert_eq!(rows, vec![vec![Value::Int(1)]]);
}

#[test]
fn test_path_length_two_edges() {
    let g = graph();
    // Alice -> Bob -> Carol is a two-edge path.
    let rows = run(
        &g,
        "MATCH p = (a:Person)-[:knows]->(b:Person)-[:knows]->(c:Person) \
         WHERE a.name = 'Alice' \
         RETURN PATH_LENGTH(p) AS len, CARDINALITY(p) AS card",
    );
    // 2 edges; cardinality = 3 nodes + 2 edges = 5.
    assert_eq!(rows, vec![vec![Value::Int(2), Value::Int(5)]]);
}

#[test]
fn test_elements_shape() {
    let g = graph();
    // ELEMENTS yields the alternating node/edge sequence in match order.
    let rows = run(
        &g,
        "MATCH p = (a:Person)-[:knows]->(b:Person) WHERE a.name = 'Alice' AND b.name = 'Dave' \
         RETURN ELEMENTS(p) AS els",
    );
    assert_eq!(rows.len(), 1);
    let Value::List(items) = &rows[0][0] else {
        panic!("ELEMENTS must project a list, got {:?}", rows[0][0]);
    };
    // [Node, Edge, Node].
    assert_eq!(items.len(), 3);
    assert!(matches!(items[0], Value::Node(_)));
    assert!(matches!(items[1], Value::Edge(_)));
    assert!(matches!(items[2], Value::Node(_)));
}

#[test]
fn test_nodes_and_edges_projections() {
    let g = graph();
    let rows = run(
        &g,
        "MATCH p = (a:Person)-[:knows]->(b:Person)-[:knows]->(c:Person) \
         WHERE a.name = 'Alice' \
         RETURN NODES(p) AS ns, EDGES(p) AS es",
    );
    assert_eq!(rows.len(), 1);
    let Value::List(ns) = &rows[0][0] else {
        panic!("NODES must project a list");
    };
    let Value::List(es) = &rows[0][1] else {
        panic!("EDGES must project a list");
    };
    // 3 nodes, 2 edges; every element carries the right reference kind.
    assert_eq!(ns.len(), 3);
    assert_eq!(es.len(), 2);
    assert!(ns.iter().all(|v| matches!(v, Value::Node(_))));
    assert!(es.iter().all(|v| matches!(v, Value::Edge(_))));
}

#[test]
fn test_named_path_under_any_shortest() {
    let g = graph();
    // The named path binds the selected path. ANY SHORTEST over unbounded
    // repetition makes the search finite; the shortest Alice→Carol walk has
    // length 2.
    let rows = run(
        &g,
        "MATCH p = ANY SHORTEST (a:Person)-[:knows]->+(c:Person) \
         WHERE a.name = 'Alice' AND c.name = 'Carol' \
         RETURN PATH_LENGTH(p) AS len",
    );
    assert_eq!(rows, vec![vec![Value::Int(2)]]);
}

#[test]
fn test_path_var_projected_bare() {
    let g = graph();
    // `RETURN p` projects the whole path as a `Value::Path`.
    let rows = run(
        &g,
        "MATCH p = (a:Person)-[:knows]->(b:Person) WHERE a.name = 'Alice' AND b.name = 'Dave' \
         RETURN p",
    );
    assert_eq!(rows.len(), 1);
    let Value::Path(items) = &rows[0][0] else {
        panic!(
            "bare path var must project a Value::Path, got {:?}",
            rows[0][0]
        );
    };
    assert_eq!(items.len(), 3); // node, edge, node
}

#[test]
fn test_path_function_on_non_path_is_type_error() {
    // ELEMENTS over a node variable is a hard type error: `n` types as a
    // node, and `meet(node, path)` is empty.
    let err = compile_query("MATCH (n:Person) RETURN ELEMENTS(n)").unwrap_err();
    assert!(
        err.contains("PATH"),
        "expected a PATH-argument type error, got: {err}"
    );
}

#[test]
fn test_path_var_is_usable_as_plain_variable_name_elsewhere() {
    // `nodes` / `elements` etc. are soft keywords: only `NAME(` is special.
    // A node variable literally named `elements` still parses as a variable.
    let g = graph();
    let rows = run(
        &g,
        "MATCH (elements:Person) WHERE elements.name = 'Alice' RETURN elements.name AS n",
    );
    assert_eq!(rows, vec![vec![Value::Str("Alice".into())]]);
}
