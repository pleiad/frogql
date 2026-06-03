//! Aggregate-function runtime tests (ISO 39075 §20.9).
//!
//! Covers the four shapes the aggregate runtime supports incrementally:
//!  - Commit 4 (this file's first batch): `COUNT(*)`.
//!  - Commit 5: `COUNT(expr)` and `COUNT(DISTINCT expr)`.
//!  - Commit 6: `SUM`, `AVG`, `MIN`, `MAX`.
//!  - Commit 7: implicit GROUP BY (RETURN mixing aggregates with plain exprs).

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

/// Three-user fixture used across most aggregate tests. Two users in
/// Boston, one in Seattle; ages 30/25/40.
fn graph_three_users() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {
            "name": "Alice", "city": "Boston", "age": 30
        }},
        {"id": "u2", "labels": ["User"], "props": {
            "name": "Bob", "city": "Boston", "age": 25
        }},
        {"id": "u3", "labels": ["User"], "props": {
            "name": "Carol", "city": "Seattle", "age": 40
        }}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

/// Helper: compile + run a query and return the projected rows.
fn run(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected result for query {q:?}"),
    }
}

// =======================================================================
// COUNT(*) — ISO §20.9 `COUNT(*)` form (no inner expression, no DISTINCT)
// =======================================================================

#[test]
fn test_count_star_total() {
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(*)"),
        vec![vec![Value::Int(3)]]
    );
}

#[test]
fn test_count_star_no_match_emits_zero_row() {
    // ISO §20.9 GR 7a-i + the implicit-group-by edge case: a pure-
    // aggregate query over zero matches still emits one row with the
    // empty-group result. For COUNT, that's 0 — never the empty table.
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: ImpossibleLabel) RETURN COUNT(*)"),
        vec![vec![Value::Int(0)]]
    );
}

#[test]
fn test_count_star_with_filter() {
    let g = graph_three_users();
    // Two users have age > 25 (Alice 30, Carol 40).
    assert_eq!(
        run(&g, "MATCH (x: User) WHERE x.age > 25 RETURN COUNT(*)"),
        vec![vec![Value::Int(2)]]
    );
}

#[test]
fn test_count_star_with_alias() {
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(*) AS total"),
        vec![vec![Value::Int(3)]]
    );
}

// =======================================================================
// COUNT(expr) — ISO §20.9 <general set function>, kind = Count
//
// Differs from COUNT(*): null inputs are eliminated before counting.
// In gqlite that means "Failure" results from run_expr (e.g. attribute
// not present on the node) drop out instead of counting as 1.
// =======================================================================

#[test]
fn test_count_expr_total() {
    let g = graph_three_users();
    // Every user has a name → COUNT(x.name) == 3.
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(x.name)"),
        vec![vec![Value::Int(3)]]
    );
}

#[test]
fn test_count_expr_skips_failures() {
    // Two of the three users have a "nickname" property; the third
    // doesn't. ISO null-elimination drops that row from COUNT(x.nickname).
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Alice", "nickname": "Al"}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Bob", "nickname": "Bo"}},
        {"id": "u3", "labels": ["User"], "props": {"name": "Carol"}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(x.nickname)"),
        vec![vec![Value::Int(2)]]
    );
}

#[test]
fn test_count_distinct() {
    let g = graph_three_users();
    // Cities: Boston, Boston, Seattle → 2 distinct.
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(DISTINCT x.city)"),
        vec![vec![Value::Int(2)]]
    );
}

#[test]
fn test_count_distinct_with_alias() {
    let g = graph_three_users();
    assert_eq!(
        run(
            &g,
            "MATCH (x: User) RETURN COUNT(DISTINCT x.city) AS unique_cities"
        ),
        vec![vec![Value::Int(2)]]
    );
}

#[test]
fn test_count_distinct_skips_failures() {
    // DISTINCT applies AFTER null-elimination, so missing nickname
    // doesn't count as "a distinct null". Two users have nicknames
    // ("Al" and "Bo"), both distinct → 2.
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"nickname": "Al"}},
        {"id": "u2", "labels": ["User"], "props": {"nickname": "Bo"}},
        {"id": "u3", "labels": ["User"], "props": {}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(DISTINCT x.nickname)"),
        vec![vec![Value::Int(2)]]
    );
}

