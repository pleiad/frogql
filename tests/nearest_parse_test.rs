//! `NEAREST` surface syntax: parsing and typechecking.
//!
//! Nothing here runs a query — the evaluation strategies are tested
//! separately. What matters here is that the clause parses into the
//! shape the strategies expect, that its soft keywords do not steal
//! names from ordinary queries, and that the typechecker rejects the
//! forms no strategy could sensibly evaluate.

use frogql::parser::parse_query;
use frogql::syntax::expr::Expr;
use frogql::syntax::query::KMode;
use frogql::typing::checker::Typechecker;

fn parse(q: &str) -> frogql::syntax::query::Query {
    parse_query(q).unwrap_or_else(|e| panic!("parse failed for `{q}`: {e}"))
}

/// Typecheck against the permissive schema and return the diagnostics.
fn check(q: &str) -> (bool, Vec<String>, Vec<String>) {
    let query = frogql::elaborate::elaborate_query(parse(q));
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&query);
    (r.ok, tc.errors.clone(), tc.warnings.clone())
}

// -- parsing ---------------------------------------------------------

#[test]
fn parses_the_minimal_form() {
    let q = parse("MATCH (a)-[:p]->(b) NEAREST 10 b.emb TO [1.0, 2.0]");
    let n = q.nearest.expect("clause present");
    assert_eq!(n.k, 10);
    assert_eq!(n.mode, KMode::DistinctVar);
    assert_eq!(n.var, "b");
    assert_eq!(n.attr, "emb");
    assert_eq!(n.dist_var, None);
    assert!(matches!(n.query, Expr::Const(_)));
}

#[test]
fn parses_the_rows_mode() {
    let q = parse("MATCH (a)-[:p]->(b) NEAREST 5 ROWS b.emb TO [1.0]");
    assert_eq!(q.nearest.unwrap().mode, KMode::Rows);
}

#[test]
fn parses_the_distance_binding() {
    let q = parse("MATCH (a)-[:p]->(b) NEAREST 5 b.emb TO [1.0] AS d");
    assert_eq!(q.nearest.unwrap().dist_var.as_deref(), Some("d"));
}

#[test]
fn parses_the_vector_builtin_as_the_query() {
    let q = parse("MATCH (a)-[:p]->(b) NEAREST 5 b.emb TO VECTOR(42, 'emb') AS d");
    let n = q.nearest.unwrap();
    match n.query {
        Expr::Call { name, args } => {
            assert_eq!(name, "VECTOR");
            assert_eq!(args.len(), 2);
        }
        other => panic!("expected a VECTOR call, got {other:?}"),
    }
}

#[test]
fn coexists_with_return_order_by_and_limit() {
    let q = parse(
        "MATCH (tower)-[:img]->(img) \
         NEAREST 10 img.emb TO VECTOR(7, 'emb') AS dist \
         RETURN tower, img, dist ORDER BY dist LIMIT 3",
    );
    assert!(q.nearest.is_some());
    assert_eq!(q.returns.unwrap().len(), 3);
    assert!(q.order_by.is_some());
    assert_eq!(q.limit, Some(3));
}

#[test]
fn renders_in_display() {
    // Rendering only, not a round trip. `Query`'s Display does not parse
    // back today for reasons that predate this clause and are orthogonal
    // to it: an edge renders as `-[p {*}]->` (no `:` before the label,
    // an unparseable `{*}` property type) and a string literal renders
    // with double quotes while the lexer accepts only single ones.
    let shown = parse("MATCH (a)-[:p]->(b) NEAREST 5 ROWS b.emb TO [1.0, 2.0] AS d").to_string();
    assert!(shown.contains("NEAREST 5 ROWS b.emb TO"), "{shown}");
    assert!(shown.contains("AS d"), "{shown}");

    let shown = parse("MATCH (a)-[:p]->(b) NEAREST 5 b.emb TO VECTOR(1, 'emb')").to_string();
    assert!(shown.contains("NEAREST 5 b.emb TO VECTOR("), "{shown}");
}

#[test]
fn a_query_without_the_clause_has_none() {
    assert!(parse("MATCH (a)-[:p]->(b) RETURN a").nearest.is_none());
}

// -- soft keywords ---------------------------------------------------

