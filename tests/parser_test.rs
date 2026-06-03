use gqlrust::model::value::Value;
use gqlrust::parser::{parse, parse_query};
use gqlrust::syntax::descriptor::Descriptor;
use gqlrust::syntax::expr::{BinOp, Expr, UnOp};
use gqlrust::syntax::path_pattern::PathPattern;
use gqlrust::syntax::query::{
    Aggregator, GeneralSetKind, MatchStatement, ReturnItem, SetQuantifier,
};
use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::simple_type::SimpleType;

fn star_desc() -> Descriptor {
    Descriptor::type_only(DescriptorType::star())
}

fn var_star(name: &str) -> Descriptor {
    Descriptor::new(Some(name.into()), DescriptorType::star())
}

fn label_dt(label: &str) -> DescriptorType {
    DescriptorType::new(LabelType::Label(label.into()), PropertyType::open_empty())
}

// ==================== Parser tests from parser_test.py ====================

#[test]
fn test_node_empty() {
    assert_eq!(parse("()").unwrap(), PathPattern::Node(Some(star_desc())));
}

#[test]
fn test_node_variable() {
    assert_eq!(
        parse("(x)").unwrap(),
        PathPattern::Node(Some(var_star("x")))
    );
}

#[test]
fn test_descriptor() {
    assert_eq!(
        parse("(x:Person)").unwrap(),
        PathPattern::Node(Some(Descriptor::new(Some("x".into()), label_dt("Person"),)))
    );
}

#[test]
fn test_descriptor_empty_record() {
    assert_eq!(
        parse("(x:Person {})").unwrap(),
        PathPattern::Node(Some(Descriptor::new(Some("x".into()), label_dt("Person"),)))
    );
}

#[test]
fn test_descriptor_record() {
    let mut pt = PropertyType::open_empty();
    pt.extend("a".into(), SimpleType::Z);
    assert_eq!(
        parse("(x :Person {a int})").unwrap(),
        PathPattern::Node(Some(Descriptor::new(
            Some("x".into()),
            DescriptorType::new(LabelType::Label("Person".into()), pt),
        )))
    );
}

#[test]
fn test_descriptor_record_multiple() {
    let mut pt = PropertyType::open_empty();
    pt.extend("a".into(), SimpleType::Z);
    pt.extend("b".into(), SimpleType::B);
    assert_eq!(
        parse("(:Person {a int, b bool})").unwrap(),
        PathPattern::Node(Some(Descriptor::new(
            None,
            DescriptorType::new(LabelType::Label("Person".into()), pt),
        )))
    );
}

#[test]
fn test_descriptor_record_double_colon() {
    let mut pt = PropertyType::open_empty();
    pt.extend("a".into(), SimpleType::Z);
    pt.extend("b".into(), SimpleType::B);
    assert_eq!(
        parse("(:Person {a :: int, b :: bool})").unwrap(),
        PathPattern::Node(Some(Descriptor::new(
            None,
            DescriptorType::new(LabelType::Label("Person".into()), pt),
        )))
    );
}

#[test]
fn test_descriptor_record_typed_keyword() {
    let mut pt = PropertyType::open_empty();
    pt.extend("a".into(), SimpleType::Z);
    pt.extend("b".into(), SimpleType::B);
    assert_eq!(
        parse("(:Person {a TYPED int, b TYPED bool})").unwrap(),
        PathPattern::Node(Some(Descriptor::new(
            None,
            DescriptorType::new(LabelType::Label("Person".into()), pt),
        )))
    );
}

#[test]
fn test_descriptor_record_mixed_separators() {
    // The three type-ascription forms are interchangeable within a single
    // descriptor: implicit, `::`, and `TYPED`.
    let mut pt = PropertyType::open_empty();
    pt.extend("a".into(), SimpleType::Z);
    pt.extend("b".into(), SimpleType::B);
    pt.extend("c".into(), SimpleType::S);
    assert_eq!(
        parse("(:Person {a int, b :: bool, c TYPED str})").unwrap(),
        PathPattern::Node(Some(Descriptor::new(
            None,
            DescriptorType::new(LabelType::Label("Person".into()), pt),
        )))
    );
}

#[test]
fn test_descriptor_value_filter_with_colon_still_works() {
    // `:` is reserved for value filters (elaborated to WHERE).
    let parsed = parse("(:Person {age: 30})").unwrap();
    let s = format!("{parsed:?}");
    assert!(s.contains("value_filters"), "got: {s}");
    assert!(s.contains("\"age\""), "got: {s}");
}

// ISO/IEC 39075:2024 §16: `elementPropertySpecification` is a sibling of
// `isLabelExpression` under `elementPatternFiller`, so the colon is
// optional in front of the `{...}` record.

#[test]
fn test_node_bare_property_spec_anonymous() {
    // `({k: v})` — no variable, no colon, just a property spec. Label
    // defaults to Star (any), value filter hoisted by elaboration.
    let parsed = parse("({age: 30})").unwrap();
    let s = format!("{parsed:?}");
    assert!(s.contains("value_filters"), "got: {s}");
    assert!(s.contains("\"age\""), "got: {s}");
    assert!(s.contains("Star"), "got: {s}");
}

