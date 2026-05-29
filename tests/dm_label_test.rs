//! MVP-1.D tests: ISO §13.3 / §13.4 SET / REMOVE labels (Feature GD02).
//!
//! Covers `<set label item>` (`SET x:Label`, `SET x IS Label`) and
//! `<remove label item>` (`REMOVE x:Label`, `REMOVE x IS Label`) on
//! both nodes and edges, the idempotency of repeated ops, label-index
//! coherence after the overlay applies, and G2000 enforcement when an
//! active GRAPH TYPE rejects the post-mutation shape.

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
use gqlrust::typing::variable_type::{Schema, VariableType};

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_dm_label");
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

fn label_strings_for(store: &LazyGraphStore, id: u32) -> Vec<String> {
    MemoryGraphStore::label_strings(&store.node_labels(id))
}

#[test]
fn set_label_adds_new_label_to_base_node() {
    let store = fraud_store("set_label_base.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a:VIP");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_modified, 1);

    let id = store
        .nodes_with_label("VIP")
        .expect("VIP label must now resolve")
        .into_iter()
        .next()
        .unwrap();
    let labels = label_strings_for(&store, id);
    assert!(labels.contains(&"Account".to_string()));
    assert!(labels.contains(&"VIP".to_string()));
    // Base index for Account still surfaces the same node.
    assert!(store.nodes_with_label("Account").unwrap().contains(&id));
}

#[test]
fn set_label_is_keyword_alias() {
    // ISO §13.3 GR8 c: `SET x IS Label` is a synonym of `SET x:Label`.
    let store = fraud_store("set_label_is.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Scott'}) SET a IS VIP");
    run_dm(&store, &dm, None).unwrap();
    let vips = store.nodes_with_label("VIP").unwrap();
    assert_eq!(vips.len(), 1);
}

#[test]
fn set_label_is_idempotent() {
    let store = fraud_store("set_label_idempotent.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a:Account");
    run_dm(&store, &dm, None).unwrap();
    let id = store
        .nodes_with_label("Account")
        .unwrap()
        .into_iter()
        .find(|&n| {
            matches!(
                store.node_props(n).get("owner"),
                Some(gqlrust::model::value::Value::Str(s)) if s == "Aretha"
            )
        })
        .unwrap();
    let labels = label_strings_for(&store, id);
    // Account appears exactly once; no duplicates.
    assert_eq!(
        labels.iter().filter(|l| *l == "Account").count(),
        1,
        "duplicate Account label after idempotent SET: {labels:?}"
    );
}

#[test]
fn remove_label_drops_label_from_base_node() {
    let store = fraud_store("remove_label_base.gdb");
    // Node `d1` has both Dummy and Person labels.
    let dm = parse_dm_or_panic("MATCH (p:Dummy) REMOVE p:Dummy");
    run_dm(&store, &dm, None).unwrap();

    // Dummy label index no longer returns this node.
    let dummies = store.nodes_with_label("Dummy");
    assert!(
        dummies.as_ref().map(|v| v.is_empty()).unwrap_or(true),
        "Dummy index should be empty after REMOVE: {dummies:?}"
    );
    // Person remains.
    let people = store.nodes_with_label("Person").unwrap();
    assert_eq!(people.len(), 1);
    let labels = label_strings_for(&store, people[0]);
    assert!(!labels.contains(&"Dummy".to_string()));
    assert!(labels.contains(&"Person".to_string()));
}

#[test]
fn remove_label_is_idempotent_when_label_absent() {
    // ISO §13.4 GR4 b: removing a label the element does not carry is a no-op.
    let store = fraud_store("remove_label_absent.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Jay'}) REMOVE a:NotPresent");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_modified, 1);
    let id = store
        .nodes_with_label("Account")
        .unwrap()
        .into_iter()
        .find(|&n| {
            matches!(
                store.node_props(n).get("owner"),
                Some(gqlrust::model::value::Value::Str(s)) if s == "Jay"
            )
        })
        .unwrap();
    let labels = label_strings_for(&store, id);
    assert_eq!(labels, vec!["Account".to_string()]);
}

#[test]
fn set_then_remove_label_cancels_out() {
    // SET and REMOVE land in separate statements (multi-DML deferred to v2).
    let store = fraud_store("set_remove_cancel.gdb");
    let dm_set = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a:VIP");
    run_dm(&store, &dm_set, None).unwrap();
    let dm_rem = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) REMOVE a:VIP");
    run_dm(&store, &dm_rem, None).unwrap();
    let vips = store.nodes_with_label("VIP");
    assert!(
        vips.as_ref().map(|v| v.is_empty()).unwrap_or(true),
        "VIP must not survive a SET+REMOVE pair: {vips:?}"
    );
}