#[test]
fn test_count_expr_zero_match_emits_zero_row() {
    // Same edge case as COUNT(*) but for COUNT(expr): pure-aggregate
    // query over zero matches still emits one row with count = 0.
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: ImpossibleLabel) RETURN COUNT(x.name)"),
        vec![vec![Value::Int(0)]]
    );
}

#[test]
fn test_count_star_groupby_basic() {
    // Two groups by city (Boston: 2, Seattle: 1) + COUNT(*) per group.
    // The output order is the order the runtime first sees each group key.
    let g = graph_three_users();
    let rs = run(
        &g,
        "MATCH (x: User) GROUP BY x.city RETURN x.city, COUNT(*)",
    );
    assert_eq!(rs.len(), 2);

    // Sort by city for deterministic comparison (insertion order depends
    // on graph node ordering, which is stable but fixture-dependent).
    let mut sorted = rs.clone();
    sorted.sort_by(|a, b| match (&a[0], &b[0]) {
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    });
    assert_eq!(
        sorted,
        vec![
            vec![Value::Str("Boston".into()), Value::Int(2)],
            vec![Value::Str("Seattle".into()), Value::Int(1)],
        ]
    );
}

// =======================================================================
// SUM, AVG, MIN, MAX — ISO §20.9 <general set function>, kinds 7a-iii..vi
//
// Shared with COUNT(expr) on the runtime side: same null-elimination,
// same DISTINCT semantics. Differ only in the reduction step, exercised
// by tests below.
// =======================================================================

#[test]
fn test_sum_int() {
    let g = graph_three_users();
    // Ages: 30 + 25 + 40 = 95.
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN SUM(x.age)"),
        vec![vec![Value::Int(95)]]
    );
}

#[test]
fn test_sum_promotes_to_float_on_mixed() {
    // One user has a float field, two have ints — SUM promotes to Float.
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["P"], "props": {"v": 1}},
        {"id": "b", "labels": ["P"], "props": {"v": 2}},
        {"id": "c", "labels": ["P"], "props": {"v": 3.5}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    assert_eq!(
        run(&g, "MATCH (x: P) RETURN SUM(x.v)"),
        vec![vec![Value::Float(6.5)]]
    );
}

#[test]
fn test_sum_distinct() {
    // values: 30, 30, 40 — DISTINCT keeps 30 once.
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["P"], "props": {"v": 30}},
        {"id": "b", "labels": ["P"], "props": {"v": 30}},
        {"id": "c", "labels": ["P"], "props": {"v": 40}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    assert_eq!(
        run(&g, "MATCH (x: P) RETURN SUM(DISTINCT x.v)"),
        vec![vec![Value::Int(70)]]
    );
}

#[test]
fn test_sum_empty_emits_null() {
    // No matches: pure-aggregate over empty group → ISO null.
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: ImpossibleLabel) RETURN SUM(x.age)"),
        vec![vec![Value::Null]]
    );
}

#[test]
fn test_avg_always_float() {
    let g = graph_three_users();
    // (30 + 25 + 40) / 3 = 31.666...
    let r = run(&g, "MATCH (x: User) RETURN AVG(x.age)");
    assert_eq!(r.len(), 1);
    match &r[0][0] {
        Value::Float(f) => assert!((f - 31.666666_f64).abs() < 1e-3, "got {f}"),
        other => panic!("expected Float, got {other:?}"),
    }
}

#[test]
fn test_avg_empty_emits_null() {
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: ImpossibleLabel) RETURN AVG(x.age)"),
        vec![vec![Value::Null]]
    );
}

#[test]
fn test_min_int() {
    let g = graph_three_users();
    // Ages 30/25/40 → min 25.
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN MIN(x.age)"),
        vec![vec![Value::Int(25)]]
    );
}

#[test]
fn test_max_int() {
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN MAX(x.age)"),
        vec![vec![Value::Int(40)]]
    );
}

#[test]
fn test_min_max_strings() {
    // Strings compare lexicographically.
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN MIN(x.name)"),
        vec![vec![Value::Str("Alice".into())]]
    );
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN MAX(x.name)"),
        vec![vec![Value::Str("Carol".into())]]
    );
}

#[test]
fn test_min_max_empty_emits_null() {
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: ImpossibleLabel) RETURN MIN(x.age)"),
        vec![vec![Value::Null]]
    );
    assert_eq!(
        run(&g, "MATCH (x: ImpossibleLabel) RETURN MAX(x.age)"),
        vec![vec![Value::Null]]
    );
}