#[test]
fn test_node_bare_property_spec_with_var() {
    // `(x {k: v})` — variable plus property spec, no colon.
    let parsed = parse("(x {age: 30})").unwrap();
    let s = format!("{parsed:?}");
    assert!(s.contains("value_filters"), "got: {s}");
    assert!(s.contains("\"x\""), "got: {s}");
    assert!(s.contains("\"age\""), "got: {s}");
}

#[test]
fn test_edge_bare_property_spec_anonymous() {
    // `-[{k: v}]->` — no variable, no colon, just a property spec.
    let parsed = parse("-[{since: 2020}]->").unwrap();
    let s = format!("{parsed:?}");
    assert!(s.contains("value_filters"), "got: {s}");
    assert!(s.contains("\"since\""), "got: {s}");
}

#[test]
fn test_edge_bare_property_spec_with_var() {
    // `-[e {k: v}]->` — variable plus property spec, no colon.
    let parsed = parse("-[e {since: 2020}]->").unwrap();
    let s = format!("{parsed:?}");
    assert!(s.contains("value_filters"), "got: {s}");
    assert!(s.contains("\"e\""), "got: {s}");
    assert!(s.contains("\"since\""), "got: {s}");
}

#[test]
fn test_typed_predicate_in_where_explicit() {
    // `TYPED` is the explicit type-predicate operator in expressions.
    assert!(parse("((x) WHERE x.a TYPED int)").is_ok());
}

#[test]
fn test_typed_predicate_in_where_implicit() {
    // After a value term, a type-head token implies the type predicate.
    assert!(parse("((x) WHERE x.a int)").is_ok());
}

#[test]
fn test_descriptor_no_label() {
    let mut pt = PropertyType::open_empty();
    pt.extend("a".into(), SimpleType::Z);
    pt.extend("b".into(), SimpleType::B);
    assert_eq!(
        parse("(:{a int, b bool})").unwrap(),
        PathPattern::Node(Some(Descriptor::new(
            None,
            DescriptorType::new(LabelType::Star, pt),
        )))
    );
}

#[test]
fn test_edge_right_empty() {
    assert_eq!(
        parse("->").unwrap(),
        PathPattern::EdgeRight(Some(star_desc()))
    );
}

#[test]
fn test_edge_right_empty_alt() {
    assert_eq!(
        parse("-[]->").unwrap(),
        PathPattern::EdgeRight(Some(star_desc()))
    );
}

#[test]
fn test_edge_right_with_descriptor() {
    let mut pt = PropertyType::open_empty();
    pt.extend("a".into(), SimpleType::Z);
    assert_eq!(
        parse("-[x:Person {a int}]->").unwrap(),
        PathPattern::EdgeRight(Some(Descriptor::new(
            Some("x".into()),
            DescriptorType::new(LabelType::Label("Person".into()), pt),
        )))
    );
}

#[test]
fn test_edge_left_empty() {
    assert_eq!(
        parse("<-").unwrap(),
        PathPattern::EdgeLeft(Some(star_desc()))
    );
}

#[test]
fn test_edge_left_empty_alt() {
    assert_eq!(
        parse("<-[]-").unwrap(),
        PathPattern::EdgeLeft(Some(star_desc()))
    );
}

#[test]
fn test_edge_left_with_descriptor() {
    let mut pt = PropertyType::open_empty();
    pt.extend("a".into(), SimpleType::Z);
    assert_eq!(
        parse("<-[x:Person {a int}]-").unwrap(),
        PathPattern::EdgeLeft(Some(Descriptor::new(
            Some("x".into()),
            DescriptorType::new(LabelType::Label("Person".into()), pt),
        )))
    );
}

#[test]
fn test_edge_non_directional_empty() {
    assert_eq!(
        parse("~").unwrap(),
        PathPattern::EdgeUndirected(Some(star_desc()))
    );
}

#[test]
fn test_edge_non_directional_empty_alt() {
    assert_eq!(
        parse("~[]~").unwrap(),
        PathPattern::EdgeUndirected(Some(star_desc()))
    );
}

#[test]
fn test_edge_non_directional_with_descriptor() {
    let mut pt = PropertyType::open_empty();
    pt.extend("a".into(), SimpleType::Z);
    assert_eq!(
        parse("~[x:Person {a int}]~").unwrap(),
        PathPattern::EdgeUndirected(Some(Descriptor::new(
            Some("x".into()),
            DescriptorType::new(LabelType::Label("Person".into()), pt),
        )))
    );
}

#[test]
fn test_concatenation() {
    let x = PathPattern::Node(Some(var_star("x")));
    let y = PathPattern::EdgeUndirected(Some(var_star("y")));
    let z = PathPattern::Node(Some(var_star("z")));
    assert_eq!(
        parse("(x)~[y]~(z)").unwrap(),
        PathPattern::Concat(
            Box::new(PathPattern::Concat(Box::new(x), Box::new(y))),
            Box::new(z),
        )
    );
}

#[test]
fn test_filter_attribute_gt() {
    assert_eq!(
        parse("(x where x.a>10)").unwrap(),
        PathPattern::Filter(
            Box::new(PathPattern::Node(Some(var_star("x")))),
            Expr::Binop {
                op: BinOp::Gt,
                left: Box::new(Expr::AttrLookup {
                    var: "x".into(),
                    attr: "a".into(),
                }),
                right: Box::new(Expr::Const(Value::Int(10))),
            },
        )
    );
}

