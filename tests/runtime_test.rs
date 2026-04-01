use std::path::Path;

use gqlrust::model::graph::Graph;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::syntax::descriptor::Descriptor;
use gqlrust::syntax::expr::{BinOp, Expr, UnOp};
use gqlrust::syntax::path_pattern::PathPattern;
use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::simple_type::SimpleType;

fn fraud_graph() -> Graph {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    Graph::from_file(&p).unwrap()
}

fn social_graph() -> Graph {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/social-network.json");
    Graph::from_file(&p).unwrap()
}

/// Helper: descriptor with just a label
fn label_desc(label: &str) -> Option<Descriptor> {
    Some(Descriptor::type_only(DescriptorType::new(
        LabelType::Label(label.into()),
        PropertyType::open_empty(),
    )))
}

/// Helper: descriptor with a variable and a label
fn var_label_desc(var: &str, label: &str) -> Option<Descriptor> {
    Some(Descriptor::new(
        Some(var.into()),
        DescriptorType::new(LabelType::Label(label.into()), PropertyType::open_empty()),
    ))
}

/// Helper: descriptor with just a variable (no type constraint)
fn var_desc(var: &str) -> Option<Descriptor> {
    Some(Descriptor::var_only(var))
}

/// Helper: descriptor with a variable and an open property type ({...})
fn var_open_prop_desc(var: &str, props: &[(&str, SimpleType)]) -> Option<Descriptor> {
    let mut pt = PropertyType::open_empty();
    for (k, t) in props {
        pt.extend(k.to_string(), t.clone());
    }
    Some(Descriptor::new(
        Some(var.into()),
        DescriptorType::new(LabelType::Star, pt),
    ))
}

// ==================== Python runtime_test.py ports ====================

// test_node_empty: ()
#[test]
fn test_node_empty() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    assert_eq!(r.run(&PathPattern::Node(None)).rows.len(), 5);
}

// test_node_capturing: (x)
#[test]
fn test_node_capturing() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    assert_eq!(r.run(&PathPattern::Node(var_desc("x"))).rows.len(), 5);
}

// test_node_filter_by_label: (x: Account)
#[test]
fn test_node_filter_by_label() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    assert_eq!(
        r.run(&PathPattern::Node(var_label_desc("x", "Account")))
            .rows
            .len(),
        4
    );
}

// test_edge_empty: -[]->
#[test]
fn test_edge_empty() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    assert_eq!(r.run(&PathPattern::EdgeRight(None)).rows.len(), 5);
}

// test_edge_nondirectional: ~~ (fraud has no undirected edges)
#[test]
fn test_edge_nondirectional() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    assert_eq!(r.run(&PathPattern::EdgeUndirected(None)).rows.len(), 0);
}

// test_edge_filter_by_label: -[x: Transfer]->
#[test]
fn test_edge_filter_by_label() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    assert_eq!(
        r.run(&PathPattern::EdgeRight(var_label_desc("x", "Transfer")))
            .rows
            .len(),
        4
    );
}

// test_concat: ()-[]->
#[test]
fn test_concat() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Concat(
        Box::new(PathPattern::Node(None)),
        Box::new(PathPattern::EdgeRight(None)),
    );
    assert_eq!(r.run(&p).rows.len(), 5);
}

// test_concat_label: (x)-[:Foo]->
#[test]
fn test_concat_label() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Concat(
        Box::new(PathPattern::Node(var_desc("x"))),
        Box::new(PathPattern::EdgeRight(label_desc("Foo"))),
    );
    assert_eq!(r.run(&p).rows.len(), 1);
}

// test_size_2: ()-[]->()-[]->()
#[test]
fn test_size_2() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    // ()-[]->()-[]->() parses as Concat(Concat(Concat(Concat(Node, EdgeRight), Node), EdgeRight), Node)
    let p2 = PathPattern::Concat(
        Box::new(PathPattern::Concat(
            Box::new(PathPattern::Concat(
                Box::new(PathPattern::Concat(
                    Box::new(PathPattern::Node(None)),
                    Box::new(PathPattern::EdgeRight(None)),
                )),
                Box::new(PathPattern::Node(None)),
            )),
            Box::new(PathPattern::EdgeRight(None)),
        )),
        Box::new(PathPattern::Node(None)),
    );
    assert_eq!(r.run(&p2).rows.len(), 5);
}

// test_selector: (x WHERE x.isDummy is bool)
#[test]
fn test_selector() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Filter(
        Box::new(PathPattern::Node(var_desc("x"))),
        Expr::Binop {
            op: BinOp::Is,
            left: Box::new(Expr::AttrLookup {
                var: "x".into(),
                attr: "isDummy".into(),
            }),
            right: Box::new(Expr::Type(SimpleType::B)),
        },
    );
    assert_eq!(r.run(&p).rows.len(), 1);
}

