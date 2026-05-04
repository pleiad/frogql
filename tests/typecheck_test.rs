//! Typechecker tests.
//!
//! Originally translated line-for-line from `fppc/src/typechecker/checker.rs`'s
//! inline `#[cfg(test)] mod tests` (the typechecker reference test suite).
//! Surface-syntax differences are adapted: gqlite uses `{a int}` for
//! property type ascription where fppc uses `{a int}`; gqlite's parser
//! requires node anchors (`()-[]->()` not bare `->`); fppc's
//! `test_incompatible_records` uses double-brace nested record syntax that
//! gqlite's parser doesn't accept and is omitted (the underlying lattice
//! case is covered by the closed-schema tests below).
//!
//! These assert the same `ok` / `empty` / warning predicates the fppc
//! suite asserts. The file name reflects gqlite-local convention
//! (`*_test.rs`), not fppc dependency.

use std::collections::BTreeMap;

use gqlrust::elaborate;
use gqlrust::parser;
use gqlrust::syntax::query::Query;
use gqlrust::typing::checker::{TypecheckResult, Typechecker};
use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::simple_type::SimpleType;
use gqlrust::typing::variable_type::{Schema, VariableType};

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

/// Parse, elaborate, and typecheck a bare path-pattern query under the
/// given schema. Returns the typecheck result and the checker's
/// errors/warnings vectors.
fn check_with(schema: Schema, query: &str) -> (TypecheckResult, Vec<String>, Vec<String>) {
    let ast = parser::parse(query).expect("parse failed");
    let q = Query::pattern_only(ast);
    let q = elaborate::elaborate_query(q);
    let mut tc = Typechecker::new(schema);
    let r = tc.check_query(&q);
    (r, tc.errors.clone(), tc.warnings.clone())
}

/// Untyped (Schema::star) check.
fn check(query: &str) -> (TypecheckResult, Vec<String>, Vec<String>) {
    check_with(Schema::star(), query)
}

fn label(s: &str) -> LabelType {
    LabelType::Label(s.into())
}

fn label_and(a: LabelType, b: LabelType) -> LabelType {
    LabelType::And(Box::new(a), Box::new(b))
}

fn closed(props: &[(&str, SimpleType)]) -> PropertyType {
    let m: BTreeMap<String, SimpleType> = props
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    PropertyType::Closed(m)
}

fn node_dt(label: LabelType, props: PropertyType) -> VariableType {
    VariableType::Node(DescriptorType::new(label, props))
}

fn directed_edge(
    desc_label: LabelType,
    desc_props: PropertyType,
    left: VariableType,
    right: VariableType,
) -> VariableType {
    VariableType::EdgeDirectional {
        desc: DescriptorType::new(desc_label, desc_props),
        left: Box::new(left),
        right: Box::new(right),
    }
}

// -----------------------------------------------------------------------
// Schemas (mirrored from fppc/src/typechecker/checker.rs::tests)
// -----------------------------------------------------------------------

/// Fraud detection schema, mirroring fppc::tests::fraud_schema:
///   Account {owner str, isBlocked bool}
///   Dummy & Person {owner str, isBlocked bool, isDummy bool}
///   Transfer {amount int}: Account -> Account
///   Foo {amount int}:      Account -> Dummy&Person
fn fraud_schema() -> Schema {
    let account = node_dt(
        label("Account"),
        closed(&[("owner", SimpleType::S), ("isBlocked", SimpleType::B)]),
    );
    let dummy_person = node_dt(
        label_and(label("Dummy"), label("Person")),
        closed(&[
            ("owner", SimpleType::S),
            ("isBlocked", SimpleType::B),
            ("isDummy", SimpleType::B),
        ]),
    );

    let transfer = directed_edge(
        label("Transfer"),
        closed(&[("amount", SimpleType::Z)]),
        account.clone(),
        account.clone(),
    );
    let foo_edge = directed_edge(
        label("Foo"),
        closed(&[("amount", SimpleType::Z)]),
        account.clone(),
        dummy_person.clone(),
    );

    Schema::from_parts(vec![account, dummy_person], vec![transfer, foo_edge])
}