#[test]
fn test_filter_and() {
    assert_eq!(
        parse("(x where 11>10 and (1 = 2 or 3>='1'))").unwrap(),
        PathPattern::Filter(
            Box::new(PathPattern::Node(Some(var_star("x")))),
            Expr::Binop {
                op: BinOp::And,
                left: Box::new(Expr::Binop {
                    op: BinOp::Gt,
                    left: Box::new(Expr::Const(Value::Int(11))),
                    right: Box::new(Expr::Const(Value::Int(10))),
                }),
                right: Box::new(Expr::Binop {
                    op: BinOp::Or,
                    left: Box::new(Expr::Binop {
                        op: BinOp::Eq,
                        left: Box::new(Expr::Const(Value::Int(1))),
                        right: Box::new(Expr::Const(Value::Int(2))),
                    }),
                    right: Box::new(Expr::Binop {
                        op: BinOp::Ge,
                        left: Box::new(Expr::Const(Value::Int(3))),
                        right: Box::new(Expr::Const(Value::Str("1".into()))),
                    }),
                }),
            },
        )
    );
}

#[test]
fn test_prioritization() {
    // "11 = 10 and 1 = 2 or 1=2" → (11=10 and 1=2) or (1=2)
    // But the Python grammar has logical_op as comparison (lop comparison)*,
    // which is left-to-right, so: ((11=10) and (1=2)) or (1=2)
    assert_eq!(
        parse("(x where 11 = 10 and 1 = 2 or 1=2)").unwrap(),
        PathPattern::Filter(
            Box::new(PathPattern::Node(Some(var_star("x")))),
            Expr::Binop {
                op: BinOp::Or,
                left: Box::new(Expr::Binop {
                    op: BinOp::And,
                    left: Box::new(Expr::Binop {
                        op: BinOp::Eq,
                        left: Box::new(Expr::Const(Value::Int(11))),
                        right: Box::new(Expr::Const(Value::Int(10))),
                    }),
                    right: Box::new(Expr::Binop {
                        op: BinOp::Eq,
                        left: Box::new(Expr::Const(Value::Int(1))),
                        right: Box::new(Expr::Const(Value::Int(2))),
                    }),
                }),
                right: Box::new(Expr::Binop {
                    op: BinOp::Eq,
                    left: Box::new(Expr::Const(Value::Int(1))),
                    right: Box::new(Expr::Const(Value::Int(2))),
                }),
            },
        )
    );
}

#[test]
fn test_simple_logical() {
    assert_eq!(
        parse("(x where true and 1>2)").unwrap(),
        PathPattern::Filter(
            Box::new(PathPattern::Node(Some(var_star("x")))),
            Expr::Binop {
                op: BinOp::And,
                left: Box::new(Expr::Const(Value::Bool(true))),
                right: Box::new(Expr::Binop {
                    op: BinOp::Gt,
                    left: Box::new(Expr::Const(Value::Int(1))),
                    right: Box::new(Expr::Const(Value::Int(2))),
                }),
            },
        )
    );
}

#[test]
fn test_simple_arithmetic() {
    // "x.a>x.b>1" → left-assoc: (x.a > x.b) > 1
    assert_eq!(
        parse("(x where x.a>x.b>1)").unwrap(),
        PathPattern::Filter(
            Box::new(PathPattern::Node(Some(var_star("x")))),
            Expr::Binop {
                op: BinOp::Gt,
                left: Box::new(Expr::Binop {
                    op: BinOp::Gt,
                    left: Box::new(Expr::AttrLookup {
                        var: "x".into(),
                        attr: "a".into(),
                    }),
                    right: Box::new(Expr::AttrLookup {
                        var: "x".into(),
                        attr: "b".into(),
                    }),
                }),
                right: Box::new(Expr::Const(Value::Int(1))),
            },
        )
    );
}

#[test]
fn test_filter_on_edge() {
    let x = PathPattern::Node(Some(var_star("x")));
    let y = PathPattern::Filter(
        Box::new(PathPattern::EdgeRight(Some(var_star("y")))),
        Expr::Binop {
            op: BinOp::Gt,
            left: Box::new(Expr::AttrLookup {
                var: "y".into(),
                attr: "a".into(),
            }),
            right: Box::new(Expr::Const(Value::Int(10))),
        },
    );
    let z = PathPattern::Node(Some(var_star("z")));
    assert_eq!(
        parse("(x)-[y where y.a>10]->(z)").unwrap(),
        PathPattern::Concat(
            Box::new(PathPattern::Concat(Box::new(x), Box::new(y))),
            Box::new(z),
        )
    );
}