#[test]
fn test_aggregates_combined() {
    // All four kinds in one RETURN, plus COUNT(*).
    let g = graph_three_users();
    let rs = run(
        &g,
        "MATCH (x: User) RETURN COUNT(*), SUM(x.age), MIN(x.age), MAX(x.age)",
    );
    assert_eq!(rs.len(), 1);
    assert_eq!(
        rs[0],
        vec![
            Value::Int(3),  // COUNT(*)
            Value::Int(95), // SUM = 30+25+40
            Value::Int(25), // MIN
            Value::Int(40), // MAX
        ]
    );
}

// =======================================================================
// Implicit GROUP BY combinations (Cypher-style: any non-aggregate item
// in RETURN becomes part of the group key automatically)
//
// The framework already supports this — the cases below only exercise
// less-trivial combinations that weren't covered above.
// =======================================================================

/// Sort a result table by its first two columns (Str, Int) for
/// deterministic comparison; the runtime emits groups in insertion
/// order, which depends on graph node ordering.
fn sort_by_first_two(mut rs: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    rs.sort_by(|a, b| match (&a[0], &b[0]) {
        (Value::Str(x), Value::Str(y)) => x.cmp(y).then_with(|| match (&a[1], &b[1]) {
            (Value::Int(p), Value::Int(q)) => p.cmp(q),
            _ => std::cmp::Ordering::Equal,
        }),
        _ => std::cmp::Ordering::Equal,
    });
    rs
}

#[test]
fn test_groupby_with_sum() {
    // Boston: Alice (30) + Bob (25) = 55. Seattle: Carol (40) = 40.
    let g = graph_three_users();
    let rs = sort_by_first_two(run(
        &g,
        "MATCH (x: User) GROUP BY x.city RETURN x.city, SUM(x.age)",
    ));
    assert_eq!(
        rs,
        vec![
            vec![Value::Str("Boston".into()), Value::Int(55)],
            vec![Value::Str("Seattle".into()), Value::Int(40)],
        ]
    );
}

#[test]
fn test_groupby_with_avg() {
    // Boston: avg(30, 25) = 27.5. Seattle: avg(40) = 40.0.
    let g = graph_three_users();
    let rs = sort_by_first_two(run(
        &g,
        "MATCH (x: User) GROUP BY x.city RETURN x.city, AVG(x.age)",
    ));
    assert_eq!(rs.len(), 2);
    match &rs[0][1] {
        Value::Float(f) => assert!((f - 27.5).abs() < 1e-9, "Boston avg: {f}"),
        other => panic!("expected Float, got {other:?}"),
    }
    match &rs[1][1] {
        Value::Float(f) => assert!((f - 40.0).abs() < 1e-9, "Seattle avg: {f}"),
        other => panic!("expected Float, got {other:?}"),
    }
}

#[test]
fn test_groupby_with_min_max_per_group() {
    let g = graph_three_users();
    let rs = sort_by_first_two(run(
        &g,
        "MATCH (x: User) GROUP BY x.city RETURN x.city, MIN(x.age), MAX(x.age)",
    ));
    assert_eq!(
        rs,
        vec![
            vec![
                Value::Str("Boston".into()),
                Value::Int(25), // min
                Value::Int(30), // max
            ],
            vec![Value::Str("Seattle".into()), Value::Int(40), Value::Int(40),],
        ]
    );
}

#[test]
fn test_groupby_two_columns() {
    // Group key has two columns; expect one row per (country, city) pair.
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["U"], "props": {"country": "CL", "city": "Santiago"}},
        {"id": "u2", "labels": ["U"], "props": {"country": "CL", "city": "Santiago"}},
        {"id": "u3", "labels": ["U"], "props": {"country": "CL", "city": "Valparaiso"}},
        {"id": "u4", "labels": ["U"], "props": {"country": "AR", "city": "BuenosAires"}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let rs = sort_by_first_two(run(
        &g,
        "MATCH (x: U) GROUP BY x.country, x.city RETURN x.country, x.city, COUNT(*)",
    ));
    // Three groups: (AR, BuenosAires) → 1, (CL, Santiago) → 2, (CL, Valparaiso) → 1.
    assert_eq!(rs.len(), 3);
    assert_eq!(rs[0][2], Value::Int(1)); // AR/BuenosAires
    assert_eq!(rs[1][2], Value::Int(2)); // CL/Santiago
    assert_eq!(rs[2][2], Value::Int(1)); // CL/Valparaiso
}

