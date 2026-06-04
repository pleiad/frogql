//! GROUP BY a RETURN alias. ISO restricts a `<grouping element>` to a
//! binding-variable reference; froGQL additionally lowers the common
//! convenience form `... <expr> AS k ... GROUP BY k` to the underlying
//! expression during elaboration, so both the typechecker's functional
//! dependency check and the runtime grouping see a key evaluable over the
//! binding table. A binding variable shadows any same-named alias.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

/// Three people across two cities; ages chosen so `age + age` collides
/// only within a city group.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "p1", "labels": ["Person"], "props": {"city": "NYC", "age": 30}},
        {"id": "p2", "labels": ["Person"], "props": {"city": "NYC", "age": 40}},
        {"id": "p3", "labels": ["Person"], "props": {"city": "LA",  "age": 30}}
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

#[test]
fn group_by_attribute_alias() {
    let g = graph();
    let rows = run(
        &g,
        "MATCH (n:Person) RETURN n.city AS city, COUNT(n) AS c \
         GROUP BY city ORDER BY city ASC",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Str("LA".into()), Value::Int(1)],
            vec![Value::Str("NYC".into()), Value::Int(2)],
        ]
    );
}

#[test]
fn group_by_alias_matches_spelled_out_expression() {
    // The alias form must produce identical grouping to spelling the
    // expression out in GROUP BY (the form that already worked).
    let g = graph();
    let by_alias = run(
        &g,
        "MATCH (n:Person) RETURN n.city AS city, COUNT(n) AS c \
         GROUP BY city ORDER BY city ASC",
    );
    let by_expr = run(
        &g,
        "MATCH (n:Person) RETURN n.city AS city, COUNT(n) AS c \
         GROUP BY n.city ORDER BY n.city ASC",
    );
    assert_eq!(by_alias, by_expr);
}

#[test]
fn group_by_computed_alias() {
    // `(age + age) AS doubled ... GROUP BY doubled`: NYC has ages 30 and 40
    // → doubled 60 and 80 (two groups); LA has 30 → 60.
    let g = graph();
    let rows = run(
        &g,
        "MATCH (n:Person) RETURN (n.age + n.age) AS doubled, COUNT(n) AS c \
         GROUP BY doubled ORDER BY doubled ASC",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Int(60), Value::Int(2)], // p1 (NYC) + p3 (LA)
            vec![Value::Int(80), Value::Int(1)], // p2 (NYC)
        ]
    );
}

#[test]
fn binding_variable_shadows_alias() {
    // `city` is both a (would-be) alias and a binding variable here: the
    // binding variable wins, so grouping is by node identity — three groups.
    let g = graph();
    let rows = run(
        &g,
        "MATCH (city:Person) RETURN city.age AS a, COUNT(city) AS c \
         GROUP BY city ORDER BY a ASC, c ASC",
    );
    // Three distinct nodes → three groups, each count 1.
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r[1] == Value::Int(1)));
}

#[test]
fn group_by_unknown_name_still_errors() {
    let r = compile_query("MATCH (n:Person) RETURN n.city AS city GROUP BY nope");
    assert!(r.is_err());
}

#[test]
fn group_by_aggregate_alias_is_rejected() {
    // Grouping by an aggregate result is not a valid grouping key.
    let r = compile_query("MATCH (n:Person) RETURN COUNT(n) AS c GROUP BY c");
    assert!(r.is_err());
}