#[test]
fn test_repetition() {
    assert_eq!(
        parse("(x)*").unwrap(),
        PathPattern::Repeat {
            pattern: Box::new(PathPattern::Node(Some(var_star("x")))),
            lb: 0,
            ub: None,
        }
    );
    assert_eq!(
        parse("(x)+").unwrap(),
        PathPattern::Repeat {
            pattern: Box::new(PathPattern::Node(Some(var_star("x")))),
            lb: 1,
            ub: None,
        }
    );
    assert_eq!(
        parse("(x){1,2}").unwrap(),
        PathPattern::Repeat {
            pattern: Box::new(PathPattern::Node(Some(var_star("x")))),
            lb: 1,
            ub: Some(2),
        }
    );
    assert_eq!(
        parse("(x){2,}").unwrap(),
        PathPattern::Repeat {
            pattern: Box::new(PathPattern::Node(Some(var_star("x")))),
            lb: 2,
            ub: None,
        }
    );
}

#[test]
fn test_questioned_edge() {
    assert_eq!(
        parse("-[z]->?").unwrap(),
        PathPattern::Questioned(Box::new(PathPattern::EdgeRight(Some(var_star("z")))))
    );
}

#[test]
fn test_label_and() {
    assert_eq!(
        parse("(:Person & Company)").unwrap(),
        PathPattern::Node(Some(Descriptor::new(
            None,
            DescriptorType::new(
                LabelType::And(
                    Box::new(LabelType::Label("Person".into())),
                    Box::new(LabelType::Label("Company".into())),
                ),
                PropertyType::open_empty(),
            ),
        )))
    );
}

#[test]
fn test_union() {
    assert_eq!(
        parse("() | ()").unwrap(),
        PathPattern::Union(
            Box::new(PathPattern::Node(Some(star_desc()))),
            Box::new(PathPattern::Node(Some(star_desc()))),
        )
    );
}

#[test]
fn test_descriptor_record_closed() {
    let mut pt = PropertyType::closed_empty();
    pt.extend("a".into(), SimpleType::Z);
    assert_eq!(
        parse("(x :Person {{a int}})").unwrap(),
        PathPattern::Node(Some(Descriptor::new(
            Some("x".into()),
            DescriptorType::new(LabelType::Label("Person".into()), pt),
        )))
    );
}

#[test]
fn test_unop_1() {
    assert_eq!(
        parse("(x WHERE not x.status)").unwrap(),
        PathPattern::Filter(
            Box::new(PathPattern::Node(Some(var_star("x")))),
            Expr::Unop {
                op: UnOp::Not,
                operand: Box::new(Expr::AttrLookup {
                    var: "x".into(),
                    attr: "status".into(),
                }),
            },
        )
    );
}

#[test]
fn test_unop_2() {
    assert_eq!(
        parse("(x WHERE -x.status>0)").unwrap(),
        PathPattern::Filter(
            Box::new(PathPattern::Node(Some(var_star("x")))),
            Expr::Binop {
                op: BinOp::Gt,
                left: Box::new(Expr::Unop {
                    op: UnOp::Neg,
                    operand: Box::new(Expr::AttrLookup {
                        var: "x".into(),
                        attr: "status".into(),
                    }),
                }),
                right: Box::new(Expr::Const(Value::Int(0))),
            },
        )
    );
}

#[test]
fn test_unop_filter_pattern() {
    // ((x) WHERE -x.status>0) — inner path pattern is (x), then filter
    assert_eq!(
        parse("((x) WHERE -x.status>0)").unwrap(),
        PathPattern::Filter(
            Box::new(PathPattern::Node(Some(var_star("x")))),
            Expr::Binop {
                op: BinOp::Gt,
                left: Box::new(Expr::Unop {
                    op: UnOp::Neg,
                    operand: Box::new(Expr::AttrLookup {
                        var: "x".into(),
                        attr: "status".into(),
                    }),
                }),
                right: Box::new(Expr::Const(Value::Int(0))),
            },
        )
    );
}

// ==================== Comma-join (Q1, Q2) tests ====================

#[test]
fn test_join_simple() {
    // Two node patterns joined by comma
    assert_eq!(
        parse("(x), (y)").unwrap(),
        PathPattern::Join(
            Box::new(PathPattern::Node(Some(var_star("x")))),
            Box::new(PathPattern::Node(Some(var_star("y")))),
        )
    );
}

#[test]
fn test_join_paths() {
    // Two path patterns joined by comma
    let result = parse("(x) -[]-> (), (y) -[]-> ()").unwrap();
    match &result {
        PathPattern::Join(left, right) => {
            assert!(matches!(left.as_ref(), PathPattern::Concat(_, _)));
            assert!(matches!(right.as_ref(), PathPattern::Concat(_, _)));
        }
        _ => panic!("expected Join, got {result:?}"),
    }
}

#[test]
fn test_join_three_way() {
    // Three patterns: left-associative
    let result = parse("(x), (y), (z)").unwrap();
    match &result {
        PathPattern::Join(left, right) => {
            // The inner left should itself be a Join
            assert!(matches!(left.as_ref(), PathPattern::Join(_, _)));
            assert!(matches!(right.as_ref(), PathPattern::Node(_)));
        }
        _ => panic!("expected Join, got {result:?}"),
    }
}

#[test]
fn test_join_with_labels() {
    // (x: Car) -[]-> (y), (:Person) -[]-> (y)
    let result = parse("(x: Car) -[]-> (y), (:Person) -[]-> (y)").unwrap();
    assert!(matches!(result, PathPattern::Join(_, _)));
}

