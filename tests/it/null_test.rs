//! Tests for the `Value::Null` variant, the `null` literal, and the
//! `IS NULL` / `IS NOT NULL` operators.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// Three users, with Carol missing the `email` property. Used to exercise
/// the missing-attribute path that the engine treats as null.
fn graph_with_optional_email() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {
            "name": "Alice", "email": "alice@example.com"
        }},
        {"id": "u2", "labels": ["User"], "props": {
            "name": "Bob", "email": "bob@example.com"
        }},
        {"id": "u3", "labels": ["User"], "props": {
            "name": "Carol"
        }}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected result for query {q:?}"),
    }
}

#[test]
fn test_is_null_matches_missing_property() {
    let g = graph_with_optional_email();
    let rows = run(&g, "MATCH (x: User) WHERE x.email IS NULL RETURN x.name");
    assert_eq!(rows, vec![vec![Value::Str("Carol".into())]]);
}

#[test]
fn test_is_not_null_matches_present_property() {
    let g = graph_with_optional_email();
    let mut names: Vec<String> = run(
        &g,
        "MATCH (x: User) WHERE x.email IS NOT NULL RETURN x.name",
    )
    .into_iter()
    .map(|row| match &row[0] {
        Value::Str(s) => s.clone(),
        other => panic!("expected Str, got {other:?}"),
    })
    .collect();
    names.sort();
    assert_eq!(names, vec!["Alice", "Bob"]);
}

#[test]
fn test_eq_null_drops_all_rows_under_3vl() {
    // Comparison against null literal yields false → predicate is null →
    // no row passes. This is SQL behavior; users must use IS NULL.
    let g = graph_with_optional_email();
    let rows = run(&g, "MATCH (x: User) WHERE x.email = null RETURN x.name");
    assert!(rows.is_empty(), "expected no rows, got {rows:?}");
}

#[test]
fn test_null_literal_in_return() {
    let g = graph_with_optional_email();
    let rows = run(&g, "MATCH (x: User) RETURN null");
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(row, &vec![Value::Null]);
    }
}

#[test]
fn test_is_null_uppercase() {
    // `NULL` keyword in upper case parses identically.
    let g = graph_with_optional_email();
    let rows = run(&g, "MATCH (x: User) WHERE x.email IS NULL RETURN x.name");
    assert_eq!(rows, vec![vec![Value::Str("Carol".into())]]);
}

#[test]
fn test_empty_aggregate_returns_null_value() {
    // Pure-aggregate over an empty set produces `Value::Null`, not a
    // string sentinel. Spelling: the typechecker accepts the query but
    // the schema rejects ImpossibleLabel; switch to a label that exists
    // and a filter that excludes everything.
    let g = graph_with_optional_email();
    let rows = run(
        &g,
        "MATCH (x: User) WHERE x.name = 'Nobody' RETURN SUM(x.age)",
    );
    assert_eq!(rows, vec![vec![Value::Null]]);
}

// --- 3VL equality through composite values -------------------------------
//
// `[1, null] = [1, null]` unfolds to `1 = 1 AND null = null`, so it is
// *unknown*, not true. Before the fix, list and record equality was the
// derived structural `PartialEq`, which treats `Null == Null` as true and
// answered `true` here. These pin the element-wise 3VL behaviour, including
// the two ways a definite verdict still wins over an inner null: a length /
// key-set mismatch, and a position that definitely disagrees.

/// Helper: evaluate one scalar expression once, over a single-row match.
fn eval1(g: &MemoryGraphStore, expr: &str) -> Value {
    let q = format!("MATCH (x: User) WHERE x.name = 'Alice' RETURN {expr}");
    let rows = run(g, &q);
    assert_eq!(rows.len(), 1, "expected exactly one row for {expr:?}");
    rows[0][0].clone()
}

#[test]
fn test_list_equality_with_inner_null_is_unknown() {
    let g = graph_with_optional_email();
    assert_eq!(eval1(&g, "[1, null] = [1, null]"), Value::Null);
    assert_eq!(eval1(&g, "[1, null] <> [1, null]"), Value::Null);
}