/// Social network schema, mirroring fppc::tests::social_schema:
///   Person & Teacher {name str, status str}
///   Person & Student {name str, status int}
///   Comment {content str, status bool}
///   Knows {since int}: Teacher -> Student   (modeled directed)
///   Likes {}:           Teacher -> Comment
///   Author {}:          Comment -> Student
fn social_schema() -> Schema {
    let teacher = node_dt(
        label_and(label("Person"), label("Teacher")),
        closed(&[("name", SimpleType::S), ("status", SimpleType::S)]),
    );
    let student = node_dt(
        label_and(label("Person"), label("Student")),
        closed(&[("name", SimpleType::S), ("status", SimpleType::Z)]),
    );
    let comment = node_dt(
        label("Comment"),
        closed(&[("content", SimpleType::S), ("status", SimpleType::B)]),
    );

    let knows = directed_edge(
        label("Knows"),
        closed(&[("since", SimpleType::Z)]),
        teacher.clone(),
        student.clone(),
    );
    let likes = directed_edge(
        label("Likes"),
        PropertyType::closed_empty(),
        teacher.clone(),
        comment.clone(),
    );
    let author = directed_edge(
        label("Author"),
        PropertyType::closed_empty(),
        comment.clone(),
        student.clone(),
    );

    Schema::from_parts(vec![teacher, student, comment], vec![knows, likes, author])
}

// =======================================================================
// Basic node patterns (untyped)
// =======================================================================

#[test]
fn test_node_empty() {
    assert!(check("()").0.ok);
}
#[test]
fn test_node_var() {
    assert!(check("(x)").0.ok);
}
#[test]
fn test_node_non_empty() {
    assert!(check("(x: Person {owner str})").0.ok);
}

// =======================================================================
// Basic edge patterns
// =======================================================================

// fppc test_node_variable: `->` — gqlite's parser requires at least one
// node anchor. Use `()-[]->()` for the equivalent typecheck.
#[test]
fn test_node_variable() {
    assert!(check("()-[]->()").0.ok);
}

#[test]
fn test_node_variable_2() {
    assert!(check("()-[x: Transfer {amount int}]->()").0.ok);
}

// =======================================================================
// Concatenation
// =======================================================================

#[test]
fn test_concat() {
    assert!(check("(x:Account)(y:Account)").0.ok);
}
#[test]
fn test_concat_1() {
    assert!(check("(x: {a int})(x:Person {b bool, a bool})").0.ok);
}
#[test]
fn test_concat_node_edge() {
    assert!(check("(x: {a int})-[y:Person {b bool, a bool}]->()").0.ok);
}
#[test]
fn test_concat_node_node() {
    assert!(check("(x: {a int}) (y: {b bool})").0.ok);
}
#[test]
fn test_concat_node_edge_node() {
    assert!(
        check("(x: {a int}) -[y:Person {b bool, a bool}]-> (z: {b bool})")
            .0
            .ok
    );
}
#[test]
fn test_concat_edge_edge() {
    assert!(check("()-[x:Person]->()-[y:University]->()").0.ok);
}
#[test]
fn test_concat_node_node_node_edge() {
    assert!(
        check("(x: {a int}) (y: {b bool}) (z: {c str}) -[w:Person]-> ()")
            .0
            .ok
    );
}
#[test]
fn test_concat_node_node_empty() {
    assert!(check("(x: {a int}) (y: {a bool})").0.ok);
}

// =======================================================================
// Schema-based emptiness (fraud)
// =======================================================================

#[test]
fn test_empty_1() {
    let (r, _, _) = check_with(fraud_schema(), "(x: {a int})");
    assert!(r.ok);
    assert!(r.empty);
}

#[test]
fn test_empty_2() {
    let (r, _, _) = check_with(fraud_schema(), "()-[x: {nonExistant int}]->()");
    assert!(r.ok);
    assert!(r.empty);
}

// =======================================================================
// Filter / WHERE
// =======================================================================

#[test]
fn test_where() {
    assert!(
        check("(: {b str}) (x: {a str, b str} WHERE x.a = x.b)")
            .0
            .ok
    );
}

#[test]
fn test_filter_4() {
    // Just check it doesn't panic.
    let _ = check("()-[y WHERE y.a]->()");
}

#[test]
fn test_no_warnings() {
    let (r, _, w) = check("()-[y WHERE y.amount>=3500000]->()");
    assert!(r.ok);
    assert!(w.is_empty(), "expected no warnings, got: {:?}", w);
}

