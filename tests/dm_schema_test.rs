//! Phase 6 tests: G2000 graph-type validation on INSERT.
//!
//! When the active GRAPH TYPE is a user-declared schema (not DEFAULT),
//! every inserted node and edge must satisfy that schema; failures get
//! rolled back atomically.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::parser::parse_statement;
use gqlrust::runtime::dm::run_dm;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::statement::Statement;
use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::simple_type::SimpleType;
use gqlrust::typing::variable_type::{Schema, VariableType};

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_dm_phase6");
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

fn person_only_schema() -> Schema {
    let mut props = BTreeMap::new();
    props.insert("name".to_string(), SimpleType::S);
    Schema::from_parts(
        vec![VariableType::Node(DescriptorType::new(
            LabelType::Label("Person".into()),
            PropertyType::Closed(props),
        ))],
        vec![],
    )
}

#[test]
fn insert_under_strict_schema_succeeds_for_matching_shape() {
    let store = fraud_store("schema_ok.gdb");
    let schema = person_only_schema();
    let dm = parse_dm_or_panic("INSERT (a:Person {name: 'Alice'})");
    let exec = run_dm(&store, &dm, Some(&schema)).unwrap();
    assert_eq!(exec.nodes_inserted, 1);
}

#[test]
fn insert_under_strict_schema_rejects_unknown_label() {
    let store = fraud_store("schema_reject_label.gdb");
    let schema = person_only_schema();
    let dm = parse_dm_or_panic("INSERT (b:Robot {name: 'r2d2'})");
    let err = run_dm(&store, &dm, Some(&schema)).unwrap_err();
    assert!(err.contains("G2000"), "expected G2000, got: {err}");
    // Atomicity: rollback must wipe overlay.
    let robots = store.nodes_with_label("Robot");
    assert!(robots.is_none() || robots.unwrap().is_empty());
}

#[test]
fn insert_under_strict_schema_rejects_extra_property() {
    let store = fraud_store("schema_reject_prop.gdb");
    let schema = person_only_schema(); // closed: {name: STR} only
    let dm = parse_dm_or_panic("INSERT (a:Person {name: 'Alice', age: 30})");
    let err = run_dm(&store, &dm, Some(&schema)).unwrap_err();
    assert!(err.contains("G2000"), "expected G2000, got: {err}");
}

#[test]
fn insert_with_no_validation_accepts_anything() {
    // None schema means caller didn't request validation (DEFAULT or no
    // active GRAPH TYPE) — every insert succeeds regardless of shape.
    let store = fraud_store("no_validation.gdb");
    let dm = parse_dm_or_panic("INSERT (b:Robot {make: 'kuka'})");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_inserted, 1);
}
