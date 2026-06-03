use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "jan", "labels": ["Person"], "props": {"name": "jan", "birthday": 0, "score": 4}},
        {"id": "feb", "labels": ["Person"], "props": {"name": "feb", "birthday": 2678400000, "score": 5}},
        {"id": "dec", "labels": ["Person"], "props": {"name": "dec", "birthday": -86400000}}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn projected(q: &str) -> Vec<Vec<Value>> {
    let g = graph();
    let rt = Runtime::new(&g);
    let q = compile_query(q).unwrap();
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rows) => rows,
        QueryResult::Raw(_) => panic!("expected projected rows"),
    }
}

#[test]
fn case_when_and_mod_evaluate_in_projection() {
    let rows = projected(
        "MATCH (p:Person)
         RETURN p.name AS name,
                CASE
                  WHEN p.score IS NULL THEN -1
                  WHEN MOD(p.score, 2) = 0 THEN 10
                  ELSE 20
                END AS bucket
         ORDER BY name ASC",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("dec".into()), Value::Int(-1)],
            vec![Value::Str("feb".into()), Value::Int(20)],
            vec![Value::Str("jan".into()), Value::Int(10)],
        ]
    );
}

#[test]
fn case_when_compiles_inside_aggregate() {
    let rows = projected(
        "MATCH (p:Person)
         RETURN SUM(CASE WHEN MOD(p.score, 2) = 0 THEN 1 ELSE -1 END) AS score",
    );
    assert_eq!(rows, vec![vec![Value::Int(-1)]]);
}

#[test]
fn mod_result_keeps_dividend_sign() {
    let rows = projected(
        "MATCH (p:Person)
         RETURN MOD(-5, 3) AS a, MOD(5, -3) AS b, MOD(-5, -3) AS c
         LIMIT 1",
    );
    assert_eq!(
        rows,
        vec![vec![Value::Int(-2), Value::Int(2), Value::Int(-2)]]
    );
}

#[test]
fn casted_return_alias_can_order_projected_rows() {
    let rows = projected(
        "MATCH (p:Person)
         RETURN p.name AS name, p.score AS rawScore
         ORDER BY CAST(rawScore AS INTEGER) DESC, name ASC",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("feb".into()), Value::Int(5)],
            vec![Value::Str("jan".into()), Value::Int(4)],
            vec![Value::Str("dec".into()), Value::Null],
        ]
    );
}

#[test]
fn mod_uses_iso_function_syntax_not_infix_operator() {
    let query = "MATCH (p:Person) RETURN p.score MOD 2 AS remainder";
    assert!(compile_query(query).is_err());
}

#[test]
fn extract_is_not_accepted_as_iso_gql_surface() {
    let query = "MATCH (p:Person) RETURN EXTRACT(MONTH FROM p.birthday) AS month";
    assert!(compile_query(query).is_err());
}

#[test]
fn ldbc_catalog_keeps_ic10_blocked_until_iso_temporals() {
    let toml = std::fs::read_to_string("bench/ldbc-queries/ic10.toml").unwrap();
    assert!(toml.contains("status = \"blocked\""));
    assert!(toml.contains("CASE WHEN"));
    assert!(toml.contains("MOD(a, b)"));
    assert!(toml.contains("CAST(<return-alias> AS INTEGER)"));
    assert!(!toml.contains("query ="));
}

#[test]
fn ldbc_catalog_uses_casted_order_by_where_supported() {
    for ic in [3, 5, 9, 11] {
        let path = format!("bench/ldbc-queries/ic{ic}.toml");
        let toml = std::fs::read_to_string(path).unwrap();
        assert!(!toml.contains("CAST isn't in the parser"));
        assert!(toml.contains("ORDER BY"));
        assert!(toml.contains("CAST("));
    }
}
