//! Parser tests for ISO/IEC 39075:2024 §13 data-modification statements.
//!
//! Covers MVP-0 surface: standalone INSERT, MATCH+(DETACH|NODETACH)? DELETE,
//! optional RETURN, plus negative cases for the constructs that ISO §16.5
//! explicitly forbids inside an `<insert path pattern>`.

use gqlrust::parser::parse_statement;
use gqlrust::syntax::dm::{DmOp, DmStatement};
use gqlrust::syntax::path_pattern::PathPattern;
use gqlrust::syntax::statement::Statement;

fn parse_dm(input: &str) -> DmStatement {
    match parse_statement(input).unwrap_or_else(|e| panic!("parse failed for {input:?}: {e}")) {
        Statement::DataModification(dm) => dm,
        other => panic!("expected DM, got {other:?}"),
    }
}

#[test]
fn insert_standalone_single_node() {
    let dm = parse_dm("INSERT (a:Person {name: 'Alice'})");
    assert!(dm.matches.is_empty());
    assert!(dm.returns.is_none());
    let DmOp::Insert(patterns) = &dm.op else {
        panic!("expected Insert");
    };
    assert_eq!(patterns.len(), 1);
    assert!(matches!(patterns[0], PathPattern::Node(Some(_))));
}

#[test]
fn insert_standalone_multiple_paths() {
    let dm = parse_dm("INSERT (a:Person), (b:Person), (a)-[:KNOWS]->(b)");
    let DmOp::Insert(patterns) = &dm.op else {
        panic!("expected Insert");
    };
    assert_eq!(patterns.len(), 3);
}

#[test]
fn insert_directed_edge() {
    let dm = parse_dm("INSERT (a:Person)-[:KNOWS]->(b:Person)");
    let DmOp::Insert(patterns) = &dm.op else {
        panic!();
    };
    assert!(matches!(&patterns[0], PathPattern::Concat(_, _)));
}

#[test]
fn insert_with_return() {
    let dm = parse_dm("INSERT (a:Tag {name: 'x'}) RETURN a");
    assert!(dm.returns.is_some());
    assert_eq!(dm.returns.as_ref().unwrap().len(), 1);
}

#[test]
fn insert_rejects_where() {
    // ISO §16.5 keeps INSERT path patterns free of WHERE filters. We don't
    // care which layer surfaces the error (validator vs. unexpected-token
    // upstream); the contract is that the statement fails to parse.
    let err = parse_statement("INSERT (a:Person) WHERE a.age > 18").unwrap_err();
    assert!(!err.is_empty(), "expected parse error, got: {err}");
}

#[test]
fn insert_rejects_repetition() {
    let err = parse_statement("INSERT (a)-[:K]->(b){1,3}").unwrap_err();
    assert!(
        err.to_lowercase().contains("repetition") || err.contains("§16.5"),
        "expected repetition reject, got: {err}"
    );
}

#[test]
fn insert_rejects_union() {
    // Same contract as `insert_rejects_where`: a `|` between path terms
    // is invalid in INSERT regardless of which layer surfaces the error.
    let err = parse_statement("INSERT (a:Person)|(b:Tag)").unwrap_err();
    assert!(!err.is_empty(), "expected parse error, got: {err}");
}

#[test]
fn insert_rejects_any_direction_edge() {
    let err = parse_statement("INSERT (a:Person)-[:K]-(b:Person)").unwrap_err();
    assert!(
        err.to_lowercase().contains("any-direction") || err.contains("§16.5"),
        "expected any-direction reject, got: {err}"
    );
}

#[test]
fn delete_simple() {
    use gqlrust::syntax::expr::Expr;
    let dm = parse_dm("MATCH (a:Person) DELETE a");
    assert_eq!(dm.matches.len(), 1);
    let DmOp::Delete { detach, targets } = &dm.op else {
        panic!("expected Delete");
    };
    assert!(!(*detach), "DELETE without prefix → NODETACH (§13.5 SR6)");
    assert_eq!(targets.len(), 1);
    // Bare variable reference parses as `Expr::Var` after MVP-1.E.
    assert!(matches!(&targets[0], Expr::Var(name) if name == "a"));
}