// test_concat_selector: ()(x:{isDummy: bool})
#[test]
fn test_concat_selector() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Concat(
        Box::new(PathPattern::Node(None)),
        Box::new(PathPattern::Node(var_open_prop_desc(
            "x",
            &[("isDummy", SimpleType::B)],
        ))),
    );
    assert_eq!(r.run(&p).rows.len(), 1);
}

// test_union: (x: Dummy) | (y: Account)
#[test]
fn test_union() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Union(
        Box::new(PathPattern::Node(var_label_desc("x", "Dummy"))),
        Box::new(PathPattern::Node(var_label_desc("y", "Account"))),
    );
    assert_eq!(r.run(&p).rows.len(), 5);
}

// test_filter_1: (y WHERE y.isBlocked=true), etc.
#[test]
fn test_filter_1() {
    let g = fraud_graph();
    let r = Runtime::new(&g);

    let make_filter = |val: Value| {
        PathPattern::Filter(
            Box::new(PathPattern::Node(var_desc("y"))),
            Expr::Binop {
                op: BinOp::Eq,
                left: Box::new(Expr::AttrLookup {
                    var: "y".into(),
                    attr: "isBlocked".into(),
                }),
                right: Box::new(Expr::Const(val)),
            },
        )
    };

    assert_eq!(r.run(&make_filter(Value::Bool(true))).rows.len(), 1);
    assert_eq!(r.run(&make_filter(Value::Bool(false))).rows.len(), 4);
    assert_eq!(r.run(&make_filter(Value::Int(1))).rows.len(), 0);
}

// test_filter_2: -[y WHERE y.amount>=3500000 and y.amount>1]->
#[test]
fn test_filter_2() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Filter(
        Box::new(PathPattern::EdgeRight(var_desc("y"))),
        Expr::Binop {
            op: BinOp::And,
            left: Box::new(Expr::Binop {
                op: BinOp::Ge,
                left: Box::new(Expr::AttrLookup {
                    var: "y".into(),
                    attr: "amount".into(),
                }),
                right: Box::new(Expr::Const(Value::Int(3500000))),
            }),
            right: Box::new(Expr::Binop {
                op: BinOp::Gt,
                left: Box::new(Expr::AttrLookup {
                    var: "y".into(),
                    attr: "amount".into(),
                }),
                right: Box::new(Expr::Const(Value::Int(1))),
            }),
        },
    );
    assert_eq!(r.run(&p).rows.len(), 1);
}

// test_filter_4: -[y WHERE y.bambino > 0]-> (missing attribute → failure → filtered out)
#[test]
fn test_filter_4() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Filter(
        Box::new(PathPattern::EdgeRight(var_desc("y"))),
        Expr::Binop {
            op: BinOp::Gt,
            left: Box::new(Expr::AttrLookup {
                var: "y".into(),
                attr: "bambino".into(),
            }),
            right: Box::new(Expr::Const(Value::Int(0))),
        },
    );
    assert_eq!(r.run(&p).rows.len(), 0);
}

// test_union_fail: (x: NoExists) | (x: NoExists)
#[test]
fn test_union_fail() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Union(
        Box::new(PathPattern::Node(var_label_desc("x", "NoExists"))),
        Box::new(PathPattern::Node(var_label_desc("x", "NoExists"))),
    );
    assert_eq!(r.run(&p).rows.len(), 0);
}

// test_concat_any_right: - (any direction edge)
#[test]
fn test_concat_any_right() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    // "-" = EdgeAnyDirection(None)
    assert_eq!(
        r.run(&PathPattern::EdgeAnyDirection(None)).rows.len(),
        10
    );
}

// test_repetition: -->{1,2} parses as Concat(EdgeAnyDirection, Repeat(EdgeRight, 1, 2))
#[test]
fn test_repetition() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    // "-->{1,2}" = "--" concat "(-->){1,2}"
    let p = PathPattern::Concat(
        Box::new(PathPattern::EdgeAnyDirection(None)),
        Box::new(PathPattern::Repeat {
            pattern: Box::new(PathPattern::EdgeRight(None)),
            lb: 1,
            ub: Some(2),
        }),
    );
    assert_eq!(r.run(&p).rows.len(), 23);
}

// test_repetition_descriptor: -[x]->{2,3}
#[test]
fn test_repetition_descriptor() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Repeat {
        pattern: Box::new(PathPattern::EdgeRight(var_desc("x"))),
        lb: 2,
        ub: Some(3),
    };
    assert_eq!(r.run(&p).rows.len(), 10);
}

// test_repetition_repetition: (-[x]->{1,2}){2,3}
#[test]
fn test_repetition_repetition() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let inner = PathPattern::Repeat {
        pattern: Box::new(PathPattern::EdgeRight(var_desc("x"))),
        lb: 1,
        ub: Some(2),
    };
    let p = PathPattern::Repeat {
        pattern: Box::new(inner),
        lb: 2,
        ub: Some(3),
    };
    assert_eq!(r.run(&p).rows.len(), 60);
}

