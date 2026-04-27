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

/// Pin the structural invariant of the multi-MATCH refactor (ISO §14.3-14.4):
/// every parsed Query has exactly one match statement, and it is a `Simple`
/// holding the parsed pattern. Without this, `Query::collapsed_pattern()`
/// would silently panic on `vec![]` instead of failing in tests.
#[test]
fn test_query_has_one_simple_match_statement() {
    let q = gqlrust::compile_query("MATCH (x)-[:Knows]->(y) RETURN x.name").unwrap();
    assert_eq!(q.matches.len(), 1, "parser must produce exactly one match");
    assert!(
        matches!(&q.matches[0], MatchStatement::Simple { .. }),
        "current parser only emits Simple match statements (Optional comes in a later PR)"
    );
}

#[test]
fn test_match_where_return() {
    let q = gqlrust::compile_query(
        "MATCH (x) -[:Transfer]-> (y) WHERE x.amount > 100 RETURN x.name, y.name",
    )
    .unwrap();
    assert!(matches!(q.collapsed_pattern(), PathPattern::Filter(_, _)));
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
        let MatchStatement::Simple { pattern } = m;
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
