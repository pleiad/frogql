//! End-to-end tests: parse GQL strings → run against graph → check result counts.
//! These mirror the Python runtime_test.py tests but use string parsing.

use std::path::Path;

use gqlrust::compile;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::runtime::engine::Runtime;

fn fraud_run(query: &str) -> usize {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    let g = MemoryGraphStore::from_file(&p).unwrap();
    let r = Runtime::new(&g);
    let pattern = compile(query).unwrap();
    r.run(&pattern).rows.len()
}

fn social_run(query: &str) -> usize {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/social-network.json");
    let g = MemoryGraphStore::from_file(&p).unwrap();
    let r = Runtime::new(&g);
    let pattern = compile(query).unwrap();
    r.run(&pattern).rows.len()
}

#[test]
fn test_node_empty() {
    assert_eq!(fraud_run("()"), 5);
}

#[test]
fn test_node_capturing() {
    assert_eq!(fraud_run("(x)"), 5);
}

#[test]
fn test_node_filter_by_label() {
    assert_eq!(fraud_run("(x: Account)"), 4);
}

#[test]
fn test_edge_empty() {
    assert_eq!(fraud_run("-[]->"), 5);
}

#[test]
fn test_edge_nondirectional() {
    assert_eq!(fraud_run("~~"), 0);
}

#[test]
fn test_edge_filter_by_label() {
    assert_eq!(fraud_run("-[x: Transfer]->"), 4);
}

#[test]
fn test_concat() {
    assert_eq!(fraud_run("()-[]->"), 5);
}

#[test]
fn test_concat_label() {
    assert_eq!(fraud_run("(x)-[:Foo]->"), 1);
}

#[test]
fn test_size_2() {
    assert_eq!(fraud_run("()-[]->()-[]->()"), 5);
}

#[test]
fn test_selector() {
    assert_eq!(fraud_run("(x WHERE x.isDummy bool)"), 1);
}

#[test]
fn test_concat_selector() {
    assert_eq!(fraud_run("()(x:{isDummy bool})"), 1);
}

// ISO §16 elementPropertySpecification without isLabelExpression.
// Same value-filter semantics as the colonised forms.

#[test]
fn test_node_bare_prop_spec_anonymous_runtime() {
    // 1 node has isBlocked=true (p2 Mike).
    assert_eq!(fraud_run("({isBlocked: true})"), 1);
    // 4 nodes have isBlocked=false (a1, a2, p1, d1).
    assert_eq!(fraud_run("({isBlocked: false})"), 4);
}

#[test]
fn test_node_bare_prop_spec_with_var_runtime() {
    assert_eq!(fraud_run("(x {owner: 'Aretha'})"), 1);
}

#[test]
fn test_edge_bare_prop_spec_anonymous_runtime() {
    // 2 transfers/edges with amount=2000000 (t4 Transfer, t5 Foo).
    assert_eq!(fraud_run("()-[{amount: 2000000}]->()"), 2);
}

#[test]
fn test_edge_bare_prop_spec_with_var_runtime() {
    assert_eq!(fraud_run("()-[e {amount: 2500000}]->()"), 1);
}

#[test]
fn test_union() {
    assert_eq!(fraud_run("(x: Dummy) | (y: Account)"), 5);
}

#[test]
fn test_filter_blocked_true() {
    assert_eq!(fraud_run("(y WHERE y.isBlocked=true)"), 1);
}

#[test]
fn test_filter_blocked_false() {
    assert_eq!(fraud_run("(y WHERE y.isBlocked=false)"), 4);
}

#[test]
fn test_filter_blocked_int() {
    assert_eq!(fraud_run("(y WHERE y.isBlocked=1)"), 0);
}

#[test]
fn test_filter_2() {
    assert_eq!(
        fraud_run("-[y WHERE y.amount>=3500000 and y.amount>1]->"),
        1
    );
}

#[test]
fn test_filter_4() {
    assert_eq!(fraud_run("-[y WHERE y.bambino > 0]->"), 0);
}

#[test]
fn test_union_fail() {
    assert_eq!(fraud_run("(x: NoExists) | (x: NoExists)"), 0);
}

#[test]
fn test_concat_any_right() {
    assert_eq!(fraud_run("-"), 10);
}