#[test]
fn test_join_lower_precedence_than_union() {
    // (x) | (y), (z)  should parse as  ((x) | (y)), (z)
    let result = parse("(x) | (y), (z)").unwrap();
    match &result {
        PathPattern::Join(left, right) => {
            assert!(matches!(left.as_ref(), PathPattern::Union(_, _)));
            assert!(matches!(right.as_ref(), PathPattern::Node(_)));
        }
        _ => panic!("expected Join, got {result:?}"),
    }
}

#[test]
fn test_join_shared_var() {
    // (x) -[]-> (), (:Car) -[]-> (x) -[]-> ()
    let result = parse("(x) -[]-> (), (:Car) -[]-> (x) -[]-> ()").unwrap();
    assert!(matches!(result, PathPattern::Join(_, _)));
    // Both sides should have "x" in their free variables
    let fv = result.freevars();
    assert!(fv.contains("x"));
}

// ==================== MATCH/WHERE/RETURN tests ====================

#[test]
fn test_match_simple() {
    let q = gqlrust::compile_query("MATCH (x)").unwrap();
    assert!(matches!(q.collapsed_pattern(), PathPattern::Node(_)));
    assert!(q.returns.is_none());
}

/// Pin structural invariants for a minimal query: one `MATCH`, emitted as
/// `MatchStatement::Simple`. Multi-clause and `OPTIONAL MATCH` queries are
/// covered in `optional_match_test` and `multi_match_proptest`.
#[test]
fn test_query_has_one_simple_match_statement() {
    let q = gqlrust::compile_query("MATCH (x)-[:Knows]->(y) RETURN x.name").unwrap();
    assert_eq!(q.matches.len(), 1, "parser must produce exactly one match");
    assert!(
        matches!(&q.matches[0], MatchStatement::Simple { .. }),
        "expected `MATCH ...` without OPTIONAL to parse as `Simple`"
    );
}

#[test]
fn test_match_where_return() {
    let q = gqlrust::compile_query(
        "MATCH (x) -[:Transfer]-> (y) WHERE x.amount > 100 RETURN x.name, y.name",
    )
    .unwrap();
    // Pushdown absorbs `x.amount > 100` into the descriptor's `value_preds`,
    // so no Filter wrapper survives. Verify the predicate landed on `x`.
    let pat = q.collapsed_pattern();
    assert!(!matches!(pat, PathPattern::Filter(_, _)));
    let mut found = false;
    fn scan(p: &PathPattern, found: &mut bool) {
        match p {
            PathPattern::Node(Some(d)) if d.var.as_deref() == Some("x") => {
                *found = !d.value_preds.is_empty();
            }
            PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
                scan(a, found);
                scan(b, found);
            }
            PathPattern::Filter(inner, _) | PathPattern::Questioned(inner) => scan(inner, found),
            PathPattern::Repeat { pattern, .. } => scan(pattern, found),
            _ => {}
        }
    }
    scan(&pat, &mut found);
    assert!(found, "expected value_preds on x after pushdown");
    assert_eq!(q.returns.as_ref().unwrap().len(), 2);
    assert!(!q.distinct);
}

#[test]
fn test_match_return_distinct() {
    let q = gqlrust::compile_query("MATCH (x) -[]-> (y) RETURN DISTINCT x.name").unwrap();
    assert!(q.distinct);
    assert_eq!(q.returns.as_ref().unwrap().len(), 1);
}

#[test]
fn test_match_return_alias() {
    let q = gqlrust::compile_query("MATCH (m: Movie) RETURN m.title AS title, m.votes AS votes")
        .unwrap();
    let returns = q.returns.as_ref().unwrap();
    assert_eq!(returns.len(), 2);
    assert_eq!(returns[0].alias(), Some("title"));
    assert_eq!(returns[1].alias(), Some("votes"));
}

#[test]
fn test_no_match_keyword_still_works() {
    let q = gqlrust::compile_query("(x) -[]-> (y)").unwrap();
    assert!(matches!(q.collapsed_pattern(), PathPattern::Concat(_, _)));
    assert!(q.returns.is_none());
}

// ===== Multi-MATCH parser support (ISO §14.3-14.4) =====
//
// Use `parse_query` directly: `compile_query_unchecked` already runs
// `optimize_query` which collapses the chain to one Simple, hiding
// what the parser actually produces.

#[test]
fn test_multi_match_two_clauses() {
    let q = parse_query("MATCH (x: Account) MATCH (y: Account)").unwrap();
    assert_eq!(q.matches.len(), 2);
    for m in &q.matches {
        assert!(matches!(m, MatchStatement::Simple { .. }));
    }
}

#[test]
fn test_multi_match_three_clauses() {
    let q = parse_query("MATCH (x) MATCH (y) MATCH (z)").unwrap();
    assert_eq!(q.matches.len(), 3);
}

#[test]
fn test_multi_match_per_clause_where() {
    let q = parse_query(
        "MATCH (x: Account) WHERE x.isBlocked = false \
         MATCH (y: Account) WHERE y.isBlocked = true",
    )
    .unwrap();
    assert_eq!(q.matches.len(), 2);
    for m in &q.matches {
        let MatchStatement::Simple { pattern, .. } = m else {
            panic!("expected Simple match");
        };
        assert!(matches!(pattern, PathPattern::Filter(_, _)));
    }
}

