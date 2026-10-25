//! ISO §4.16.6 / §20.27 temporal values — DATE and LOCAL_DATETIME MVP.
//!
//! Query-time values only in this phase: constructors from a string or a
//! field record, total ordering within each type, ORDER BY / GROUP BY
//! support, and datetime + integer-millis arithmetic (the form the
//! `DURATION({...})` desugaring produces). ZONED types, typed durations
//! and property storage are the documented next steps.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

fn dated_nodes() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "n1", "labels": ["Ev"], "props": {"d": "2011-03-15", "name": "b"}},
        {"id": "n2", "labels": ["Ev"], "props": {"d": "2010-01-05", "name": "a"}},
        {"id": "n3", "labels": ["Ev"], "props": {"d": "2010-01-05", "name": "c"}}
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
        other => panic!("expected projected rows, got {other:?}"),
    }
}

// ---------------------------------------------------------- constructors ---

#[test]
fn date_from_string_displays_iso() {
    let g = dated_nodes();
    let rows = run(&g, "MATCH (x:Ev) WHERE x.name = 'a' RETURN DATE(x.d) AS d");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].to_string(), "2010-01-05");
    assert!(matches!(rows[0][0], Value::Date(_)));
}

#[test]
fn date_from_record_equals_date_from_string() {
    let g = dated_nodes();
    let rows = run(
        &g,
        "MATCH (x:Ev) WHERE x.name = 'a' \
         RETURN DATE({year: 2010, month: 1, day: 5}) = DATE(x.d) AS eq",
    );
    assert_eq!(rows, vec![vec![Value::Bool(true)]]);
}

#[test]
fn local_datetime_from_string_roundtrips() {
    let g = dated_nodes();
    let rows = run(
        &g,
        "MATCH (x:Ev) WHERE x.name = 'a' \
         RETURN LOCAL_DATETIME('2010-01-05T08:30:15') AS t",
    );
    assert_eq!(rows[0][0].to_string(), "2010-01-05T08:30:15");
}

#[test]
fn malformed_date_string_degrades_to_null() {
    // Failure-as-null, the same convention as CAST('hola' AS INTEGER):
    // 2023 is not a leap year, so Feb 29 does not exist.
    let g = dated_nodes();
    let rows = run(
        &g,
        "MATCH (x:Ev) WHERE x.name = 'a' RETURN DATE('2023-02-29') AS d",
    );
    assert_eq!(rows, vec![vec![Value::Null]]);
}

#[test]
fn leap_day_is_a_valid_date() {
    let g = dated_nodes();
    let rows = run(
        &g,
        "MATCH (x:Ev) WHERE x.name = 'a' RETURN DATE('2024-02-29') AS d",
    );
    assert_eq!(rows[0][0].to_string(), "2024-02-29");
}

#[test]
fn bare_date_name_stays_usable_as_variable() {
    // Soft keyword: only the call form is special.
    let g = dated_nodes();
    let rows = run(&g, "MATCH (date:Ev) WHERE date.name = 'a' RETURN date.d");
    assert_eq!(rows, vec![vec![Value::Str("2010-01-05".into())]]);
}

// ------------------------------------------------- comparison + ordering ---

#[test]
fn dates_compare_chronologically_in_where() {
    let g = dated_nodes();
    let rows = run(
        &g,
        "MATCH (x:Ev) WHERE DATE(x.d) < DATE('2011-01-01') RETURN x.name",
    );
    assert_eq!(rows.len(), 2, "only the two 2010 events pass the filter");
}

#[test]
fn order_by_date_sorts_chronologically() {
    let g = dated_nodes();
    let rows = run(
        &g,
        "MATCH (x:Ev) RETURN x.name AS n, DATE(x.d) AS d ORDER BY d DESC, n ASC",
    );
    let names: Vec<String> = rows.iter().map(|r| r[0].to_string()).collect();
    assert_eq!(names, vec!["\"b\"", "\"a\"", "\"c\""]);
}

#[test]
fn group_by_date_collapses_equal_dates() {
    let g = dated_nodes();
    let rows = run(
        &g,
        "MATCH (x:Ev) RETURN DATE(x.d) AS d, COUNT(*) AS c \
         GROUP BY DATE(x.d) ORDER BY c DESC",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Int(2), "two events share 2010-01-05");
}

#[test]
fn date_and_datetime_do_not_cross_compare() {
    // Distinct temporal types: every comparison operator yields false,
    // so the filter drops the row (3VL-style).
    let g = dated_nodes();
    let rows = run(
        &g,
        "MATCH (x:Ev) WHERE x.name = 'a' \
         AND DATE('2010-01-05') = LOCAL_DATETIME('2010-01-05T00:00:00') \
         RETURN x.name",
    );
    assert!(rows.is_empty());
}

// ----------------------------------------------------------- arithmetic ---

#[test]
fn datetime_plus_duration_millis_advances_a_day() {
    let g = dated_nodes();
    let rows = run(
        &g,
        "MATCH (x:Ev) WHERE x.name = 'a' \
         RETURN LOCAL_DATETIME('2010-01-05T23:30:00') + DURATION({days: 1}) AS t",
    );
    assert_eq!(rows[0][0].to_string(), "2010-01-06T23:30:00");
}

// ----------------------------------------------------------- typechecker ---

#[test]
fn typecheck_accepts_date_as_sort_key() {
    // Temporal types are base types: orderable per §22.14 without GA04.
    let r = compile_query("MATCH (x:Ev) RETURN DATE(x.d) AS d ORDER BY d");
    assert!(r.is_ok(), "got: {:?}", r.err());
}

#[test]
fn typecheck_rejects_provably_wrong_constructor_argument() {
    let r = compile_query("MATCH (x:Ev) RETURN DATE(true) AS d");
    let err = r.expect_err("DATE(bool) must be a type error");
    assert!(err.contains("DATE"), "got: {err}");
}