#[test]
fn test_list_equality_without_nulls_is_definite() {
    let g = graph_with_optional_email();
    assert_eq!(eval1(&g, "[1, 2] = [1, 2]"), Value::Bool(true));
    assert_eq!(eval1(&g, "[1, 2] = [1, 3]"), Value::Bool(false));
    assert_eq!(eval1(&g, "[1, 2] <> [1, 3]"), Value::Bool(true));
}

#[test]
fn test_definite_disagreement_beats_inner_null() {
    // One position definitely disagrees, so the whole comparison is false
    // whatever the null position would have said (`AND` with a `false`).
    let g = graph_with_optional_email();
    assert_eq!(eval1(&g, "[1, null] = [2, null]"), Value::Bool(false));
    assert_eq!(eval1(&g, "[1, null] <> [2, null]"), Value::Bool(true));
}

#[test]
fn test_length_mismatch_is_false_not_unknown() {
    // Structure is decided before contents: different lengths can never be
    // equal, so no null inside can make it unknown.
    let g = graph_with_optional_email();
    assert_eq!(eval1(&g, "[1, null] = [1, null, 3]"), Value::Bool(false));
    assert_eq!(eval1(&g, "[null] = []"), Value::Bool(false));
}

#[test]
fn test_nested_list_null_propagates() {
    let g = graph_with_optional_email();
    assert_eq!(
        eval1(&g, "[[1, null], [2]] = [[1, null], [2]]"),
        Value::Null
    );
    assert_eq!(
        eval1(&g, "[[1, null], [2]] = [[1, null], [3]]"),
        Value::Bool(false)
    );
}

#[test]
fn test_record_equality_with_inner_null_is_unknown() {
    let g = graph_with_optional_email();
    assert_eq!(eval1(&g, "{a: 1, b: null} = {a: 1, b: null}"), Value::Null);
    assert_eq!(
        eval1(&g, "{a: 1, b: null} = {a: 2, b: null}"),
        Value::Bool(false)
    );
    // Different key sets are a structural mismatch, decided before contents.
    assert_eq!(eval1(&g, "{a: 1, b: null} = {a: 1}"), Value::Bool(false));
    assert_eq!(eval1(&g, "{a: 1, b: 2} = {a: 1, b: 2}"), Value::Bool(true));
}

#[test]
fn test_unknown_equality_drops_the_row_in_where() {
    // An unknown WHERE is not true, so the row does not survive — the
    // observable consequence of the change for filtering.
    let g = graph_with_optional_email();
    let rows = run(
        &g,
        "MATCH (x: User) WHERE [1, null] = [1, null] RETURN x.name",
    );
    assert!(rows.is_empty(), "unknown filter kept rows: {rows:?}");

    let rows = run(&g, "MATCH (x: User) WHERE [1, 2] = [1, 2] RETURN x.name");
    assert_eq!(rows.len(), 3);
}

#[test]
fn test_in_membership_is_three_valued_over_composites() {
    // `IN` parses only in comparison position (WHERE), not in RETURN, so the
    // verdict is read off row survival. `NOT` separates the two falsy cases:
    // `NOT false` keeps rows, `NOT unknown` is still unknown and drops them.
    let g = graph_with_optional_email();
    let rows = |q: &str| run(&g, q).len();

    // The only candidate matches except at a null: unknown, not a hit.
    assert_eq!(
        rows("MATCH (x: User) WHERE [1, null] IN [[1, null]] RETURN x.name"),
        0
    );
    assert_eq!(
        rows("MATCH (x: User) WHERE NOT ([1, null] IN [[1, null]]) RETURN x.name"),
        0,
        "unknown membership must stay unknown under NOT"
    );

    // A definite hit earlier in the list wins over a later unknown.
    assert_eq!(
        rows("MATCH (x: User) WHERE [1, 2] IN [[1, 2], [1, null]] RETURN x.name"),
        3
    );

    // Every candidate definitely disagrees: definite false, so NOT keeps all.
    assert_eq!(
        rows("MATCH (x: User) WHERE [1, 2] IN [[3, 4]] RETURN x.name"),
        0
    );
    assert_eq!(
        rows("MATCH (x: User) WHERE NOT ([1, 2] IN [[3, 4]]) RETURN x.name"),
        3
    );
}

