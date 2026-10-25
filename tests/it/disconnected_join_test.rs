//! A comma join whose operands share no variable is a cartesian product,
//! and every combination is a match.
//!
//! The LTJ decomposes a pattern into `(src, label, tgt)` triples and binds
//! variables by leapfrogging the indexes those triples name. A variable that
//! appears in **no** triple — a standalone node operand joined against a
//! pattern that does have edges — has no iterator to enumerate it, so the
//! search bound it to nothing and the query returned zero rows instead of
//! the product. The hash-join fallback has always handled this shape
//! correctly, so the fix is for the decomposition to decline it.
//!
//! Node-only joins (`(n), (m)`) never reached the LTJ at all — with no
//! triples there is nothing to decompose — which is why the bug only showed
//! up once one side had an edge.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

fn graph() -> MemoryGraphStore {
    // a -[:R]-> b, c -[:R]-> b, plus a disconnected :M node.
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"name": "a"}},
        {"id": "b", "labels": ["N"], "props": {"name": "b"}},
        {"id": "c", "labels": ["N"], "props": {"name": "c"}},
        {"id": "p", "labels": ["M"], "props": {"name": "p"}},
        {"id": "q", "labels": ["M"], "props": {"name": "q"}}
      ],
      "edges": [
        {"id": "e1", "labels": ["R"], "props": {},
         "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "e2", "labels": ["R"], "props": {},
         "endpoints": ["c", "b"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn names(q: &str) -> Vec<Vec<String>> {
    let g = graph();
    let rt = Runtime::new(&g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile {q:?}: {e}"));
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected result for {q:?}, got {other:?}"),
    };
    let mut out: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|v| match v {
                    Value::Str(s) => s.clone(),
                    Value::Null => "-".to_string(),
                    other => format!("{other:?}"),
                })
                .collect()
        })
        .collect();
    out.sort();
    out
}

#[test]
fn edge_pattern_joined_with_a_disconnected_node() {
    // 2 edges × 2 `:M` nodes = 4 rows.
    assert_eq!(
        names("MATCH (x:N)-[:R]->(y:N), (m:M) RETURN x.name AS a, m.name AS b"),
        vec![
            vec!["a".to_string(), "p".to_string()],
            vec!["a".to_string(), "q".to_string()],
            vec!["c".to_string(), "p".to_string()],
            vec!["c".to_string(), "q".to_string()],
        ]
    );
}

#[test]
fn disconnected_node_keeps_its_own_filter() {
    assert_eq!(
        names("MATCH (x:N)-[:R]->(y:N), (m:M) WHERE m.name = 'p' RETURN x.name AS a, m.name AS b"),
        vec![
            vec!["a".to_string(), "p".to_string()],
            vec!["c".to_string(), "p".to_string()],
        ]
    );
}

#[test]
fn two_disconnected_edge_patterns() {
    // Both operands have edges and share nothing: 2 × 2 = 4 rows.
    assert_eq!(
        names("MATCH (x:N)-[:R]->(y:N), (u:N)-[:R]->(v:N) RETURN x.name AS a, u.name AS b"),
        vec![
            vec!["a".to_string(), "a".to_string()],
            vec!["a".to_string(), "c".to_string()],
            vec!["c".to_string(), "a".to_string()],
            vec!["c".to_string(), "c".to_string()],
        ]
    );
}

#[test]
fn connected_join_is_unchanged() {
    // Non-regression: the shape the LTJ exists for still goes through it.
    assert_eq!(
        names("MATCH (x:N)-[:R]->(y:N), (y)-[:R]->(z:N) RETURN x.name AS a, y.name AS b"),
        Vec::<Vec<String>>::new()
    );
    assert_eq!(
        names("MATCH (x:N)-[:R]->(y:N), (z:N)-[:R]->(y) RETURN x.name AS a, z.name AS b"),
        vec![
            vec!["a".to_string(), "a".to_string()],
            vec!["a".to_string(), "c".to_string()],
            vec!["c".to_string(), "a".to_string()],
            vec!["c".to_string(), "c".to_string()],
        ]
    );
}
