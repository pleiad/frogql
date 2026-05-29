//! LTJ label-disjunction filter — regression: a label-union descriptor
//! (`(m: A | B)`) joined through an edge must constrain `m` to nodes
//! whose label set actually overlaps `{A, B}`. Anonymous-edge patterns
//! must not lose their label during WHERE-pushdown.
//!
//! Fixture (`test_data/ltj_label_disjunction.json`):
//!     Person  p1  {id=1}
//!     Comment c1  {id=100}, c2 {id=101}
//!     Post    po1 {id=200}
//!     Forum   f1  {id=999}
//!
//!     c1 / c2 / po1  -[:hasCreator]->   p1
//!     f1             -[:hasModerator]-> p1     // NOT :hasCreator

use std::path::Path;

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;

fn graph() -> MemoryGraphStore {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/ltj_label_disjunction.json");
    MemoryGraphStore::from_file(&p).unwrap()
}

fn run_ids(query: &str) -> Vec<i64> {
    let g = graph();
    let r = Runtime::new(&g);
    let q = compile_query(query).unwrap();
    let result = r.run_query(&q, 0);
    let rows = match result {
        gqlrust::runtime::result::QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected result"),
    };
    rows.into_iter()
        .map(|row| match &row[0] {
            Value::Int(n) => *n,
            other => panic!("expected Int in row, got {:?}", other),
        })
        .collect()
}

#[test]
fn single_label_comment_finds_only_comments() {
    let ids = run_ids("MATCH (p: Person)<-[:hasCreator]-(m: Comment) RETURN m.id");
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(sorted, vec![100, 101]);
}

#[test]
fn single_label_post_finds_only_posts() {
    let ids = run_ids("MATCH (p: Person)<-[:hasCreator]-(m: Post) RETURN m.id");
    assert_eq!(ids, vec![200]);
}

/// Forum 999 has no :hasCreator edge to p1, so the label-union descriptor
/// must reject it even though its `id` matches the WHERE bound.
#[test]
fn disjunction_does_not_admit_forum_via_id_lookup() {
    let ids = run_ids(
        "MATCH (p: Person)<-[:hasCreator]-(m: Comment | Post) WHERE m.id = 999 RETURN m.id",
    );
    assert!(ids.is_empty(), "got ids: {:?}", ids);
}

#[test]
fn disjunction_matches_exactly_comment_post_count() {
    let ids = run_ids("MATCH (p: Person)<-[:hasCreator]-(m: Comment | Post) RETURN m.id");
    assert_eq!(ids.len(), 3, "expected 3 Comments+Posts; got {:?}", ids);
}