#[test]
fn test_bad_attribute() {
    // Untyped: gradual typing means a bogus attribute name doesn't
    // make the query invalid — just produces Star and works.
    assert!(check("(x: {amount int} WHERE x.amout > 1000)").0.ok);
}

// =======================================================================
// Union
// =======================================================================

#[test]
fn test_union() {
    assert!(check("(x: {a int}) | (y: {b bool})").0.ok);
}
#[test]
fn test_union_2() {
    assert!(check("(x: {a int}) | (x: {b bool})").0.ok);
}
#[test]
fn test_union_heterogeneous() {
    // fppc: `(x: {a int}) | -[x: {a bool}]->`. gqlite needs node anchors.
    assert!(check("(x: {a int}) | ()-[x: {a bool}]->()").0.ok);
}
#[test]
fn test_union_concat_fail() {
    assert!(check("((:{a int}) | (:{a bool})) (:{a str})").0.ok);
}
#[test]
fn test_union_concat_ok() {
    assert!(check("((:{a int}) | (:{a bool})) (:{a int})").0.ok);
}
#[test]
fn test_zero_path() {
    assert!(check("((:{a int})(:{a bool})) | ()").0.ok);
}

// =======================================================================
// Repetition / quantifier
// =======================================================================

#[test]
fn test_repetition_1() {
    assert!(check("(x: {a int}){1,2}").0.ok);
}
#[test]
fn test_repetition_2() {
    assert!(check("(()-[x: {a int}]->()){1,2}").0.ok);
}
#[test]
fn test_repetition_3() {
    let (r, _, _) = check_with(fraud_schema(), "(y)((()-[x: {a int}]->()){1,2}){2,3}");
    assert!(r.ok);
}
#[test]
fn test_repetition_4() {
    // Just check it doesn't panic.
    let _ = check("(()-[]->()){1,2}");
}
#[test]
fn test_foo() {
    assert!(check("(()-[x]->()){1,3}").0.ok);
}

// =======================================================================
// Misc
// =======================================================================

#[test]
fn test_bad_pop() {
    assert!(check("()()").0.ok);
}

#[test]
fn test_readers_digest_ex1() {
    assert!(
        check("(x)-[z:Transfer WHERE z.amount>1000000]->(y WHERE y.isBlocked=true)")
            .0
            .ok
    );
}

// =======================================================================
// is / as
// =======================================================================

#[test]
fn test_is() {
    assert!(check("(x: {a int} WHERE x.a int)").0.ok);
}
#[test]
fn test_as() {
    assert!(check("(x: {a int} WHERE x.a as bool)").0.ok);
}

// =======================================================================
// Error detection
// =======================================================================

#[test]
fn test_example21() {
    // Same variable name for node and edge — the env meet at Concat
    // collapses incompatible shapes.
    let (r, errs, _) = check("(x)-[x]->()");
    assert!(
        !r.ok,
        "expected !ok for shared var across node/edge, errors={:?}",
        errs
    );
}

#[test]
fn test_example22() {
    // Same variable with incompatible property types -> empty
    let (r, _, _) = check("(x: {status bool} WHERE x.status = true)-[:Knows]->(x: {status str})");
    assert!(r.empty);
}

#[test]
fn test_example23() {
    // Untyped: ok, no warnings
    let (r1, _, w1) = check("(x: {stauts int} WHERE x.stauts > 0)");
    assert!(r1.ok);
    assert!(
        w1.is_empty(),
        "expected no warnings on untyped, got: {:?}",
        w1
    );

    // With closed schema {status bool} — typo "stauts" doesn't match -> empty
    let schema = Schema::from_parts(
        vec![node_dt(
            LabelType::Star,
            closed(&[("status", SimpleType::B)]),
        )],
        vec![],
    );
    let (r2, _, _) = check_with(schema, "(x: {stauts int} WHERE x.stauts > 0)");
    assert!(r2.empty);
}

#[test]
fn test_example24() {
    let schema = Schema::from_parts(
        vec![node_dt(
            LabelType::Star,
            closed(&[("status", SimpleType::B)]),
        )],
        vec![],
    );
    let (r, _, _) = check_with(schema, "(x: {status bool} WHERE x.status > 0)");
    assert!(r.empty);
}