// --- Type mismatch: error at the top, false inside a composite ------------
//
// Equality has no common domain for `1` and `'a'`. At the top level that is a
// type error; the error does not abort the query, it reduces to an error value
// that propagates outward — the row is dropped in `WHERE`, the cell is null in
// `RETURN`. *Inside* a list the same pair must be `false` instead, because
// there is no domain check down there to report a mismatch against, so the
// element-wise comparison has to be total.

#[test]
fn test_top_level_mismatch_is_an_error_not_false() {
    // A `Failure` reaching `RETURN` becomes a null cell (never `false`), which
    // is how a type error is observable in a projection.
    let g = graph_with_optional_email();
    assert_eq!(eval1(&g, "1 = 'a'"), Value::Null);
    assert_eq!(eval1(&g, "1 <> 'a'"), Value::Null);
    // Not a mismatch: same domain, definite answer.
    assert_eq!(eval1(&g, "1 = 2"), Value::Bool(false));
}

#[test]
fn test_top_level_mismatch_drops_the_row() {
    let g = graph_with_optional_email();
    let rows = run(&g, "MATCH (x: User) WHERE 1 = 'a' RETURN x.name");
    assert!(rows.is_empty(), "a type error must not keep rows: {rows:?}");
    // And negating it does not resurrect them: an error is not `false`.
    let rows = run(&g, "MATCH (x: User) WHERE NOT (1 = 'a') RETURN x.name");
    assert!(
        rows.is_empty(),
        "NOT of an error is still an error: {rows:?}"
    );
}

#[test]
fn test_nested_mismatch_is_false_not_an_error() {
    // The Lean-side totality: `[1] = ['a']` compares element-wise with no
    // domain check available, so the mismatched pair is `false` and the whole
    // comparison is a definite `false` — a real bool, not a null cell.
    let g = graph_with_optional_email();
    assert_eq!(eval1(&g, "[1] = ['a']"), Value::Bool(false));
    assert_eq!(eval1(&g, "[1, 2] = [1, 'a']"), Value::Bool(false));
    assert_eq!(eval1(&g, "{a: 1} = {a: 'x'}"), Value::Bool(false));
}

#[test]
fn test_error_is_not_suppressed_by_a_true_disjunct() {
    // froGQL evaluates both operands before the connective runs, so a type
    // error in *either* disjunct propagates even though `true` is absorbing
    // for OR. The absorbing shortcut is about nulls (`true OR null` is true),
    // not about swallowing errors — ISO §UA004 permits suppressing an error
    // in an inessential position, and froGQL deliberately does not take that
    // latitude (see *Null semantics* in CLAUDE.md).
    let g = graph_with_optional_email();
    let rows = run(&g, "MATCH (x: User) WHERE true OR 1 = 'a' RETURN x.name");
    assert!(
        rows.is_empty(),
        "an inessential type error must still empty the path: {rows:?}"
    );

    // The null it contrasts with: `true OR null` really is true, all rows kept.
    let rows = run(
        &g,
        "MATCH (x: User) WHERE true OR x.email = null RETURN x.name",
    );
    assert_eq!(rows.len(), 3);
}

#[test]
fn test_in_membership_mismatch_is_an_error_unless_a_hit_absorbs_it() {
    let g = graph_with_optional_email();
    // No hit, and one candidate is from another domain: type error.
    let rows = run(&g, "MATCH (x: User) WHERE 1 IN ['a'] RETURN x.name");
    assert!(rows.is_empty());
    // A definite hit is absorbing, so the mismatched candidate never decides.
    let rows = run(&g, "MATCH (x: User) WHERE 1 IN [1, 'a'] RETURN x.name");
    assert_eq!(rows.len(), 3);
}

#[test]
fn test_int_and_float_compare_across_the_numeric_split() {
    // The runtime widens mixed Int/Float everywhere else (`as_num_pair`,
    // `cmp_values`); equality had been left out, which made the pushed-down
    // and residual paths disagree on a float-valued property.
    let g = graph_with_optional_email();
    assert_eq!(eval1(&g, "1 = 1.0"), Value::Bool(true));
    assert_eq!(eval1(&g, "1 <> 1.0"), Value::Bool(false));
    assert_eq!(eval1(&g, "1 = 1.5"), Value::Bool(false));
    assert_eq!(eval1(&g, "[1] = [1.0]"), Value::Bool(true));
}