#[test]
fn test_multi_match_with_return() {
    let q = parse_query("MATCH (x: Account) MATCH (y: Account) RETURN x.owner, y.owner").unwrap();
    assert_eq!(q.matches.len(), 2);
    assert_eq!(q.returns.as_ref().unwrap().len(), 2);
}

#[test]
fn test_single_match_still_one_clause() {
    let q = parse_query("MATCH (x) -[]-> (y)").unwrap();
    assert_eq!(q.matches.len(), 1);
}

/// `compile_query` runs the full pipeline; `optimize_query` collapses
/// matches to one Simple, so post-pipeline `len == 1` even from multi-MATCH.
#[test]
fn test_multi_match_compiles_end_to_end() {
    let q = gqlrust::compile_query("MATCH (x: Account) MATCH (y) RETURN x.owner").unwrap();
    assert_eq!(q.matches.len(), 1);
}

// =======================================================================
// Aggregate functions (ISO 39075 §20.9)
// =======================================================================

/// Helper: parse a query and return the single aggregate item it produces.
fn single_agg(q: &str) -> Aggregator {
    let parsed = gqlrust::compile_query(q).unwrap();
    let returns = parsed.returns.expect("expected RETURN clause");
    assert_eq!(returns.len(), 1, "expected exactly one return item");
    match returns.into_iter().next().unwrap() {
        ReturnItem::Aggregate { agg, .. } => agg,
        other => panic!("expected Aggregate item, got {other:?}"),
    }
}

#[test]
fn test_parse_count_star() {
    assert_eq!(
        single_agg("MATCH (x) RETURN COUNT(*)"),
        Aggregator::CountStar
    );
}

#[test]
fn test_parse_count_star_lowercase() {
    // Soft-keyword: lowercase aggregate name still recognized.
    assert_eq!(
        single_agg("MATCH (x) RETURN count(*)"),
        Aggregator::CountStar
    );
}

#[test]
fn test_parse_count_expr() {
    let agg = single_agg("MATCH (x) RETURN COUNT(x.name)");
    match agg {
        Aggregator::GeneralSet {
            kind: GeneralSetKind::Count,
            quantifier: SetQuantifier::All,
            ..
        } => {}
        other => panic!("expected GeneralSet{{Count, ALL}}, got {other:?}"),
    }
}

#[test]
fn test_parse_count_distinct() {
    let agg = single_agg("MATCH (x) RETURN COUNT(DISTINCT x.city)");
    match agg {
        Aggregator::GeneralSet {
            kind: GeneralSetKind::Count,
            quantifier: SetQuantifier::Distinct,
            ..
        } => {}
        other => panic!("expected GeneralSet{{Count, DISTINCT}}, got {other:?}"),
    }
}

#[test]
fn test_parse_count_explicit_all() {
    // ISO §20.9: ALL is implicit, but explicit ALL is also valid syntax.
    let agg = single_agg("MATCH (x) RETURN COUNT(ALL x.name)");
    match agg {
        Aggregator::GeneralSet {
            quantifier: SetQuantifier::All,
            ..
        } => {}
        other => panic!("expected ALL quantifier, got {other:?}"),
    }
}

#[test]
fn test_parse_sum_avg_min_max() {
    for (q, expected_kind) in [
        ("MATCH (x) RETURN SUM(x.amount)", GeneralSetKind::Sum),
        ("MATCH (x) RETURN AVG(x.score)", GeneralSetKind::Avg),
        ("MATCH (x) RETURN MIN(x.age)", GeneralSetKind::Min),
        ("MATCH (x) RETURN MAX(x.age)", GeneralSetKind::Max),
    ] {
        let agg = single_agg(q);
        match agg {
            Aggregator::GeneralSet { kind, .. } if kind == expected_kind => {}
            other => panic!("expected GeneralSet{{{expected_kind:?}}}, got {other:?}"),
        }
    }
}

#[test]
fn test_parse_aggregate_with_alias() {
    let parsed = gqlrust::compile_query("MATCH (x) RETURN COUNT(*) AS total").unwrap();
    let returns = parsed.returns.unwrap();
    assert_eq!(returns.len(), 1);
    match &returns[0] {
        ReturnItem::Aggregate { agg, alias } => {
            assert_eq!(*agg, Aggregator::CountStar);
            assert_eq!(alias.as_deref(), Some("total"));
        }
        other => panic!("expected Aggregate, got {other:?}"),
    }
}

#[test]
fn test_parse_mixed_aggregate_and_expr() {
    // RETURN that mixes a plain expr and an aggregate — the parser
    // produces a heterogeneous Vec<ReturnItem>. The full pipeline
    // (compile_query) requires explicit GROUP BY for this to typecheck,
    // but the parser stage doesn't enforce that — use compile_query_unchecked
    // to verify the AST shape without typechecker rejection.
    let parsed = gqlrust::compile_query_unchecked("MATCH (x) RETURN x.country, COUNT(*)").unwrap();
    let returns = parsed.returns.unwrap();
    assert_eq!(returns.len(), 2);
    assert!(matches!(returns[0], ReturnItem::Expr { .. }));
    assert!(matches!(returns[1], ReturnItem::Aggregate { .. }));
}