#[test]
fn detach_delete_simple() {
    let dm = parse_dm("MATCH (a:Person) DETACH DELETE a");
    let DmOp::Delete { detach, targets } = &dm.op else {
        panic!();
    };
    assert!(*detach);
    assert_eq!(targets.len(), 1);
}

#[test]
fn nodetach_delete_explicit() {
    let dm = parse_dm("MATCH (a:Person) NODETACH DELETE a");
    let DmOp::Delete { detach, .. } = &dm.op else {
        panic!();
    };
    assert!(!*detach);
}

#[test]
fn delete_multiple_targets() {
    use gqlrust::syntax::expr::Expr;
    let dm = parse_dm("MATCH (a:Person), (b:Person) DETACH DELETE a, b");
    let DmOp::Delete { targets, .. } = &dm.op else {
        panic!();
    };
    let names: Vec<&str> = targets
        .iter()
        .map(|e| match e {
            Expr::Var(n) => n.as_str(),
            other => panic!("expected Var, got {other:?}"),
        })
        .collect();
    assert_eq!(names, vec!["a", "b"]);
}

#[test]
fn delete_requires_match() {
    let err = parse_statement("DELETE x").unwrap_err();
    assert!(err.to_lowercase().contains("match"), "got: {err}");
}

#[test]
fn detach_alone_is_an_error() {
    // DETACH without DELETE.
    let err = parse_statement("MATCH (a) DETACH").unwrap_err();
    assert!(!err.is_empty());
}

#[test]
fn match_then_insert() {
    let dm = parse_dm("MATCH (a:Person) INSERT (a)-[:KNOWS]->(b:Tag {name: 'x'})");
    assert_eq!(dm.matches.len(), 1);
    let DmOp::Insert(patterns) = &dm.op else {
        panic!();
    };
    assert_eq!(patterns.len(), 1);
}

#[test]
fn match_then_insert_with_return() {
    let dm = parse_dm("MATCH (a:Person) INSERT (a)-[:K]->(b:Tag) RETURN a, b");
    assert!(dm.returns.is_some());
    assert_eq!(dm.returns.as_ref().unwrap().len(), 2);
}

#[test]
fn set_property_parses_in_mvp1() {
    // MVP-1.B: `SET x.prop = expr` parses cleanly; the runtime side is
    // covered by `dm_set_test.rs`.
    let dm = parse_dm("MATCH (a:Person) SET a.name = 'B'");
    assert_eq!(dm.matches.len(), 1);
    assert!(matches!(dm.op, DmOp::Set(_)));
}

#[test]
fn set_all_properties_parses_in_mvp1() {
    let dm = parse_dm("MATCH (a:Person) SET a = { name: 'X', age: 7 }");
    assert!(matches!(dm.op, DmOp::Set(_)));
}

#[test]
fn remove_property_parses_in_mvp1() {
    // MVP-1.C: REMOVE x.prop parses cleanly; runtime side covered by
    // dm_remove_test.rs.
    let dm = parse_dm("MATCH (a:Person) REMOVE a.name");
    assert!(matches!(dm.op, DmOp::Remove(_)));
}

#[test]
fn plain_query_still_parses() {
    // Make sure the dispatcher refactor didn't break Query parsing.
    match parse_statement("MATCH (a:Person) RETURN a").unwrap() {
        Statement::Query(_) => {}
        other => panic!("expected Query, got {other:?}"),
    }
}

#[test]
fn ddl_still_parses() {
    match parse_statement("SHOW GRAPH TYPES").unwrap() {
        Statement::ShowGraphTypes => {}
        other => panic!("expected ShowGraphTypes, got {other:?}"),
    }
}

#[test]
fn insert_with_undirected_edge() {
    let dm = parse_dm("INSERT (a:Person)~[:FRIENDS]~(b:Person)");
    let DmOp::Insert(patterns) = &dm.op else {
        panic!();
    };
    assert!(matches!(&patterns[0], PathPattern::Concat(_, _)));
}

#[test]
fn insert_with_left_edge() {
    let dm = parse_dm("INSERT (a:Person)<-[:KNOWS]-(b:Person)");
    let DmOp::Insert(patterns) = &dm.op else {
        panic!();
    };
    assert!(matches!(&patterns[0], PathPattern::Concat(_, _)));
}
