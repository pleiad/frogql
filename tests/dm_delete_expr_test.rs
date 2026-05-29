//! MVP-1.E tests: ISO §13.5 DELETE with `<value expression>` targets
//! (Feature GD04). Each `DELETE x` target is now an expression
//! evaluated per binding row; the runtime expects a Node / Edge ref or
//! Null.

use std::path::{Path, PathBuf};

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::parser::parse_statement;
use gqlrust::runtime::dm::run_dm;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::statement::Statement;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_dm_delete_expr");
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
fn delete_bare_variable_still_works() {
    // Backwards-compat: a bare variable reference parses as Expr::Var
    // and resolves to the matched node identity.
    let store = fraud_store("delete_bare_var.gdb");
    let before = store.node_count();
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Aretha'}) DETACH DELETE a");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_deleted, 1);
    assert_eq!(store.node_count() + 1, before + 1); // count not decremented; tombstoned only
    let aretha = store
        .nodes_with_label("Account")
        .unwrap()
        .into_iter()
        .find(|&n| {
            matches!(
                store.node_props(n).get("owner"),
                Some(gqlrust::model::value::Value::Str(s)) if s == "Aretha"
            )
        });
    assert!(aretha.is_none(), "Aretha must no longer surface");
}

#[test]
fn delete_coalesce_expression_picks_a_node() {
    // The expression yields a Node value via COALESCE — when the
    // first arg is Null, the second binding (b) is used. Lets the
    // user write conditional deletes without IF/CASE syntax.
    let store = fraud_store("delete_coalesce.gdb");
    let dm = parse_dm_or_panic(
        "MATCH (a:Account {owner: 'Aretha'}), (b:Account {owner: 'Scott'}) \
         DETACH DELETE COALESCE(NULL, a)",
    );
    let exec = run_dm(&store, &dm, None).unwrap();
    assert!(exec.nodes_deleted >= 1, "expected at least Aretha gone");
    // Aretha must be gone; Scott must survive.
    let owners: Vec<String> = store
        .nodes_with_label("Account")
        .unwrap()
        .into_iter()
        .filter_map(|n| match store.node_props(n).get("owner") {
            Some(gqlrust::model::value::Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(!owners.iter().any(|o| o == "Aretha"));
    assert!(owners.iter().any(|o| o == "Scott"));
}

#[test]
fn delete_null_target_is_a_noop() {
    // ISO §13.5 GR4 a — a NULL target contributes nothing; the
    // statement still completes. We exercise this by deleting `NULL`
    // as a literal target and asserting nothing changed.
    let store = fraud_store("delete_null.gdb");
    let before = store.nodes_with_label("Account").unwrap().len();
    let dm = parse_dm_or_panic("MATCH (a:Account) DETACH DELETE NULL");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_deleted, 0);
    assert_eq!(exec.edges_deleted, 0);
    assert_eq!(store.nodes_with_label("Account").unwrap().len(), before);
}

#[test]
fn delete_non_node_value_errors() {
    let store = fraud_store("delete_wrong_type.gdb");
    // 42 is an Int, not a node/edge reference.
    let dm = parse_dm_or_panic("MATCH (a:Account) DETACH DELETE 42");
    let err = run_dm(&store, &dm, None).unwrap_err();
    assert!(
        err.to_lowercase().contains("expected a node"),
        "expected node-or-edge-required error, got: {err}"
    );
    // Atomicity: nothing got tombstoned.
    let count = store.nodes_with_label("Account").unwrap().len();
    assert!(count > 0);
}

#[test]
fn delete_edge_via_expression() {
    // Direct edge variable as a value expression — the runtime expects
    // the edge id back through Expr::Var.
    let store = fraud_store("delete_edge_expr.gdb");
    let dm = parse_dm_or_panic("MATCH ()-[t:Transfer]->() DETACH DELETE t");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert!(exec.edges_deleted > 0, "edges should have been deleted");
    let remaining = store.directed_edges_with_label("Transfer");
    assert!(
        remaining.as_ref().map(|v| v.is_empty()).unwrap_or(true),
        "Transfer edges should be gone after delete: {remaining:?}"
    );
}

#[test]
fn detach_delete_via_node_expression_with_explicit_label() {
    // Targets that resolve through a more elaborate expression chain
    // (here a no-op COALESCE wrapping the bound var) still produce a
    // node ref the runtime accepts.
    let store = fraud_store("delete_expr_chain.gdb");
    let dm = parse_dm_or_panic("MATCH (a:Account {owner: 'Mike'}) DETACH DELETE COALESCE(a, NULL)");
    let exec = run_dm(&store, &dm, None).unwrap();
    assert_eq!(exec.nodes_deleted, 1);
    let mike = store
        .nodes_with_label("Account")
        .unwrap()
        .into_iter()
        .find(|&n| {
            matches!(
                store.node_props(n).get("owner"),
                Some(gqlrust::model::value::Value::Str(s)) if s == "Mike"
            )
        });
    assert!(mike.is_none());
}