#[test]
fn test_count_as_field_name_still_works() {
    // Soft-keyword: `count` not followed by `(` stays a regular Name.
    // This query treats `count` as a property name in a record literal.
    let parsed =
        gqlrust::compile_query("MATCH (x) WHERE x.id = 1 RETURN {name: 'ok', count: 1}").unwrap();
    assert_eq!(parsed.returns.unwrap().len(), 1);
}

// ----------------------------- LIMIT clause -------------------------------

#[test]
fn test_limit_after_return() {
    let q = gqlrust::compile_query("MATCH (x) RETURN x.name LIMIT 20").unwrap();
    assert_eq!(q.limit, Some(20));
}

#[test]
fn test_limit_lowercase() {
    // The lexer accepts both casings (matches the existing convention
    // for MATCH/RETURN/WHERE).
    let q = gqlrust::compile_query("MATCH (x) RETURN x.name limit 5").unwrap();
    assert_eq!(q.limit, Some(5));
}

#[test]
fn test_limit_absent_is_none() {
    let q = gqlrust::compile_query("MATCH (x) RETURN x.name").unwrap();
    assert_eq!(q.limit, None);
}

#[test]
fn test_limit_without_return() {
    // LIMIT is independent of RETURN — the runtime applies it to whatever
    // the query produces. `compile_query_unchecked` skips typechecking so
    // we can exercise just the parser shape.
    let q = gqlrust::compile_query_unchecked("MATCH (x) LIMIT 7").unwrap();
    assert_eq!(q.limit, Some(7));
    assert!(q.returns.is_none());
}

#[test]
fn test_limit_zero_parses() {
    // `LIMIT 0` is syntactically valid — ISO 39075:2024 explicitly
    // permits it ("select first 0 records, return empty binding
    // table"). The execution semantics are checked separately by a
    // runtime test; here we only assert the AST shape.
    let q = gqlrust::compile_query("MATCH (x) RETURN x.name LIMIT 0").unwrap();
    assert_eq!(q.limit, Some(0));
}

#[test]
fn test_limit_negative_rejected() {
    let err = gqlrust::compile_query("MATCH (x) RETURN x.name LIMIT -3").unwrap_err();
    assert!(
        err.to_lowercase().contains("limit"),
        "error should mention LIMIT, got {err:?}"
    );
}

#[test]
fn test_limit_after_where() {
    let q = gqlrust::compile_query(
        "MATCH (x) -[:Transfer]-> (y) WHERE x.amount > 100 RETURN y.name LIMIT 50",
    )
    .unwrap();
    assert_eq!(q.limit, Some(50));
    // Sanity: the WHERE-driven pushdown still happened — LIMIT didn't
    // disturb the surrounding parse.
    assert!(q.returns.is_some());
}

#[test]
fn test_limit_with_group_by_aggregate() {
    let q =
        gqlrust::compile_query("MATCH (x) GROUP BY x.country RETURN x.country, COUNT(*) LIMIT 10")
            .unwrap();
    assert_eq!(q.limit, Some(10));
    assert!(q.group_by.is_some());
}

#[test]
fn test_limit_displays_back() {
    // Round-trip Display: a parsed query renders LIMIT N back into its
    // string form, so REPL/error messages stay readable.
    let q = gqlrust::compile_query("MATCH (x) RETURN x.name LIMIT 25").unwrap();
    assert!(
        q.to_string().contains("LIMIT 25"),
        "Display should include LIMIT clause, got {}",
        q
    );
}

// -----------------------------------------------------------------------
// EXISTS / NOT EXISTS — parser-level tests
// -----------------------------------------------------------------------

#[test]
fn test_parse_exists_simple() {
    let q = parse_query("MATCH (a) WHERE EXISTS { (a)-[:KNOWS]->(b) } RETURN a.name").unwrap();
    let pat = q.matches[0].pattern();
    let PathPattern::Filter(_, expr) = pat else {
        panic!("expected MATCH ... WHERE filter, got {pat:?}");
    };
    assert!(
        matches!(expr, Expr::Exists { .. }),
        "expected Expr::Exists, got {expr:?}"
    );
}

#[test]
fn test_parse_not_exists_distinct_from_unop_not() {
    // `NOT EXISTS { ... }` produces a dedicated `Expr::NotExists`, not
    // `Expr::Unop { op: Not, operand: Expr::Exists }`. The optimiser
    // relies on the distinction.
    let q = parse_query("MATCH (a) WHERE NOT EXISTS { (a)-[:KNOWS]->(b) } RETURN a.name").unwrap();
    let pat = q.matches[0].pattern();
    let PathPattern::Filter(_, expr) = pat else {
        panic!("expected MATCH ... WHERE filter, got {pat:?}");
    };
    assert!(
        matches!(expr, Expr::NotExists { .. }),
        "expected Expr::NotExists, got {expr:?}"
    );
}