#[test]
fn nearest_rows_and_to_stay_usable_as_names() {
    // The three words are matched only in clause position. If any of them
    // were lexed as a hard keyword, these would fail to parse.
    for q in [
        "MATCH (nearest)-[:p]->(rows) RETURN nearest, rows",
        "MATCH (x)-[:p]->(y) WHERE x.to = 3 RETURN x",
        "MATCH (x:nearest)-[:to]->(y:rows) RETURN x",
        "MATCH (x)-[:p]->(y) RETURN x.nearest AS to",
    ] {
        parse(q);
    }
}

#[test]
fn vector_stays_usable_as_a_name_outside_a_call() {
    for q in [
        "MATCH (vector)-[:p]->(y) RETURN vector",
        "MATCH (x)-[:p]->(y) WHERE x.vector = 1 RETURN x",
        "MATCH (x:vector)-[:p]->(y) RETURN x",
    ] {
        parse(q);
    }
}

// -- parse errors ----------------------------------------------------

#[test]
fn rejects_a_missing_to() {
    let e = parse_query("MATCH (a)-[:p]->(b) NEAREST 5 b.emb [1.0]").unwrap_err();
    assert!(e.contains("TO"), "{e}");
}

#[test]
fn rejects_a_missing_attribute() {
    assert!(parse_query("MATCH (a)-[:p]->(b) NEAREST 5 b TO [1.0]").is_err());
}

#[test]
fn rejects_a_negative_count() {
    let e = parse_query("MATCH (a)-[:p]->(b) NEAREST -1 b.emb TO [1.0]").unwrap_err();
    assert!(e.contains("NEAREST"), "{e}");
}

#[test]
fn rejects_a_one_argument_vector_call() {
    let e = parse_query("MATCH (a)-[:p]->(b) NEAREST 5 b.emb TO VECTOR(1)").unwrap_err();
    assert!(e.contains("VECTOR"), "{e}");
}

// -- typechecking ----------------------------------------------------

#[test]
fn accepts_a_well_formed_clause() {
    let (ok, errors, _) = check("MATCH (a)-[:p]->(b) NEAREST 5 b.emb TO [1.0, 2.0] AS d");
    assert!(ok, "{errors:?}");
}

#[test]
fn the_distance_variable_is_projectable_and_orderable() {
    let (ok, errors, _) =
        check("MATCH (a)-[:p]->(b) NEAREST 5 b.emb TO [1.0] AS d RETURN b, d ORDER BY d LIMIT 2");
    assert!(ok, "{errors:?}");
}

#[test]
fn rejects_a_search_variable_the_pattern_does_not_bind() {
    let (ok, errors, _) = check("MATCH (a)-[:p]->(b) NEAREST 5 zz.emb TO [1.0]");
    assert!(!ok);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("not bound by the pattern")),
        "{errors:?}"
    );
}

#[test]
fn rejects_an_edge_as_the_search_variable() {
    let (ok, errors, _) = check("MATCH (a)-[e:p]->(b) NEAREST 5 e.emb TO [1.0]");
    assert!(!ok);
    assert!(
        errors.iter().any(|e| e.contains("node variable")),
        "{errors:?}"
    );
}

#[test]
fn rejects_a_non_vector_query_expression() {
    let (ok, errors, _) = check("MATCH (a)-[:p]->(b) NEAREST 5 b.emb TO 3");
    assert!(!ok);
    assert!(
        errors.iter().any(|e| e.contains("list of numbers")),
        "{errors:?}"
    );
}

#[test]
fn rejects_a_distance_variable_that_shadows_the_pattern() {
    let (ok, errors, _) = check("MATCH (a)-[:p]->(b) NEAREST 5 b.emb TO [1.0] AS a");
    assert!(!ok);
    assert!(errors.iter().any(|e| e.contains("shadow")), "{errors:?}");
}

#[test]
fn warns_on_a_zero_count() {
    let (ok, _, warnings) = check("MATCH (a)-[:p]->(b) NEAREST 0 b.emb TO [1.0]");
    assert!(ok, "zero is legal, just pointless");
    assert!(
        warnings.iter().any(|w| w.contains("empty result")),
        "{warnings:?}"
    );
}

#[test]
fn rejects_a_vector_call_with_a_non_string_attribute() {
    let (ok, errors, _) = check("MATCH (a)-[:p]->(b) NEAREST 5 b.emb TO VECTOR(1, 2)");
    assert!(!ok);
    assert!(
        errors.iter().any(|e| e.contains("string attribute name")),
        "{errors:?}"
    );
}
