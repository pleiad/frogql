//! Regression: an LTJ whose every variable is fixed before the search
//! must still verify that the triples exist.
//!
//! The leapfrog only descends into values the index holds, so in the
//! ordinary search a tuple that reaches the base case is a real match by
//! construction. When the secondary-index constant fold (and/or a
//! caller-supplied pin) fixes *every* variable, there is no leapfrog left
//! to do that checking, and the base case used to emit a row regardless —
//! inventing an edge that does not exist.
//!
//! `(a)-[:follows]->(b) WHERE a.idx = 0 AND b.idx = 3` is the smallest
//! shape that triggers it: both `User.idx` values are unique, so both
//! variables fold to constants.

use std::path::PathBuf;

use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::store::lazy::LazyGraphStore;

/// Four users in a chain: u0 → u1 → u2 → u3. One directory per test:
/// `cargo test` runs a file's tests concurrently in one process, and a
/// shared path would have them delete each other's database mid-run.
fn chain_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("frogql_ltj_all_pinned_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");

    let nodes: Vec<String> = (0..4)
        .map(|u| format!(r#"{{"id":"u{u}","labels":["User"],"props":{{"idx":{u}}}}}"#))
        .collect();
    let edges: Vec<String> = (0..3)
        .map(|u| {
            format!(
                r#"{{"id":"f{u}","labels":["follows"],"props":{{}},"endpoints":["u{u}","u{}"],"directionality":"->"}}"#,
                u + 1
            )
        })
        .collect();
    let json = format!(
        r#"{{"nodes":[{}],"edges":[{}]}}"#,
        nodes.join(","),
        edges.join(",")
    );
    MemoryGraphStore::from_json_str(&json)
        .unwrap()
        .save(&db)
        .unwrap();
    db
}

fn rows(db: &std::path::Path, q: &str) -> Vec<Vec<Value>> {
    let store = LazyGraphStore::open(db).unwrap();
    let rt = Runtime::new(&store);
    let query = frogql::compile_query(q).unwrap_or_else(|e| panic!("compile `{q}`: {e}"));
    match rt.run_query(&query, 0) {
        QueryResult::Projected(r) => r,
        other => panic!("expected a projection, got {other:?}"),
    }
}

#[test]
fn a_fully_pinned_pattern_rejects_an_edge_that_does_not_exist() {
    let db = chain_db("reject");
    let got = rows(
        &db,
        "MATCH (a:User)-[:follows]->(b:User) WHERE a.idx = 0 AND b.idx = 3 RETURN a.idx, b.idx",
    );
    assert!(got.is_empty(), "u0 does not follow u3, got {got:?}");
}

#[test]
fn a_fully_pinned_pattern_still_accepts_an_edge_that_does_exist() {
    let db = chain_db("accept");
    let got = rows(
        &db,
        "MATCH (a:User)-[:follows]->(b:User) WHERE a.idx = 0 AND b.idx = 1 RETURN a.idx, b.idx",
    );
    assert_eq!(got, vec![vec![Value::Int(0), Value::Int(1)]]);
}

#[test]
fn a_fully_pinned_two_hop_checks_both_edges() {
    let db = chain_db("two_hop");
    // u0 → u1 → u2 holds; u0 → u2 → u3 does not (u0 does not follow u2).
    assert_eq!(
        rows(
            &db,
            "MATCH (a:User)-[:follows]->(b:User), (b)-[:follows]->(c:User) \
             WHERE a.idx = 0 AND b.idx = 1 AND c.idx = 2 RETURN a.idx",
        )
        .len(),
        1
    );
    assert!(rows(
        &db,
        "MATCH (a:User)-[:follows]->(b:User), (b)-[:follows]->(c:User) \
         WHERE a.idx = 0 AND b.idx = 2 AND c.idx = 3 RETURN a.idx",
    )
    .is_empty());
}

#[test]
fn the_wrong_direction_is_not_a_match() {
    let db = chain_db("direction");
    // The edge is u0 → u1, so the reverse pair must not match.
    assert!(rows(
        &db,
        "MATCH (a:User)-[:follows]->(b:User) WHERE a.idx = 1 AND b.idx = 0 RETURN a.idx",
    )
    .is_empty());
}

#[test]
fn partially_pinned_patterns_are_unaffected() {
    let db = chain_db("partial");
    // Only `a` folds to a constant here; `b` is still searched. This is
    // the ordinary path, asserted so the fix cannot regress it.
    let got = rows(
        &db,
        "MATCH (a:User)-[:follows]->(b:User) WHERE a.idx = 1 RETURN b.idx",
    );
    assert_eq!(got, vec![vec![Value::Int(2)]]);
}

#[test]
fn an_unpinned_pattern_still_returns_every_edge() {
    let db = chain_db("unpinned");
    let got = rows(
        &db,
        "MATCH (a:User)-[:follows]->(b:User) RETURN a.idx, b.idx",
    );
    assert_eq!(got.len(), 3);
}
