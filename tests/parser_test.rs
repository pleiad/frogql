use gqlrust::model::value::Value;
use gqlrust::parser::parse;
use gqlrust::syntax::descriptor::Descriptor;
use gqlrust::syntax::expr::{BinOp, Expr, UnOp};
use gqlrust::syntax::path_pattern::PathPattern;
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
        PathPattern::Node(Some(Descriptor::new(
            Some("x".into()),
            label_dt("Person"),
        )))
    );
}

#[test]
fn test_descriptor_empty_record() {
    assert_eq!(
        parse("(x:Person {})").unwrap(),
        PathPattern::Node(Some(Descriptor::new(
            Some("x".into()),
            label_dt("Person"),
        )))
    );
}

#[test]
fn test_descriptor_record() {
    let mut pt = PropertyType::open_empty();
    pt.extend("a".into(), SimpleType::Z);
    assert_eq!(
        parse("(x :Person {a: int})").unwrap(),
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
        parse("(:Person {a: int, b: bool})").unwrap(),
        PathPattern::Node(Some(Descriptor::new(
            None,
            DescriptorType::new(LabelType::Label("Person".into()), pt),
        )))
    );
}

#[test]
fn test_descriptor_no_label() {
    let mut pt = PropertyType::open_empty();
    pt.extend("a".into(), SimpleType::Z);
    pt.extend("b".into(), SimpleType::B);
    assert_eq!(
        parse("(:{a: int, b: bool})").unwrap(),
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
        parse("-[x:Person {a: int}]->").unwrap(),
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
        parse("<-[x:Person {a: int}]-").unwrap(),
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
        parse("~[x:Person {a: int}]~").unwrap(),
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
        parse("(x :Person {{a: int}})").unwrap(),
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