// test_digest_p4: (x) -[z:Transfer WHERE z.amount>1000000]-> (y WHERE y.isBlocked=true)
#[test]
fn test_digest_p4() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Concat(
        Box::new(PathPattern::Node(var_desc("x"))),
        Box::new(PathPattern::Concat(
            Box::new(PathPattern::Filter(
                Box::new(PathPattern::EdgeRight(var_label_desc("z", "Transfer"))),
                Expr::Binop {
                    op: BinOp::Gt,
                    left: Box::new(Expr::AttrLookup {
                        var: "z".into(),
                        attr: "amount".into(),
                    }),
                    right: Box::new(Expr::Const(Value::Int(1000000))),
                },
            )),
            Box::new(PathPattern::Filter(
                Box::new(PathPattern::Node(var_desc("y"))),
                Expr::Binop {
                    op: BinOp::Eq,
                    left: Box::new(Expr::AttrLookup {
                        var: "y".into(),
                        attr: "isBlocked".into(),
                    }),
                    right: Box::new(Expr::Const(Value::Bool(true))),
                },
            )),
        )),
    );
    assert_eq!(r.run(&p).rows.len(), 1);
}

// test_is: (x WHERE x.isBlocked is bool), (x WHERE x.isBlocked is str)
#[test]
fn test_is() {
    let g = fraud_graph();
    let r = Runtime::new(&g);

    let make_is = |ty: SimpleType| {
        PathPattern::Filter(
            Box::new(PathPattern::Node(var_desc("x"))),
            Expr::Binop {
                op: BinOp::Is,
                left: Box::new(Expr::AttrLookup {
                    var: "x".into(),
                    attr: "isBlocked".into(),
                }),
                right: Box::new(Expr::Type(ty)),
            },
        )
    };

    assert_eq!(r.run(&make_is(SimpleType::B)).rows.len(), 5);
    assert_eq!(r.run(&make_is(SimpleType::S)).rows.len(), 0);
}

// test_as: (x WHERE x.isBlocked as bool), (x WHERE x.isBlocked as int > 0)
#[test]
fn test_as() {
    let g = fraud_graph();
    let r = Runtime::new(&g);

    // (x WHERE x.isBlocked as bool) → only the one with true
    let p1 = PathPattern::Filter(
        Box::new(PathPattern::Node(var_desc("x"))),
        Expr::Binop {
            op: BinOp::As,
            left: Box::new(Expr::AttrLookup {
                var: "x".into(),
                attr: "isBlocked".into(),
            }),
            right: Box::new(Expr::Type(SimpleType::B)),
        },
    );
    assert_eq!(r.run(&p1).rows.len(), 1);

    // (x WHERE x.isBlocked as int > 0) → 0 (cast fails)
    let p2 = PathPattern::Filter(
        Box::new(PathPattern::Node(var_desc("x"))),
        Expr::Binop {
            op: BinOp::Gt,
            left: Box::new(Expr::Binop {
                op: BinOp::As,
                left: Box::new(Expr::AttrLookup {
                    var: "x".into(),
                    attr: "isBlocked".into(),
                }),
                right: Box::new(Expr::Type(SimpleType::Z)),
            }),
            right: Box::new(Expr::Const(Value::Int(0))),
        },
    );
    assert_eq!(r.run(&p2).rows.len(), 0);
}

// test_where (social network): (x: {status: bool})
#[test]
fn test_where_closed_prop() {
    let g = social_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Node(var_open_prop_desc("x", &[("status", SimpleType::B)]));
    assert_eq!(r.run(&p).rows.len(), 1);
}

// test_unop_1: (x WHERE not x.isBlocked)
#[test]
fn test_unop_not() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Filter(
        Box::new(PathPattern::Node(var_desc("x"))),
        Expr::Unop {
            op: UnOp::Not,
            operand: Box::new(Expr::AttrLookup {
                var: "x".into(),
                attr: "isBlocked".into(),
            }),
        },
    );
    assert_eq!(r.run(&p).rows.len(), 4);
}

// test_unop_2: -[x WHERE -x.amount < 0]->
#[test]
fn test_unop_neg() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = PathPattern::Filter(
        Box::new(PathPattern::EdgeRight(var_desc("x"))),
        Expr::Binop {
            op: BinOp::Lt,
            left: Box::new(Expr::Unop {
                op: UnOp::Neg,
                operand: Box::new(Expr::AttrLookup {
                    var: "x".into(),
                    attr: "amount".into(),
                }),
            }),
            right: Box::new(Expr::Const(Value::Int(0))),
        },
    );
    assert_eq!(r.run(&p).rows.len(), 5);
}
