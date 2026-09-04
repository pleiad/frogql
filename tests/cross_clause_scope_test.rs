//! Cross-clause and cross-operand variable scope (issue #99).
//!
//! ISO/IEC 39075:2024 §14.3 makes a query a linear sequence of statements
//! where the outgoing working table of one is the incoming working table of
//! the next, so a later `MATCH` sees the variables an earlier one bound
//! (§16.3 SR 25). Within a single graph pattern, an element filter is not
//! evaluated during isolated path matching (§22.3) but over the combined
//! multi-path binding (§16.4 GR 9), and §22.6 widens the search space to
//! that whole binding when a referenced variable is not declared locally —
//! so a path pattern may reference a sibling operand's variables too.
//!
//! froGQL used to reject both: the typechecker checked each clause (and each
//! comma operand's own filter) against its local environment only, and the
//! runtime, with the typechecker bypassed, silently returned no rows.
//!
//! The graph is chosen so the three plausible outcomes are distinguishable:
//!
//! ```text
//!   (a:N {k:'1'})   (b:N {k:'2'})   (c:N {k:'1'})
//! ```
//!
//! `MATCH (n:N) MATCH (m:N {k: n.k})` must produce **5** rows — (a,a), (a,c),
//! (b,b), (c,a), (c,c). An unevaluated correlation gives 0; a correlation
//! dropped altogether gives the 9-row cross product.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["N"], "props": {"name": "a", "k": "1"}},
        {"id": "b", "labels": ["N"], "props": {"name": "b", "k": "2"}},
        {"id": "c", "labels": ["N"], "props": {"name": "c", "k": "1"}},
        {"id": "p", "labels": ["M"], "props": {"name": "p", "k": "1"}}
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

/// Projected rows as `(first column, second column)` strings, sorted, with
/// `NULL` rendered as `-` so a null-extended optional row is visible.
fn pairs(q: &str) -> Vec<(String, String)> {
    let g = graph();
    let rt = Runtime::new(&g);
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile {q:?}: {e}"));
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected result for {q:?}, got {other:?}"),
    };
    let render = |v: &Value| match v {
        Value::Str(s) => s.clone(),
        Value::Null => "-".to_string(),
        other => format!("{other:?}"),
    };
    let mut out: Vec<(String, String)> = rows
        .iter()
        .map(|r| {
            assert_eq!(r.len(), 2, "expected two projected columns for {q:?}");
            (render(&r[0]), render(&r[1]))
        })
        .collect();
    out.sort();
    out
}

fn expected_self_join() -> Vec<(String, String)> {
    let mut v = vec![
        ("a".to_string(), "a".to_string()),
        ("a".to_string(), "c".to_string()),
        ("b".to_string(), "b".to_string()),
        ("c".to_string(), "a".to_string()),
        ("c".to_string(), "c".to_string()),
    ];
    v.sort();
    v
}

// ---------------------------------------------------------------------
// §14.3 — a later MATCH sees the earlier clause's variables.
// ---------------------------------------------------------------------

#[test]
fn later_match_descriptor_references_earlier_clause() {
    assert_eq!(
        pairs("MATCH (n:N) MATCH (m:N {k: n.k}) RETURN n.name AS a, m.name AS b"),
        expected_self_join()
    );
}

#[test]
fn later_match_where_references_earlier_clause() {
    assert_eq!(
        pairs("MATCH (n:N) MATCH (m:N) WHERE m.k = n.k RETURN n.name AS a, m.name AS b"),
        expected_self_join()
    );
}

#[test]
fn three_clause_chain_references_the_first() {
    // The correlation reaches back two clauses, and the middle clause is
    // itself correlated — the whole run of simple clauses is one scope.
    assert_eq!(
        pairs(
            "MATCH (n:N) MATCH (m:N {k: n.k}) MATCH (o:N {k: m.k}) \
             WHERE o.name = m.name RETURN n.name AS a, m.name AS b"
        ),
        expected_self_join()
    );
}

// ---------------------------------------------------------------------
// §16.4 GR 9 + §22.6 — a sibling operand's variables are in scope.
// ---------------------------------------------------------------------

#[test]
fn sibling_operand_descriptor_references_sibling() {
    assert_eq!(
        pairs("MATCH (n:N), (m:N {k: n.k}) RETURN n.name AS a, m.name AS b"),
        expected_self_join()
    );
}