#[test]
fn test_unbound_variable() {
    let (r, _, _) = check("(y WHERE x.status = true)");
    assert!(!r.ok);
}

// =======================================================================
// Pure label subtype
// =======================================================================

#[test]
fn test_is_subtype() {
    let l = label_and(label("Person"), label("Teacher"));
    assert!(LabelType::is_subtype(&l, &l));
}

// =======================================================================
// Social network schema
// =======================================================================

#[test]
fn test_social_1() {
    let (r, _, _) = check_with(social_schema(), "(x WHERE x.status=true)");
    assert!(!r.empty);
}

#[test]
fn test_paper_example_1_part_1() {
    let (r, _, _) = check_with(social_schema(), "(x : Teacher) -[: Likes]->()");
    assert!(!r.empty);
}

#[test]
fn test_paper_example_1_part_2() {
    let (r, _, _) = check_with(
        social_schema(),
        "(: Student ) -[y : Knows WHERE y . since < 2019]- (x)",
    );
    assert!(!r.empty);
}

// fppc test_incompatible_records uses `(x: {{a bool}})` — nested record
// syntax that gqlite's parser treats differently. Skipped in phase 1; the
// underlying lattice test (Record vs Record incompatibility) is exercised
// by the closed-schema tests above.

// =======================================================================
// Unary operators (fraud)
// =======================================================================

#[test]
fn test_unop_1() {
    let (r, _, _) = check_with(fraud_schema(), "(x WHERE not x.isBlocked)");
    assert!(!r.empty);
}

#[test]
fn test_unop_2() {
    let (r, _, _) = check_with(fraud_schema(), "()-[x WHERE -x.amount < 0]->()");
    assert!(!r.empty);
}

// =======================================================================
// RETURN-clause checking (aggregates and plain expressions)
// =======================================================================

/// Parse, elaborate, and typecheck a full MATCH ... RETURN query under
/// the permissive star schema. Used by aggregate-clause tests.
fn check_full_query(query: &str) -> (TypecheckResult, Vec<String>, Vec<String>) {
    let q = parser::parse_query(query).expect("parse failed");
    let q = elaborate::elaborate_query(q);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    (r, tc.errors.clone(), tc.warnings.clone())
}

#[test]
fn test_check_count_star() {
    let (r, errs, _) = check_full_query("MATCH (x) RETURN COUNT(*)");
    assert!(r.ok, "expected ok, errors={errs:?}");
}

#[test]
fn test_check_count_star_with_alias() {
    let (r, errs, _) = check_full_query("MATCH (x) RETURN COUNT(*) AS total");
    assert!(r.ok, "expected ok, errors={errs:?}");
}

#[test]
fn test_check_aggregate_unbound_var() {
    let (r, errs, _) = check_full_query("MATCH (x) RETURN COUNT(z.foo)");
    assert!(!r.ok, "expected !ok for unbound z");
    assert!(
        errs.iter().any(|e| e.contains("z")),
        "expected an error mentioning z, got: {errs:?}"
    );
}

#[test]
fn test_check_sum_unbound_var() {
    let (r, errs, _) = check_full_query("MATCH (x) RETURN SUM(z.amount)");
    assert!(!r.ok, "expected !ok for unbound z in SUM");
    assert!(
        errs.iter().any(|e| e.contains("z")),
        "expected an error mentioning z, got: {errs:?}"
    );
}

#[test]
fn test_check_mixed_aggregate_and_expr_requires_explicit_groupby() {
    let (r_no_gb, errs_no_gb, _) = check_full_query("MATCH (x) RETURN x.foo, COUNT(*)");
    assert!(!r_no_gb.ok, "expected error without GROUP BY");
    assert!(
        errs_no_gb.iter().any(|e| e.contains("GROUP BY")),
        "expected GROUP BY error, got: {errs_no_gb:?}"
    );

    let (r_gb, errs_gb, _) = check_full_query("MATCH (x) GROUP BY x.foo RETURN x.foo, COUNT(*)");
    assert!(r_gb.ok, "expected ok with GROUP BY, errs={errs_gb:?}");
}