#[test]
fn set_label_on_freshly_inserted_node() {
    // Multi-DML chains (`INSERT ... SET ...` in one statement) are out of
    // scope until v2 per the plan, so the test runs the two ops as
    // separate statements. The second statement matches the new node by
    // its property and exercises the overlay-allocated path of
    // `add_node_label`.
    let store = fraud_store("set_label_new.gdb");
    let dm_insert = parse_dm_or_panic("INSERT (n:Person {name: 'Alice'})");
    let exec = run_dm(&store, &dm_insert, None).unwrap();
    assert_eq!(exec.nodes_inserted, 1);

    let dm_set = parse_dm_or_panic("MATCH (n:Person {name: 'Alice'}) SET n:Employee");
    let exec = run_dm(&store, &dm_set, None).unwrap();
    assert_eq!(exec.nodes_modified, 1);

    let employees = store.nodes_with_label("Employee").unwrap();
    assert_eq!(employees.len(), 1);
    let labels = label_strings_for(&store, employees[0]);
    assert!(labels.contains(&"Person".to_string()));
    assert!(labels.contains(&"Employee".to_string()));
}

#[test]
fn set_label_on_edge() {
    let store = fraud_store("set_label_edge.gdb");
    let dm = parse_dm_or_panic("MATCH ()-[t:Transfer]->() SET t:Audited");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert!(exec.edges_modified > 0);
    let audited = store
        .directed_edges_with_label("Audited")
        .expect("Audited edge index must populate after SET");
    assert!(!audited.is_empty());
}

#[test]
fn remove_all_labels_emits_star() {
    // Node `a1` only has the Account label. After REMOVE, the label
    // type collapses to Star (the empty-set encoding).
    let store = fraud_store("remove_all_labels.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) REMOVE a:Account");
    run_dm(&store, &dm, None).unwrap();
    // The Account index should no longer return this node, but every
    // other Account survives.
    let remaining = store.nodes_with_label("Account").unwrap();
    for &id in &remaining {
        let labels = label_strings_for(&store, id);
        assert!(!labels.is_empty(), "id {id} got stripped accidentally");
    }
    // The unlabeled node is reachable via a full scan.
    let unlabeled = store
        .nodes()
        .into_iter()
        .find(|&id| matches!(store.node_labels(id), LabelType::Star))
        .expect("at least one node should have Star (empty) labels now");
    assert_eq!(
        store.node_props(unlabeled).get("owner"),
        Some(&gqlrust::model::value::Value::Str("Aretha".into()))
    );
}

fn account_or_vip_schema() -> Schema {
    let account = VariableType::Node(DescriptorType::new(
        LabelType::Label("Account".into()),
        PropertyType::Open(BTreeMap::new()),
    ));
    let vip = VariableType::Node(DescriptorType::new(
        LabelType::And(
            Box::new(LabelType::Label("Account".into())),
            Box::new(LabelType::Label("VIP".into())),
        ),
        PropertyType::Open(BTreeMap::new()),
    ));
    let person = VariableType::Node(DescriptorType::new(
        LabelType::Label("Person".into()),
        PropertyType::Open(BTreeMap::new()),
    ));
    let dummy_person = VariableType::Node(DescriptorType::new(
        LabelType::And(
            Box::new(LabelType::Label("Dummy".into())),
            Box::new(LabelType::Label("Person".into())),
        ),
        PropertyType::Open(BTreeMap::new()),
    ));
    // SET / REMOVE on nodes only validates the node side of the schema,
    // so the edge list can stay empty without affecting these tests.
    Schema::from_parts(vec![account, vip, person, dummy_person], vec![])
}

#[test]
fn set_label_under_strict_schema_succeeds_when_combo_is_allowed() {
    let store = fraud_store("set_label_schema_ok.gdb");
    let schema = account_or_vip_schema();
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a:VIP");
    run_dm(&store, &dm, Some(&schema)).unwrap();
}

#[test]
fn set_label_under_strict_schema_raises_g2000_when_combo_unknown() {
    // The schema has Account, Account+VIP, Person, Dummy+Person, and
    // Account-Transfer-Account. Adding `:Robot` to an Account produces
    // a label combination that does not subtype any node type.
    let store = fraud_store("set_label_schema_g2000.gdb");
    let schema = account_or_vip_schema();
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a:Robot");
    let err = run_dm(&store, &dm, Some(&schema)).unwrap_err();
    assert!(err.contains("G2000"), "expected G2000, got: {err}");
    // Atomicity: the SET must not leave a stray Robot label behind.
    let robots = store.nodes_with_label("Robot");
    assert!(
        robots.as_ref().map(|v| v.is_empty()).unwrap_or(true),
        "Robot label index must be empty after rollback: {robots:?}"
    );
}

#[test]
fn save_persists_label_mods_through_reopen() {
    // SET label on a base node, .save, reopen, and confirm the new
    // label survives. Validates that materialize_to_graph picks up the
    // overlay's mod_node_labels rather than just `mod_node_props`.
    let path = temp_db("set_label_persist.gdb");
    {
        let json_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
        let g = MemoryGraphStore::from_file(&json_path).unwrap();
        g.save(&path).unwrap();
        let store = LazyGraphStore::open(&path).unwrap();
        let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) SET a:VIP");
        run_dm(&store, &dm, None).unwrap();
        store.save(&path).unwrap();
    }
    // Fresh open: the persisted file must surface the VIP label via the
    // base label-index path (no overlay involved this time).
    let reopened = LazyGraphStore::open(&path).unwrap();
    let vips = reopened.nodes_with_label("VIP").unwrap();
    assert_eq!(vips.len(), 1);
    let labels = label_strings_for(&reopened, vips[0]);
    assert!(labels.contains(&"Account".to_string()));
    assert!(labels.contains(&"VIP".to_string()));
}
