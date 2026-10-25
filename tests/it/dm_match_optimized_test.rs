//! A data-modifying statement's MATCH chain must reach the index.
//!
//! `run_dm` used to elaborate the chain and stop. Elaboration lowers
//! `(x:L {k: 'v'})` into a `WHERE x.k = 'v'` conjunct; it is the
//! **optimizer's** value-predicate pushdown that puts it back on the
//! descriptor as a `value_pred`, and `Runtime::indexed_candidates` reads
//! exactly that field to reach the secondary index. So a DM's MATCH could
//! not use an index at all — it scanned every node carrying the label and
//! decoded its properties, once per pattern variable, per statement.
//!
//! Measured on 1 200 nodes with a hash index on `(ACCOUNT, account_number)`,
//! 2 000 statements of the shape
//!
//! ```text
//! MATCH (x:ACCOUNT {account_number: 'A…'}), (y:ACCOUNT {account_number: 'A…'})
//! INSERT (x)-[:PAYS]->(y)
//! ```
//!
//! took **5.6 s**, against **0.04 s** for the same 2 000 `MATCH`es run as
//! plain queries. That gap — not the secondary-index guard, which a pure
//! edge insert never even trips — is the larger half of why loading a graph
//! by `INSERT` looked quadratic.
//!
//! The assertion here is structural rather than timed: a wall clock in the
//! test suite is a flake, and what actually has to hold is that the pattern
//! `run_dm` executes carries the pushed-down predicate.

use frogql::model::graph::MemoryGraphStore;
use frogql::model::graph_access::GraphAccess;
use frogql::parser::parse_statement;
use frogql::runtime::dm::{prepare_match_pattern, run_dm};
use frogql::syntax::descriptor::Descriptor;
use frogql::syntax::path_pattern::PathPattern;
use frogql::syntax::query::Query;
use frogql::syntax::statement::Statement;

fn dm_of(src: &str) -> frogql::syntax::dm::DmStatement {
    match parse_statement(src).expect("parses") {
        Statement::DataModification(dm) => dm,
        other => panic!("expected a DM statement, got {other:?}"),
    }
}

/// The pattern `run_dm` would execute for this statement's MATCH chain.
fn executed_pattern(src: &str) -> PathPattern {
    let dm = dm_of(src);
    let q = Query {
        matches: dm.matches.clone(),
        ..Query::empty()
    };
    prepare_match_pattern(&frogql::elaborate::elaborate_query(q))
}

/// Every node descriptor in the pattern, however nested.
fn node_descriptors(p: &PathPattern, out: &mut Vec<Descriptor>) {
    match p {
        PathPattern::Node(Some(d)) => out.push(d.clone()),
        PathPattern::Node(None) => {}
        PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
            node_descriptors(a, out);
            node_descriptors(b, out);
        }
        PathPattern::Filter(inner, _)
        | PathPattern::Repeat { pattern: inner, .. }
        | PathPattern::Questioned(inner)
        | PathPattern::Selected { pattern: inner, .. }
        | PathPattern::Named { pattern: inner, .. } => node_descriptors(inner, out),
        _ => {}
    }
}

/// The fix, stated directly: the descriptor carries the predicate the
/// index is looked up by.
#[test]
fn dm_match_chain_carries_pushed_down_value_predicates() {
    let p = executed_pattern("MATCH (x:ACCOUNT {account_number: 'A000001'}) INSERT (:Tmp {n: 1})");
    let mut descs = Vec::new();
    node_descriptors(&p, &mut descs);

    let x = descs
        .iter()
        .find(|d| d.var.as_deref() == Some("x"))
        .expect("x is in the pattern");
    assert!(
        x.value_preds
            .iter()
            .any(|(attr, _, _)| attr == "account_number"),
        "the DM's MATCH must reach the index; value_preds was {:?}",
        x.value_preds
    );
}

/// Both endpoints of a comma-join, since that is the shape a bulk edge
/// load actually writes and the one that was paying the scan twice.
#[test]
fn both_endpoints_of_a_comma_join_are_pushed_down() {
    let p = executed_pattern(
        "MATCH (x:ACCOUNT {account_number: 'A1'}), (y:ACCOUNT {account_number: 'A2'}) \
         INSERT (x)-[:PAYS]->(y)",
    );
    let mut descs = Vec::new();
    node_descriptors(&p, &mut descs);

    for var in ["x", "y"] {
        let d = descs
            .iter()
            .find(|d| d.var.as_deref() == Some(var))
            .unwrap_or_else(|| panic!("{var} is in the pattern"));
        assert!(
            !d.value_preds.is_empty(),
            "{var} lost its pushed-down predicate: {:?}",
            d.value_preds
        );
    }
}

/// Optimizing must not change which rows the DM applies to. The whole
/// point of the pass is that it is performance-preserving.
#[test]
fn optimizing_the_match_chain_does_not_change_the_rows() {
    let json = r#"{"nodes":[
        {"id":"a","labels":["P"],"props":{"k":"one"}},
        {"id":"b","labels":["P"],"props":{"k":"two"}},
        {"id":"c","labels":["P"],"props":{"k":"one"}}
    ],"edges":[]}"#;
    let store = MemoryGraphStore::from_json_str(json).unwrap();
    let before = store.nodes().len();

    let dm = dm_of("MATCH (x:P {k: 'one'}) INSERT (:Marked {via: 1})");
    let out = run_dm(&store, &dm, None).expect("runs");

    // Two nodes carry k = 'one', so the INSERT fires twice — not three
    // times (which is what dropping the filter would do) and not once.
    assert_eq!(out.nodes_inserted, 2, "one INSERT per matching row");
    assert_eq!(store.nodes().len(), before + 2);
}
