//! Regression test for the LTJ label-disjunction filter bug.
//!
//! Setup (`test_data/ltj_label_disjunction.json`):
//!     Person  p1  {id=1}
//!     Comment c1  {id=100}
//!     Comment c2  {id=101}
//!     Post    po1 {id=200}
//!     Forum   f1  {id=999}
//!
//!     c1 -[:hasCreator]-> p1
//!     c2 -[:hasCreator]-> p1
//!     po1 -[:hasCreator]-> p1
//!     f1 -[:hasModerator]-> p1     -- NOT :hasCreator
//!
//! The bug:
//!   `MATCH (p:Person)<-[:hasCreator]-(m: Comment | Post) RETURN m.id`
//!   should return exactly {100, 101, 200} (the three Comments/Posts
//!   pointing at p1 via :hasCreator). With the bug, the disjunction
//!   `(m: Comment | Post)` doesn't filter anything in the LTJ join —
//!   the count would equal the unconstrained `(m)` count, and a
//!   subsequent `WHERE m.id = 999` (the Forum's id) would return the
//!   Forum even though it has no :hasCreator edge to p1.

use std::path::Path;

use gqlrust::compile_query;
use gqlrust::model::graph::Graph;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;

fn graph() -> Graph {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/ltj_label_disjunction.json");
    Graph::from_file(&p).unwrap()
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

/// Sanity: single-label match works. (Comment) finds the two Comments.
#[test]
fn single_label_comment_finds_only_comments() {
    let ids = run_ids("MATCH (p: Person)<-[:hasCreator]-(m: Comment) RETURN m.id");
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(sorted, vec![100, 101], "got: {:?}", ids);
}

/// Sanity: single-label (Post) match works.
#[test]
fn single_label_post_finds_only_posts() {
    let ids = run_ids("MATCH (p: Person)<-[:hasCreator]-(m: Post) RETURN m.id");
    assert_eq!(ids, vec![200]);
}

/// The bug: label disjunction `(m: Comment | Post)` should return
/// exactly the Comments + Posts. With the bug, it returns the same as
/// unconstrained `(m)` — including non-Comment/Post nodes if any are
/// reachable. Here, only the three Comments+Posts are connected via
/// :hasCreator, so the bug manifests as a count mismatch ONLY when a
/// non-matching node is also in the candidate set.
///
/// We construct that candidate set explicitly via `WHERE m.id = 999`
/// — Forum 999 has no :hasCreator edge to p1, so this query MUST
/// return zero rows. With the bug, it returns the Forum.
#[test]
fn disjunction_does_not_admit_forum_via_id_lookup() {
    let ids = run_ids(
        "MATCH (p: Person)<-[:hasCreator]-(m: Comment | Post) WHERE m.id = 999 RETURN m.id",
    );
    assert!(
        ids.is_empty(),
        "label disjunction (Comment | Post) should NOT admit Forum 999 \
         (no :hasCreator edge to p1); got ids: {:?}",
        ids
    );
}

/// Direct count form: `(m: Comment | Post)` should match exactly 3
/// rows (the 3 Comments+Posts), NOT 4 (which would include the Forum
/// only if the bug were ALSO ignoring the edge type — which it isn't).
/// Here the spec count is 3.
#[test]
fn disjunction_matches_exactly_comment_post_count() {
    let g = graph();
    let r = Runtime::new(&g);
    let q =
        compile_query("MATCH (p: Person)<-[:hasCreator]-(m: Comment | Post) RETURN m.id").unwrap();
    let result = r.run_query(&q, 0);
    let rows = match result {
        gqlrust::runtime::result::QueryResult::Projected(rs) => rs,
        _ => panic!(),
    };
    assert_eq!(
        rows.len(),
        3,
        "expected 3 (Comments + Posts via :hasCreator); got {} rows: {:?}",
        rows.len(),
        rows
    );
}
