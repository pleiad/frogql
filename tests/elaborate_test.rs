//! Tests for the elaboration phase: `{k: v}` value filters inside descriptors
//! are hoisted into WHERE clauses, and `name is T` is accepted as a synonym for
//! `name: T` (type ascription).

use std::path::Path;

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

fn fraud() -> MemoryGraphStore {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    MemoryGraphStore::from_file(&p).unwrap()
}

fn rows(q: &str) -> Vec<Vec<Value>> {
    let g = fraud();
    let rt = Runtime::new(&g);
    let query = compile_query(q).unwrap();
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected result"),
    }
}

#[test]
fn test_value_filter_hoisted_to_where() {
    // `{owner: 'Aretha'}` should match exactly the Aretha account.
    let r = rows("MATCH (x: Account {owner: 'Aretha'}) RETURN x.owner");
    assert_eq!(r, vec![vec![Value::Str("Aretha".into())]]);
}

#[test]
fn test_value_filter_and_type_in_same_descriptor() {
    // Mix type ascription (`is`) and value filter (`:`) in one descriptor.
    let r = rows("MATCH (x: Account {owner str, isBlocked: true}) RETURN x.owner");
    assert_eq!(r, vec![vec![Value::Str("Mike".into())]]);
}

#[test]
fn test_value_filter_anonymous_node_gets_fresh_var() {
    // No variable — elaboration must invent one so the WHERE can name it.
    let r = rows("MATCH (: Account {owner: 'Scott'}) RETURN 1");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0], vec![Value::Int(1)]);
}

#[test]
fn test_legacy_type_ascription_with_colon_still_works() {
    // Backward compat: `{isDummy bool}` means type ascription, not value filter.
    let r = rows("MATCH (x: Dummy {isDummy bool}) RETURN x.isDummy");
    assert_eq!(r.len(), 1);
}

#[test]
fn test_value_filter_combines_with_existing_where() {
    // An explicit WHERE and a value filter should AND together, matching Mike only.
    let r = rows("MATCH (x: Account {owner: 'Mike'}) WHERE x.isBlocked = true RETURN x.owner");
    assert_eq!(r, vec![vec![Value::Str("Mike".into())]]);
}

#[test]
fn test_is_type_ascription_only() {
    // Pure type ascription form with `is`, no values.
    let r = rows("MATCH (x: Account {owner str}) RETURN x.owner");
    assert_eq!(r.len(), 4); // every Account has a str-valued owner
}