#[test]
fn test_parse_exists_with_inner_match_keyword_and_where() {
    // MATCH inside the body is optional but accepted, as is an inner
    // WHERE on the path pattern.
    let q = parse_query(
        "MATCH (a) WHERE EXISTS { \
           MATCH (a)-[:KNOWS]->(b) WHERE b.active = true \
         } RETURN a.name",
    )
    .unwrap();
    let pat = q.matches[0].pattern();
    let PathPattern::Filter(_, expr) = pat else {
        panic!("expected MATCH ... WHERE filter, got {pat:?}");
    };
    let body = match expr {
        Expr::Exists { body } => body,
        _ => panic!("expected Expr::Exists, got {expr:?}"),
    };
    assert_eq!(body.matches.len(), 1);
    // The inner WHERE folded into the path pattern as a Filter.
    assert!(matches!(
        body.matches[0],
        MatchStatement::Simple {
            pattern: PathPattern::Filter(_, _),
            ..
        }
    ));
}

#[test]
fn test_parse_exists_rejects_return_inside_body() {
    let err = parse_query("MATCH (a) WHERE EXISTS { (a)-[:KNOWS]->(b) RETURN b } RETURN a.name")
        .unwrap_err();
    assert!(
        err.contains("RETURN"),
        "error should mention RETURN, got {err:?}"
    );
}

#[test]
fn test_parse_exists_rejects_limit_inside_body() {
    let err = parse_query("MATCH (a) WHERE EXISTS { (a)-[:KNOWS]->(b) LIMIT 1 } RETURN a.name")
        .unwrap_err();
    assert!(
        err.to_uppercase().contains("LIMIT"),
        "error should mention LIMIT, got {err:?}"
    );
}

#[test]
fn test_parse_exists_multi_match_body() {
    // Two match clauses in the body: a regular MATCH followed by an
    // OPTIONAL MATCH. Both are accepted.
    let q = parse_query(
        "MATCH (a) WHERE EXISTS { \
           MATCH (a)-[:KNOWS]->(b) \
           OPTIONAL MATCH (b)-[:LIKES]->(c) \
         } RETURN a.name",
    )
    .unwrap();
    let pat = q.matches[0].pattern();
    let PathPattern::Filter(_, expr) = pat else {
        panic!("expected MATCH ... WHERE filter, got {pat:?}");
    };
    let body = match expr {
        Expr::Exists { body } => body,
        _ => panic!("expected Expr::Exists, got {expr:?}"),
    };
    assert_eq!(body.matches.len(), 2);
    assert!(matches!(body.matches[0], MatchStatement::Simple { .. }));
    assert!(matches!(body.matches[1], MatchStatement::Optional { .. }));
}

#[test]
fn test_lexer_line_comment_dash_dash() {
    // `--` to end of line (ISO GQL §3.10).
    let q = parse_query("-- header\nMATCH (p) RETURN p.x AS x").unwrap();
    assert_eq!(q.matches.len(), 1);
}

#[test]
fn test_lexer_block_comment() {
    // `/* ... */` block, non-nesting.
    let q = parse_query("/* a */ MATCH (p) /* b\nb */ RETURN p.x AS x").unwrap();
    assert_eq!(q.matches.len(), 1);
}

#[test]
fn test_lexer_dash_dash_arrow_is_edge_not_comment() {
    // `-->` is the unlabeled forward-edge sugar from §5.x — it must
    // tokenize as Minus + RightArrow, not be eaten by the `--` line
    // comment. Without disambiguation in the lexer, the rest of the
    // line gets consumed and parsing dies on EOF.
    let _ = parse("-->{1,2}").expect("--> must lex as edge, not comment");
}

#[test]
fn test_lexer_ne_alias_diamond() {
    // `<>` is the ISO GQL not-equal alias for `!=`.
    let q1 = parse_query("MATCH (a) WHERE a.x <> 1 RETURN a.x AS x").unwrap();
    let q2 = parse_query("MATCH (a) WHERE a.x != 1 RETURN a.x AS x").unwrap();
    let pat1 = q1.matches[0].pattern();
    let pat2 = q2.matches[0].pattern();
    let (PathPattern::Filter(_, e1), PathPattern::Filter(_, e2)) = (pat1, pat2) else {
        panic!("expected Filter on both sides");
    };
    let (
        Expr::Binop {
            op: op1,
            left: l1,
            right: r1,
        },
        Expr::Binop {
            op: op2,
            left: l2,
            right: r2,
        },
    ) = (e1, e2)
    else {
        panic!("expected Binop on both sides");
    };
    assert!(matches!(op1, BinOp::Ne));
    assert!(matches!(op2, BinOp::Ne));
    assert_eq!(format!("{l1:?} {r1:?}"), format!("{l2:?} {r2:?}"));
}

#[test]
fn test_parse_exists_nested_in_unop_not() {
    // `NOT EXISTS` is parsed as the dedicated variant, but `NOT (EXISTS
    // { ... })` with parens — if someone writes the parens — would
    // produce `Unop(Not, Exists)`. Smoke-test that the bare unop NOT
    // path still works for non-EXISTS operands.
    let q = parse_query("MATCH (a) WHERE NOT (a.flag = true) RETURN a.name").unwrap();
    let pat = q.matches[0].pattern();
    let PathPattern::Filter(_, expr) = pat else {
        panic!("expected filter, got {pat:?}");
    };
    assert!(
        matches!(expr, Expr::Unop { op: UnOp::Not, .. }),
        "expected Unop(Not, ...), got {expr:?}"
    );
}