#[test]
fn test_check_aggregate_distinct_inner_typed() {
    let (r, errs, _) = check_full_query("MATCH (x) RETURN COUNT(DISTINCT x.city)");
    assert!(r.ok, "expected ok, errors={errs:?}");
}

#[test]
fn test_check_plain_return_unbound_var_now_caught() {
    // Regression: RETURN was not type-checked at all before; unbound
    // vars in plain RETURN exprs slipped through. Now they're caught.
    let (r, errs, _) = check_full_query("MATCH (x) RETURN z.name");
    assert!(!r.ok, "expected !ok for unbound z in plain RETURN");
    assert!(
        errs.iter().any(|e| e.contains("z")),
        "expected an error mentioning z, got: {errs:?}"
    );
}

// -----------------------------------------------------------------------
// EXISTS / NOT EXISTS — Phase 1 (parse + typecheck + scoping)
// -----------------------------------------------------------------------

#[test]
fn test_exists_returns_bool() {
    // The body is well-formed, outer `a` is bound, the predicate types
    // as Bool — no errors, no warnings about unknown variables.
    let (r, errs, _) =
        check_full_query("MATCH (a) WHERE EXISTS { (a)-[:KNOWS]->(b) } RETURN a.name");
    assert!(r.ok, "expected ok, errors={errs:?}");
}

#[test]
fn test_not_exists_returns_bool() {
    let (r, errs, _) =
        check_full_query("MATCH (a) WHERE NOT EXISTS { (a)-[:KNOWS]->(b) } RETURN a.name");
    assert!(r.ok, "expected ok, errors={errs:?}");
}

#[test]
fn test_exists_outer_correlation_visible_inside() {
    // `friend` is introduced by the outer match and referenced inside
    // the body. The body must typecheck without "variable not found".
    let (r, errs, _) = check_full_query(
        "MATCH (person)-[:KNOWS]->(friend) \
         WHERE EXISTS { (friend)<-[:HAS_CREATOR]-(post) } \
         RETURN person.name",
    );
    assert!(r.ok, "expected ok, errors={errs:?}");
}

#[test]
fn test_exists_inner_var_does_not_leak_to_return() {
    // `b` is only bound inside the EXISTS body. Referencing it in RETURN
    // must fail with an unbound-variable error.
    let (r, errs, _) =
        check_full_query("MATCH (a) WHERE EXISTS { (a)-[:KNOWS]->(b) } RETURN a.name, b.name");
    assert!(!r.ok, "expected !ok for inner-var leak to RETURN");
    assert!(
        errs.iter().any(|e| e.contains("b")),
        "expected an error mentioning b, got: {errs:?}"
    );
}

#[test]
fn test_not_exists_inner_var_does_not_leak_to_return() {
    let (r, errs, _) =
        check_full_query("MATCH (a) WHERE NOT EXISTS { (a)-[:KNOWS]->(b) } RETURN a.name, b.name");
    assert!(!r.ok, "expected !ok for inner-var leak to RETURN");
    assert!(
        errs.iter().any(|e| e.contains("b")),
        "expected an error mentioning b, got: {errs:?}"
    );
}

#[test]
fn test_exists_with_inner_where() {
    // Mirrors the LDBC IC6-shape body: MATCH ... WHERE ... inside the
    // EXISTS braces. The inner WHERE references both an inner-only var
    // (`post2`) and an outer-correlated one (`friend`).
    let (r, errs, _) = check_full_query(
        "MATCH (person)-[:KNOWS]->(friend) \
         WHERE NOT EXISTS { \
           MATCH (friend)<-[:HAS_CREATOR]-(post2:Post) \
           WHERE post2.score > 0 \
         } \
         RETURN person.name",
    );
    assert!(r.ok, "expected ok, errors={errs:?}");
}

#[test]
fn test_exists_nested() {
    // EXISTS inside EXISTS — the inner `c` is local to the inner body,
    // `b` is local to the outer EXISTS body, `a` correlates from the
    // outermost MATCH.
    let (r, errs, _) = check_full_query(
        "MATCH (a) \
         WHERE EXISTS { \
           MATCH (a)-[:KNOWS]->(b) \
           WHERE EXISTS { (b)-[:LIKES]->(c) } \
         } \
         RETURN a.name",
    );
    assert!(r.ok, "expected ok, errors={errs:?}");
}