#[test]
fn test_groupby_count_distinct_per_group() {
    // Per-city distinct ages: Boston has {30, 25} = 2 distinct;
    // Seattle has {40} = 1.
    let g = graph_three_users();
    let rs = sort_by_first_two(run(
        &g,
        "MATCH (x: User) GROUP BY x.city RETURN x.city, COUNT(DISTINCT x.age)",
    ));
    assert_eq!(
        rs,
        vec![
            vec![Value::Str("Boston".into()), Value::Int(2)],
            vec![Value::Str("Seattle".into()), Value::Int(1)],
        ]
    );
}

#[test]
fn test_groupby_pure_aggregates_no_key() {
    // Confirm that with NO non-aggregate items, the result is exactly one
    // row regardless of how many input rows existed.
    let g = graph_three_users();
    let rs = run(&g, "MATCH (x: User) RETURN COUNT(*), AVG(x.age)");
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0][0], Value::Int(3));
    match &rs[0][1] {
        Value::Float(f) => assert!((f - 31.666666_f64).abs() < 1e-3),
        other => panic!("expected Float, got {other:?}"),
    }
}

// =======================================================================
// Bug-regression: limit and DISTINCT interaction with aggregates
// =======================================================================

#[test]
fn test_count_star_ignores_input_limit() {
    // Regression: passing limit > 0 to run_query used to make
    // run_path_pattern truncate the input rows BEFORE aggregation,
    // so COUNT(*) returned the count of `limit` matched rows instead
    // of all of them. Now the input scan runs unlimited when
    // aggregates are present.
    let g = graph_three_users();
    let query = compile_query("MATCH (x: User) RETURN COUNT(*)").unwrap();
    let rt = Runtime::new(&g);
    match rt.run_query(&query, 1) {
        QueryResult::Projected(rs) => assert_eq!(rs, vec![vec![Value::Int(3)]]),
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_aggregate_groupby_respects_output_limit() {
    // The limit is applied to OUTPUT rows (groups), not input rows.
    // Two cities → two groups; limit=1 truncates to one group.
    let g = graph_three_users();
    let query = compile_query("MATCH (x: User) GROUP BY x.city RETURN x.city, COUNT(*)").unwrap();
    let rt = Runtime::new(&g);
    match rt.run_query(&query, 1) {
        QueryResult::Projected(rs) => assert_eq!(rs.len(), 1),
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_aggregate_no_limit_returns_all_groups() {
    // limit=0 means unlimited: both groups emitted.
    let g = graph_three_users();
    let query = compile_query("MATCH (x: User) GROUP BY x.city RETURN x.city, COUNT(*)").unwrap();
    let rt = Runtime::new(&g);
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => assert_eq!(rs.len(), 2),
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_return_distinct_with_aggregate_dedupes_output() {
    // Regression: RETURN DISTINCT used to be silently ignored when any
    // aggregate appeared in the projection. Now it dedupes the
    // post-aggregation rows.
    //
    // RETURN DISTINCT COUNT(*) over a 4-row graph: the pure-aggregate
    // path produces a single output row [4]; DISTINCT runs over it as
    // a no-op. The point is that the path no longer panics or skips
    // the dedup step.
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["P"], "props": {"city": "X"}},
        {"id": "b", "labels": ["P"], "props": {"city": "Y"}},
        {"id": "c", "labels": ["P"], "props": {"city": "Z"}},
        {"id": "d", "labels": ["P"], "props": {"city": "Z"}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let rt = Runtime::new(&g);

    let q = compile_query("MATCH (x: P) RETURN DISTINCT COUNT(*)").unwrap();
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => assert_eq!(rs, vec![vec![Value::Int(4)]]),
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_compile_query_unchecked_supports_aggregates() {
    // Smoke test: the bypass path used to skip type checking should
    // still produce a runnable Query when aggregates are present.
    let g = graph_three_users();
    let query = gqlrust::compile_query_unchecked("MATCH (x: User) RETURN COUNT(*)").unwrap();
    let rt = Runtime::new(&g);
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => assert_eq!(rs, vec![vec![Value::Int(3)]]),
        _ => panic!("expected projected"),
    }
}

// =======================================================================
// Explicit GROUP BY (ISO §16.15 / Feature GQ15)
//
// gqlite extends the strict standard by accepting arbitrary <expr>s
// (not only <binding variable reference>s) as <grouping element>s; see
// the doc on `Query::group_by` for rationale.
// =======================================================================

#[test]
fn test_groupby_explicit_basic() {
    // Same query as the implicit groupby test, written explicitly.
    let g = graph_three_users();
    let rs = sort_by_first_two(run(
        &g,
        "MATCH (x: User) GROUP BY x.city RETURN x.city, COUNT(*)",
    ));
    assert_eq!(
        rs,
        vec![
            vec![Value::Str("Boston".into()), Value::Int(2)],
            vec![Value::Str("Seattle".into()), Value::Int(1)],
        ]
    );
}

#[test]
fn test_groupby_explicit_lowercase() {
    // Soft-keyword: lowercase `group by` (with whitespace) is also accepted.
    let g = graph_three_users();
    let rs = run(&g, "MATCH (x: User) group by x.city RETURN COUNT(*)");
    assert_eq!(rs.len(), 2); // two cities = two groups
}

#[test]
fn test_implicit_groupby_now_rejected() {
    // Regression: RETURN that mixes a non-aggregate item with an aggregate
    // used to silently use Cypher-style implicit grouping. As of
    // 2026-04-27 (per @mtoro) the checked pipeline rejects this — users
    // must write the GROUP BY clause explicitly.
    let result = compile_query("MATCH (x: User) RETURN x.city, COUNT(*)");
    assert!(result.is_err(), "expected error, got {result:?}");
    let err = result.unwrap_err();
    assert!(
        err.contains("GROUP BY"),
        "expected error mentioning GROUP BY, got: {err}"
    );
}

#[test]
fn test_implicit_groupby_unchecked_still_works() {
    // Escape hatch: compile_query_unchecked bypasses the typechecker
    // and runs the implicit grouping path for users who want it.
    let g = graph_three_users();
    let q = gqlrust::compile_query_unchecked("MATCH (x: User) RETURN x.city, COUNT(*)").unwrap();
    let rt = Runtime::new(&g);
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => assert_eq!(rs.len(), 2), // 2 cities
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_pure_aggregate_no_groupby_still_ok() {
    // A RETURN of only aggregates needs no GROUP BY (no items to group by).
    let g = graph_three_users();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(*)"),
        vec![vec![Value::Int(3)]]
    );
}

#[test]
fn test_pure_projection_no_groupby_still_ok() {
    // A RETURN with no aggregates needs no GROUP BY (nothing to group).
    let g = graph_three_users();
    let rs = run(&g, "MATCH (x: User) RETURN x.name");
    assert_eq!(rs.len(), 3);
}

#[test]
fn test_groupby_explicit_two_keys() {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["U"], "props": {"country": "CL", "city": "Santiago"}},
        {"id": "u2", "labels": ["U"], "props": {"country": "CL", "city": "Santiago"}},
        {"id": "u3", "labels": ["U"], "props": {"country": "CL", "city": "Valparaiso"}},
        {"id": "u4", "labels": ["U"], "props": {"country": "AR", "city": "BuenosAires"}}
      ],
      "edges": []
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let rs = sort_by_first_two(run(
        &g,
        "MATCH (x: U) GROUP BY x.country, x.city RETURN x.country, x.city, COUNT(*)",
    ));
    assert_eq!(rs.len(), 3);
    assert_eq!(rs[0][2], Value::Int(1)); // AR/BuenosAires
    assert_eq!(rs[1][2], Value::Int(2)); // CL/Santiago
    assert_eq!(rs[2][2], Value::Int(1)); // CL/Valparaiso
}

#[test]
fn test_groupby_explicit_rejects_unkeyed_return_item() {
    // ISO/SQL: any non-aggregate RETURN item must appear in GROUP BY.
    // `x.name` is not a grouping key here → typechecker rejects.
    let result = compile_query("MATCH (x: User) GROUP BY x.city RETURN x.name, COUNT(*)");
    assert!(result.is_err(), "expected typecheck error, got {result:?}");
    let err = result.unwrap_err();
    assert!(
        err.contains("GROUP BY") || err.contains("grouping"),
        "expected error message about GROUP BY, got: {err}"
    );
}

#[test]
fn test_groupby_explicit_unchecked_path_still_runs() {
    // The unchecked compile path skips the GROUP-BY-coverage check.
    // Useful for users who know what they're doing or for debugging.
    let g = graph_three_users();
    let q =
        gqlrust::compile_query_unchecked("MATCH (x: User) GROUP BY x.city RETURN x.name, COUNT(*)")
            .unwrap();
    let rt = Runtime::new(&g);
    // Result is implementation-defined when RETURN has unkeyed items
    // (we resolve `x.name` against the first row of each group), but
    // the runtime must not panic.
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rs) => assert_eq!(rs.len(), 2), // 2 cities
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_groupby_explicit_pure_aggregate() {
    // Empty GROUP BY effectively means "single group of all rows", same
    // semantics as a pure-aggregate query. Currently we don't support
    // the `GROUP BY ()` empty-grouping-set ISO syntax, but a query with
    // no GROUP BY and only aggregates produces this single group.
    let g = graph_three_users();
    let rs = run(&g, "MATCH (x: User) RETURN SUM(x.age)");
    assert_eq!(rs, vec![vec![Value::Int(95)]]);
}

// =====================================================================
// GROUP BY position: ISO §14.11 places `<group by clause>` INSIDE the
// `<return statement body>`, after the return item list. Earlier
// gqlite releases used `MATCH ... GROUP BY ... RETURN ...` (legacy);
// the canonical ISO form is `MATCH ... RETURN ... GROUP BY ...`. Both
// positions parse to the same AST; specifying GROUP BY in both is a
// parse error.
// =====================================================================

#[test]
fn test_groupby_canonical_position_after_return_items() {
    let g = graph_three_users();
    let rs = run(
        &g,
        "MATCH (x: User) RETURN x.city, COUNT(*) GROUP BY x.city",
    );
    let mut sorted: Vec<Vec<Value>> = rs;
    sorted.sort_by(|a, b| match (&a[0], &b[0]) {
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    });
    // Same expected output as the legacy form on three_users (2 cities).
    assert_eq!(sorted.len(), 2);
}

#[test]
fn test_groupby_canonical_and_legacy_produce_equal_results() {
    // Pin that the two positions are semantically equivalent.
    let g = graph_three_users();
    let canonical = run(
        &g,
        "MATCH (x: User) RETURN x.city, COUNT(*) GROUP BY x.city",
    );
    let legacy = run(
        &g,
        "MATCH (x: User) GROUP BY x.city RETURN x.city, COUNT(*)",
    );
    let sort_rows = |mut rs: Vec<Vec<Value>>| {
        rs.sort_by(|a, b| match (&a[0], &b[0]) {
            (Value::Str(x), Value::Str(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        });
        rs
    };
    assert_eq!(sort_rows(canonical), sort_rows(legacy));
}

#[test]
fn test_groupby_canonical_with_distinct_and_multi_key() {
    // ISO §14.11: GROUP BY follows the return item list within the
    // RETURN body, even when DISTINCT is set. The clause must still
    // parse and produce the right group keys.
    let q = gqlrust::compile_query_unchecked(
        "MATCH (x: U) RETURN DISTINCT x.country, x.city, COUNT(*) GROUP BY x.country, x.city",
    )
    .unwrap();
    assert!(q.distinct);
    let gb = q.group_by.expect("group_by must be present");
    assert_eq!(gb.len(), 2);
}

#[test]
fn test_groupby_specified_in_both_positions_is_parse_error() {
    let r = gqlrust::compile_query_unchecked(
        "MATCH (x: User) GROUP BY x.city RETURN x.city, COUNT(*) GROUP BY x.city",
    );
    assert!(r.is_err(), "expected parse error, got {r:?}");
    let err = r.unwrap_err();
    assert!(
        err.contains("GROUP BY") && err.contains("only once"),
        "expected error mentioning duplicate GROUP BY, got: {err}"
    );
}

#[test]
fn test_groupby_canonical_with_order_by_and_limit() {
    // Full §14.10 + §14.11 trailing chain: RETURN body (with GROUP BY
    // inside) followed by ORDER BY then LIMIT.
    let g = graph_three_users();
    let rs = run(
        &g,
        "MATCH (x: User) RETURN x.city, COUNT(*) GROUP BY x.city \
         ORDER BY x.city LIMIT 10",
    );
    assert!(rs.len() <= 10);
    assert!(!rs.is_empty());
}

// =======================================================================
// Aggregate arithmetic in RETURN (ISO §20.9): an aggregate as an operand
// of a Binop, e.g. `COUNT(x) + COUNT(y) AS total`. The runtime reduces
// each aggregate over the group, then applies the arithmetic. This is
// what unblocks LDBC IC3's `xCount + yCount AS totalCount`.
// =======================================================================

/// Fixture: two of three users carry a "nickname"; ages 30/25/40.
fn graph_users_with_nicknames() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "u1", "labels": ["User"], "props": {"name": "Alice", "city": "Boston", "age": 30, "nickname": "Al"}},
        {"id": "u2", "labels": ["User"], "props": {"name": "Bob", "city": "Boston", "age": 25, "nickname": "Bo"}},
        {"id": "u3", "labels": ["User"], "props": {"name": "Carol", "city": "Seattle", "age": 40}}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

#[test]
fn test_agg_plus_agg_pure() {
    // COUNT(*) = 3, COUNT(x.nickname) = 2 (one user has no nickname,
    // null-eliminated). Sum = 5.
    let g = graph_users_with_nicknames();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(*) + COUNT(x.nickname)"),
        vec![vec![Value::Int(5)]]
    );
}

#[test]
fn test_agg_minus_agg_pure() {
    // COUNT(*) - COUNT(x.nickname) = 3 - 2 = 1 (the users without a
    // nickname).
    let g = graph_users_with_nicknames();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(*) - COUNT(x.nickname)"),
        vec![vec![Value::Int(1)]]
    );
}

#[test]
fn test_agg_times_constant() {
    // Mixed agg/constant arithmetic: COUNT(*) * 2 = 6.
    let g = graph_users_with_nicknames();
    assert_eq!(
        run(&g, "MATCH (x: User) RETURN COUNT(*) * 2"),
        vec![vec![Value::Int(6)]]
    );
}

#[test]
fn test_agg_plus_agg_with_alias_and_keys() {
    // The IC3 shape: GROUP BY a key, project both component counts and
    // their sum, and verify total == a + b row by row.
    //   Boston: COUNT(*)=2, COUNT(age)=2 → total 4
    //   Seattle: COUNT(*)=1, COUNT(age)=1 → total 2
    let g = graph_users_with_nicknames();
    let rs = run(
        &g,
        "MATCH (x: User) \
         RETURN x.city AS city, COUNT(*) AS c, COUNT(x.age) AS a, \
                COUNT(*) + COUNT(x.age) AS total \
         GROUP BY x.city \
         ORDER BY city ASC",
    );
    assert_eq!(
        rs,
        vec![
            vec![
                Value::Str("Boston".into()),
                Value::Int(2),
                Value::Int(2),
                Value::Int(4),
            ],
            vec![
                Value::Str("Seattle".into()),
                Value::Int(1),
                Value::Int(1),
                Value::Int(2),
            ],
        ]
    );
    // Row-by-row invariant: total == c + a.
    for row in &rs {
        if let (Value::Int(c), Value::Int(a), Value::Int(total)) = (&row[1], &row[2], &row[3]) {
            assert_eq!(*total, *c + *a, "total must equal c + a per group");
        } else {
            panic!("unexpected non-int aggregate columns: {row:?}");
        }
    }
}

#[test]
fn test_agg_distinct_plus_agg_distinct() {
    // COUNT(DISTINCT ...) operands compose the same way (the exact form
    // LDBC IC3 uses: COUNT(DISTINCT messageX) + COUNT(DISTINCT messageY)).
    // Two distinct cities, two distinct ages within Boston's collapsed
    // group when grouped globally: keep it simple with one global group.
    let g = graph_users_with_nicknames();
    // DISTINCT cities = {Boston, Seattle} = 2; DISTINCT nickname = {Al, Bo} = 2.
    assert_eq!(
        run(
            &g,
            "MATCH (x: User) \
             RETURN COUNT(DISTINCT x.city) + COUNT(DISTINCT x.nickname) AS total"
        ),
        vec![vec![Value::Int(4)]]
    );
}