#[test]
fn sibling_operand_descriptor_equals_top_level_where() {
    // The same predicate written the two ways ISO says are equivalent: as an
    // inline element filter and as the graph pattern's WHERE.
    assert_eq!(
        pairs("MATCH (n:N), (m:N {k: n.k}) RETURN n.name AS a, m.name AS b"),
        pairs("MATCH (n:N), (m:N) WHERE m.k = n.k RETURN n.name AS a, m.name AS b"),
    );
}

#[test]
fn sibling_operand_reference_across_a_pattern_with_edges() {
    // The correlated operand is a whole path, not a lone node: `(x)-[:R]->(y)`
    // joined against a node whose key must match `x`'s.
    assert_eq!(
        pairs("MATCH (x:N)-[:R]->(y:N), (m:N {k: x.k}) RETURN x.name AS a, m.name AS b"),
        vec![
            ("a".to_string(), "a".to_string()),
            ("a".to_string(), "c".to_string()),
            ("c".to_string(), "a".to_string()),
            ("c".to_string(), "c".to_string()),
        ]
    );
}

// ---------------------------------------------------------------------
// OPTIONAL MATCH correlated to an outer clause.
// ---------------------------------------------------------------------

#[test]
fn optional_match_descriptor_references_outer_clause() {
    // `p` is the only `:M` node and carries k = '1', so a and c bind it and
    // b is null-extended. A correlation evaluated against the optional's own
    // rows only would null-extend everything.
    assert_eq!(
        pairs("MATCH (n:N) OPTIONAL MATCH (m:M {k: n.k}) RETURN n.name AS a, m.name AS b"),
        vec![
            ("a".to_string(), "p".to_string()),
            ("b".to_string(), "-".to_string()),
            ("c".to_string(), "p".to_string()),
        ]
    );
}

#[test]
fn optional_match_where_references_outer_clause() {
    assert_eq!(
        pairs("MATCH (n:N) OPTIONAL MATCH (m:M) WHERE m.k = n.k RETURN n.name AS a, m.name AS b"),
        vec![
            ("a".to_string(), "p".to_string()),
            ("b".to_string(), "-".to_string()),
            ("c".to_string(), "p".to_string()),
        ]
    );
}

#[test]
fn optional_match_uncorrelated_still_null_extends_nothing() {
    // Non-regression: an optional whose predicate is local keeps binding
    // every row it matched before.
    assert_eq!(
        pairs("MATCH (n:N) OPTIONAL MATCH (m:M) WHERE m.k = '1' RETURN n.name AS a, m.name AS b"),
        vec![
            ("a".to_string(), "p".to_string()),
            ("b".to_string(), "p".to_string()),
            ("c".to_string(), "p".to_string()),
        ]
    );
}

// ---------------------------------------------------------------------
// Non-regression: local scoping is unchanged.
// ---------------------------------------------------------------------

#[test]
fn local_descriptor_filter_still_applies_locally() {
    assert_eq!(
        pairs("MATCH (n:N {k: '1'}), (m:N {k: '2'}) RETURN n.name AS a, m.name AS b"),
        vec![
            ("a".to_string(), "b".to_string()),
            ("c".to_string(), "b".to_string()),
        ]
    );
}

#[test]
fn unknown_variable_is_still_an_error() {
    // Hoisting must not turn a genuine typo into a silently unfiltered
    // query: `zzz` is bound by no clause anywhere.
    let err = compile_query("MATCH (n:N) MATCH (m:N {k: zzz.k}) RETURN n.name AS a, m.name AS b")
        .expect_err("a reference to an unbound variable must not compile");
    assert!(
        err.contains("zzz"),
        "error should name the unbound variable, got: {err}"
    );
}

#[test]
fn correlated_filter_inside_a_union_arm_stays_in_its_arm() {
    // A filter must never float out of a union arm: `(n:N)|(m:N {k:'2'})`
    // filters only the right arm. Hoisting the wrong way would apply it to
    // both (or neither).
    assert_eq!(
        pairs("MATCH (x:N), ((n:N {k: '1'}) | (m:N {k: '2'})) RETURN x.name AS a, x.k AS b").len(),
        // three x bindings × (two k='1' arms + one k='2' arm) = 9 rows
        9
    );
}
