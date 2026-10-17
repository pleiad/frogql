//! A mixed match chain (one with an OPTIONAL or a §16.6 prefix somewhere)
//! is compiled with every maximal run of consecutive plain `MATCH` clauses
//! collapsed into one comma-join, so the LTJ evaluates the run as a unit.
//!
//! Each case checks the compiled shape and that the rows equal the
//! clause-by-clause evaluation of the same chain left uncollapsed.

use frogql::compile_query;
use frogql::elaborate::elaborate_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::parser;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::syntax::query::{MatchStatement, Query};

/// People who know each other in a cycle with a chord, plus their posts:
/// Alice→Bob→Dani→Alice and Alice→Carla→Ana→Dani; Alice wrote Hola and
/// Chao, Carla wrote GQL!, Robo (a Bot) wrote Beep.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "alice", "labels": ["Person"], "props": {"name": "Alice", "age": 30}},
        {"id": "bob", "labels": ["Person"], "props": {"name": "Bob", "age": 25}},
        {"id": "carla", "labels": ["Person", "Student"], "props": {"name": "Carla", "age": 22}},
        {"id": "dani", "labels": ["Person"], "props": {"name": "Dani"}},
        {"id": "ana", "labels": ["Person"], "props": {"name": "Ana", "age": 41}},
        {"id": "robo", "labels": ["Bot"], "props": {"name": "Robo"}},
        {"id": "p1", "labels": ["Post"], "props": {"content": "Hola"}},
        {"id": "p2", "labels": ["Post"], "props": {"content": "Chao"}},
        {"id": "p3", "labels": ["Post"], "props": {"content": "GQL!"}},
        {"id": "p4", "labels": ["Post"], "props": {"content": "Beep"}}
      ],
      "edges": [
        {"id": "k1", "labels": ["KNOWS"], "props": {}, "endpoints": ["alice", "bob"], "directionality": "->"},
        {"id": "k2", "labels": ["KNOWS"], "props": {}, "endpoints": ["bob", "dani"], "directionality": "->"},
        {"id": "k3", "labels": ["KNOWS"], "props": {}, "endpoints": ["dani", "alice"], "directionality": "->"},
        {"id": "k4", "labels": ["KNOWS"], "props": {}, "endpoints": ["alice", "carla"], "directionality": "->"},
        {"id": "k5", "labels": ["KNOWS"], "props": {}, "endpoints": ["carla", "ana"], "directionality": "->"},
        {"id": "k6", "labels": ["KNOWS"], "props": {}, "endpoints": ["ana", "dani"], "directionality": "->"},
        {"id": "c1", "labels": ["CREATED"], "props": {}, "endpoints": ["alice", "p1"], "directionality": "->"},
        {"id": "c2", "labels": ["CREATED"], "props": {}, "endpoints": ["alice", "p2"], "directionality": "->"},
        {"id": "c3", "labels": ["CREATED"], "props": {}, "endpoints": ["carla", "p3"], "directionality": "->"},
        {"id": "c4", "labels": ["CREATED"], "props": {}, "endpoints": ["robo", "p4"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// `S` for a plain MATCH, `O` for an OPTIONAL one, in chain order.
fn shape(q: &Query) -> String {
    q.matches
        .iter()
        .map(|m| match m {
            MatchStatement::Simple { .. } => 'S',
            MatchStatement::Optional { .. } => 'O',
        })
        .collect()
}

fn rows(g: &MemoryGraphStore, q: &Query) -> Vec<String> {
    let mut out: Vec<String> = match Runtime::new(g).run_query(q, 0) {
        QueryResult::Projected(rs) => rs.iter().map(|r| format!("{r:?}")).collect(),
        _ => panic!("expected a projected result"),
    };
    out.sort();
    out
}

/// Compile `q` and assert its chain shape, then check the rows against the
/// same chain run clause by clause (elaborated, never collapsed).
fn check(q: &str, expected_shape: &str) -> usize {
    let g = graph();
    let collapsed = compile_query(q).unwrap();
    assert_eq!(shape(&collapsed), expected_shape, "shape of {q}");
    let reference = elaborate_query(parser::parse_query(q).unwrap());
    let got = rows(&g, &collapsed);
    assert_eq!(got, rows(&g, &reference), "rows of {q}");
    got.len()
}

#[test]
fn run_before_optional_is_one_clause() {
    let n = check(
        "MATCH (a:Person)-[:KNOWS]->(b) MATCH (b)-[:KNOWS]->(c) \
         OPTIONAL MATCH (c)-[:CREATED]->(p) RETURN a.name, b.name, c.name, p.content",
        "SO",
    );
    // Seven two-hop chains; the two ending at Alice fan out over her two
    // posts, the one ending at Carla finds GQL!, the rest get a null.
    assert_eq!(n, 9);
}

#[test]
fn run_after_leading_optional_is_one_clause() {
    let n = check(
        "OPTIONAL MATCH (x:Bot) MATCH (a:Person)-[:KNOWS]->(b) MATCH (b)-[:KNOWS]->(c) \
         RETURN x.name, a.name, b.name, c.name",
        "OS",
    );
    assert_eq!(n, 7);
}

#[test]
fn optional_between_runs_stays_a_boundary() {
    let n = check(
        "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:CREATED]->(p) \
         MATCH (a)-[:KNOWS]->(b) MATCH (b)-[:KNOWS]->(c) \
         RETURN a.name, p.content, b.name, c.name",
        "SOS",
    );
    assert_eq!(n, 4);
}

#[test]
fn where_on_a_later_clause_reads_an_earlier_variable() {
    let n = check(
        "MATCH (a:Person)-[:KNOWS]->(b) MATCH (b)-[:KNOWS]->(c) WHERE c.age > a.age \
         OPTIONAL MATCH (c)-[:CREATED]->(p) RETURN a.name, b.name, c.name, p.content",
        "SO",
    );
    // Alice→Carla→Ana (41 > 30) and Bob→Dani→Alice (30 > 25), the latter
    // over Alice's two posts.
    assert_eq!(n, 3);
}

#[test]
fn three_clause_run_closes_a_triangle() {
    let n = check(
        "MATCH (a:Person)-[:KNOWS]->(b) MATCH (b)-[:KNOWS]->(c) MATCH (c)-[:KNOWS]->(a) \
         OPTIONAL MATCH (a)-[:CREATED]->(p) RETURN a.name, b.name, c.name, p.content",
        "SO",
    );
    // Alice→Bob→Dani→Alice, entered from each of its three nodes; Alice's
    // rotation fans out over her two posts.
    assert_eq!(n, 4);
}

#[test]
fn prefixed_clause_is_not_merged() {
    let n = check(
        "MATCH ANY SHORTEST (a:Person {name: 'Alice'})-[:KNOWS]->+(d:Person {name: 'Dani'}) \
         MATCH (d)-[:KNOWS]->(e) MATCH (e)-[:KNOWS]->(f) \
         OPTIONAL MATCH (f)-[:CREATED]->(p) RETURN a.name, d.name, e.name, f.name, p.content",
        "SSO",
    );
    assert_eq!(n, 2);
}

#[test]
fn shared_edge_variable_starts_a_new_run() {
    // The comma-join does not unify an edge variable repeated across its
    // operands, so this clause stays separate and is hash-joined on `e`.
    let n = check(
        "MATCH (a:Person)-[e:KNOWS]->(b) MATCH (x)-[e]->(y) \
         OPTIONAL MATCH (b)-[:CREATED]->(p) RETURN a.name, b.name, x.name, y.name, p.content",
        "SSO",
    );
    // One row per KNOWS edge; Dani→Alice fans out over Alice's two posts.
    assert_eq!(n, 7);
}

#[test]
fn shared_group_variable_starts_a_new_run() {
    let n = check(
        "MATCH (a:Person {name: 'Alice'})-[e:KNOWS]->{1,2}(b) MATCH (x)-[e]->{1,2}(y) \
         OPTIONAL MATCH (b)-[:CREATED]->(p) RETURN a.name, b.name, x.name, y.name",
        "SSO",
    );
    assert!(n > 0);
}