#[test]
fn test_repetition() {
    assert_eq!(fraud_run("-->{1,2}"), 23);
}

#[test]
fn test_repetition_descriptor() {
    assert_eq!(fraud_run("-[x]->{2,3}"), 10);
}

#[test]
fn test_repetition_repetition() {
    assert_eq!(fraud_run("(-[x]->{1,2}){2,3}"), 60);
}

#[test]
fn test_digest_p4() {
    assert_eq!(
        fraud_run("(x) -[z:Transfer WHERE z.amount>1000000]-> (y WHERE y.isBlocked=true)"),
        1
    );
}

#[test]
fn test_is_bool() {
    assert_eq!(fraud_run("(x WHERE x.isBlocked bool)"), 5);
}

#[test]
fn test_is_str() {
    assert_eq!(fraud_run("(x WHERE x.isBlocked str)"), 0);
}

#[test]
fn test_as_bool() {
    assert_eq!(fraud_run("(x WHERE x.isBlocked as bool)"), 1);
}

#[test]
fn test_as_int_gt() {
    assert_eq!(fraud_run("(x WHERE x.isBlocked as int > 0)"), 0);
}

#[test]
fn test_where_social() {
    assert_eq!(social_run("(x: {status bool})"), 1);
}

#[test]
fn test_unop_not() {
    assert_eq!(fraud_run("(x WHERE not x.isBlocked)"), 4);
}

#[test]
fn test_unop_neg() {
    assert_eq!(fraud_run("-[x WHERE -x.amount < 0]->"), 5);
}

// --- Multi-label tests ---

#[test]
fn test_multi_label_and() {
    // d1 has labels Dummy & Person — (:Dummy & Person) should match only d1
    assert_eq!(fraud_run("(x: Dummy & Person)"), 1);
}

#[test]
fn test_multi_label_single() {
    // (:Dummy) should match d1 (the only node with Dummy label)
    assert_eq!(fraud_run("(x: Dummy)"), 1);
}

#[test]
fn test_multi_label_person() {
    // (:Person) should match d1 (it has Person label even though it also has Dummy)
    assert_eq!(fraud_run("(x: Person)"), 1);
}

#[test]
fn test_edge_label_colon_syntax() {
    // Correct syntax: -[:Transfer]-> should filter by label
    assert_eq!(fraud_run("-[:Transfer]->"), 4);
}

#[test]
fn test_multi_hop_with_label() {
    // (x: Account)-[:Transfer]->(y: Account) — transfers between accounts
    assert_eq!(fraud_run("(x: Account)-[:Transfer]->(y: Account)"), 4);
}

#[test]
fn test_social_multi_label_node() {
    // n1 has Person & Teacher, n2 has Person & Student
    // (:Person) should match both n1 and n2
    assert_eq!(social_run("(x: Person)"), 2);
}

#[test]
fn test_social_edge_label() {
    // e1 is undirected Knows, e2 is directed Likes, e3 is directed Author
    assert_eq!(social_run("~[:Knows]~"), 2); // 2 orientations for 1 undirected edge
    assert_eq!(social_run("-[:Likes]->"), 1);
    assert_eq!(social_run("-[:Author]->"), 1);
}

// --- Property descriptor tests ---

#[test]
fn test_node_with_property_desc() {
    // (x:{owner str}) — open prop type, matches all nodes that have owner as string
    // fraud: a1, a2, p1, p2, d1 all have owner:str → 5
    assert_eq!(fraud_run("(x:{owner str})"), 5);
}

#[test]
fn test_node_label_and_property() {
    // (x: Account {owner str}) — Account nodes with owner:str
    assert_eq!(fraud_run("(x: Account {owner str})"), 4);
}

#[test]
fn test_edge_label_and_property() {
    // -[y: Transfer {amount int}]-> — Transfer edges with amount:int
    assert_eq!(fraud_run("-[y: Transfer {amount int}]->"), 4);
}

#[test]
fn test_full_descriptor_chain() {
    // (x: Account {owner str})-[y: Transfer {amount int}]->(z)
    assert_eq!(
        fraud_run("(x: Account {owner str})-[y: Transfer {amount int}]->(z)"),
        4
    );
}
