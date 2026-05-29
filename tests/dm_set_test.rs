//! MVP-1.B tests: ISO §13.3 SET on node and edge properties.
//!
//! Covers <set property item> (`SET x.prop = expr`) and <set all
//! properties item> (`SET x = { prop: expr, ... }` clear+set).

use std::path::{Path, PathBuf};

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::model::value::Value;
use gqlrust::parser::parse_statement;
use gqlrust::runtime::dm::run_dm;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::statement::Statement;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_dm_set");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn fraud_store(name: &str) -> LazyGraphStore {
    let db_path = temp_db(name);
    let json_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    let g = MemoryGraphStore::from_file(&json_path).unwrap();
    g.save(&db_path).unwrap();
    LazyGraphStore::open(&db_path).unwrap()
}

fn parse_dm_or_panic(input: &str) -> gqlrust::syntax::dm::DmStatement {
    match parse_statement(input).unwrap() {
        Statement::DataModification(dm) => dm,
        other => panic!("expected DM, got {other:?}"),
    }
}

#[test]
fn set_property_overwrites_existing_value() {
    let store = fraud_store("set_overwrite.gdb");
    // Pick an Account whose `owner` is Aretha.
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a.owner = 'Renamed'");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_modified, 1);

    // Confirm the read path now returns the new value.
    let id = store
        .nodes_with_label("Account")
        .unwrap()
        .into_iter()
        .find(
            |&n| matches!(store.node_props(n).get("owner"), Some(Value::Str(s)) if s == "Renamed"),
        )
        .expect("renamed account must be visible");
    assert_eq!(
        store.node_props(id).get("owner"),
        Some(&Value::Str("Renamed".into()))
    );
}

#[test]
fn set_property_adds_new_key() {
    let store = fraud_store("set_new_key.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a.note = 'hello'");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_modified, 1);

    let id = store
        .nodes_with_label("Account")
        .unwrap()
        .into_iter()
        .find(|&n| matches!(store.node_props(n).get("owner"), Some(Value::Str(s)) if s == "Aretha"))
        .unwrap();
    assert_eq!(
        store.node_props(id).get("note"),
        Some(&Value::Str("hello".into()))
    );
    // Pre-existing props survive.
    assert_eq!(
        store.node_props(id).get("owner"),
        Some(&Value::Str("Aretha".into()))
    );
}

#[test]
fn set_all_properties_clears_then_sets() {
    // ISO §13.3 GR8 b.i: SET x = { ... } removes every existing prop
    // first, then applies the new map.
    let store = fraud_store("set_all_props.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a = { only: 'thing' }");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_modified, 1);

    // The Account no longer has `owner`; only `only` remains.
    let id = store.nodes_with_label("Account").unwrap()[0];
    let candidates: Vec<_> = store
        .nodes_with_label("Account")
        .unwrap()
        .into_iter()
        .filter(|&n| {
            store
                .node_props(n)
                .get("only")
                .map(|v| matches!(v, Value::Str(s) if s == "thing"))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        candidates.len(),
        1,
        "must find exactly the cleared+set node"
    );
    let target = candidates[0];
    let _ = id;
    let props = store.node_props(target);
    assert_eq!(props.get("only"), Some(&Value::Str("thing".into())));
    assert!(!props.contains_key("owner"), "owner must be cleared");
    assert!(
        !props.contains_key("isBlocked"),
        "isBlocked must be cleared"
    );
}

#[test]
fn set_property_on_edge() {
    let store = fraud_store("set_edge_prop.gdb");
    let dm = parse_dm_or_panic("MATCH ()-[e:Transfer]->() SET e.flag = true");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert!(exec.edges_modified >= 1);

    // Pick any Transfer edge; its `flag` should now be true.
    let some_edge = store
        .directed_edges_with_label("Transfer")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(
        store.edge_props(some_edge).get("flag"),
        Some(&Value::Bool(true))
    );
}

#[test]
fn set_with_attribute_expression_per_binding() {
    let store = fraud_store("set_attr_expr.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account) SET a.last_owner = a.owner");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert!(exec.nodes_modified >= 1);
    // Each Account should now have last_owner == owner.
    for id in store.nodes_with_label("Account").unwrap() {
        let props = store.node_props(id);
        assert_eq!(props.get("last_owner"), props.get("owner"));
    }
}

#[test]
fn set_multiple_items_in_one_statement() {
    let store = fraud_store("set_multi.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a.x = 1, a.y = 2");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(
        exec.nodes_modified, 2,
        "two SET items, one binding row each"
    );

    let id = store
        .nodes_with_label("Account")
        .unwrap()
        .into_iter()
        .find(|&n| matches!(store.node_props(n).get("owner"), Some(Value::Str(s)) if s == "Aretha"))
        .unwrap();
    assert_eq!(store.node_props(id).get("x"), Some(&Value::Int(1)));
    assert_eq!(store.node_props(id).get("y"), Some(&Value::Int(2)));
}
