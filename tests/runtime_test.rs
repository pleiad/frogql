use std::path::Path;

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::syntax::descriptor::Descriptor;
use gqlrust::syntax::expr::{BinOp, Expr, UnOp};
use gqlrust::syntax::path_pattern::PathPattern;
use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::simple_type::SimpleType;

fn fraud_graph() -> MemoryGraphStore {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    MemoryGraphStore::from_file(&p).unwrap()
}

fn social_graph() -> MemoryGraphStore {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/social-network.json");
    MemoryGraphStore::from_file(&p).unwrap()
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
    assert_eq!(r.run(&PathPattern::EdgeAnyDirection(None)).rows.len(), 10);
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

// ==================== Comma-join (Q1, Q2) tests ====================

/// Test: (x) -[]-> (), (:Car) -[]-> (x) -[]-> ()
/// From the user's example. On fraud graph, (:Car) matches nothing, so result is empty.
#[test]
fn test_join_no_match() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = gqlrust::compile("(x) -[]-> (), (:Car) -[]-> (x) -[]-> ()").unwrap();
    assert_eq!(r.run(&p).rows.len(), 0);
}

/// Test basic join without shared variables: cross-product.
/// (x: Dummy), (y: Account) — should produce 1 × 4 = 4 results (1 Dummy, 4 Accounts).
/// Wait: d1 has labels Dummy & Person. All nodes have Account except d1.
/// Actually: a1, a2, p1, p2 are Account (4 nodes). d1 is Dummy & Person.
/// So (:Dummy) = {d1}, (:Account) = {a1, a2, p1, p2} → 1 * 4 = 4.
#[test]
fn test_join_cross_product() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = gqlrust::compile("(:Dummy), (:Account)").unwrap();
    assert_eq!(r.run(&p).rows.len(), 4);
}

/// Test join with shared variable: unification filters results.
/// (x) -[]-> (y), (y) -[]-> (x) means: find pairs where x→y AND y→x
/// In the fraud graph: t4: a1→p1, t1: p1→p2 (no), t3: a2→a1, t4: a1→p1
/// Cycle edges: p1→p2→a2→a1→p1. So x→y AND y→x pairs:
/// (a1,p1) via t4 and (p1,a1) — but p1→a1 doesn't exist. p1→p2 only.
/// Actually let me trace: edges are t1(p1→p2), t2(p2→a2), t3(a2→a1), t4(a1→p1), t5(a1→d1).
/// x→y AND y→x: need (x,y) and (y,x) both as edges.
/// (a1,p1) via t4 and (p1,a1)? No edge p1→a1.
/// None of these form a mutual pair. So result should be 0.
#[test]
fn test_join_shared_var_no_mutual() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = gqlrust::compile("(x) -[]-> (y), (y) -[]-> (x)").unwrap();
    assert_eq!(r.run(&p).rows.len(), 0);
}

/// Test join with shared variable that unifies.
/// (x) -[:Transfer]-> (y), (x) -[:Transfer]-> (z)
/// This is a star pattern: x has two outgoing Transfer edges.
/// In fraud graph: a1 has t4(→p1) and t5(→d1, but Foo label). a2 has t3(→a1).
/// t5 has label Foo, not Transfer. So only nodes with 1 outgoing Transfer:
/// p1(→p2), p2(→a2), a2(→a1), a1(→p1). Each has exactly 1 Transfer edge.
/// So no node has 2 outgoing Transfer edges.
/// But (x) -[:Transfer]-> (y), (x) -[:Transfer]-> (z) allows y==z.
/// So each x with 1 outgoing Transfer yields 1 result (y=z=same target).
/// That's 4 results (p1,p2,a2,a1 each have 1 Transfer out).
#[test]
fn test_join_star_pattern() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = gqlrust::compile("(x) -[:Transfer]-> (y), (x) -[:Transfer]-> (z)").unwrap();
    assert_eq!(r.run(&p).rows.len(), 4);
}

// ==================== MATCH/WHERE/RETURN runtime tests ====================

#[test]
fn test_query_match_where_return() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query(
        "MATCH (x: Account) -[:Transfer]-> (y) WHERE x.owner = 'Jay' RETURN y.owner",
    )
    .unwrap();
    let result = r.run_query(&q, 0);
    // Jay (p1) has one Transfer out: t1 → p2 (Mike)
    assert_eq!(result.row_count(), 1);
    match result {
        gqlrust::runtime::result::QueryResult::Projected(rows) => {
            assert_eq!(rows[0][0], Value::Str("Mike".into()));
        }
        _ => panic!("expected Projected"),
    }
}

#[test]
fn test_query_return_distinct() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    // All outgoing Transfer targets — some nodes might appear multiple times
    let q = gqlrust::compile_query("MATCH (x) -[:Transfer]-> (y) RETURN DISTINCT y.owner").unwrap();
    let result = r.run_query(&q, 0);
    match result {
        gqlrust::runtime::result::QueryResult::Projected(rows) => {
            // Transfers: p1→p2(Mike), p2→a2(Scott), a2→a1(Aretha), a1→p1(Jay)
            // Distinct owners: Mike, Scott, Aretha, Jay = 4
            assert_eq!(rows.len(), 4);
        }
        _ => panic!("expected Projected"),
    }
}

#[test]
fn test_query_no_return() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH (x: Account)").unwrap();
    let result = r.run_query(&q, 0);
    match result {
        gqlrust::runtime::result::QueryResult::Raw(ir) => {
            assert_eq!(ir.rows.len(), 4); // 4 Account nodes
        }
        _ => panic!("expected Raw"),
    }
}

// ==================== Repetition grouping tests ====================

#[test]
fn test_repetition_groups_as_list() {
    // -[x]->{1,2} should bind x to a List of edges
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = gqlrust::compile("-[x]->{1,2}").unwrap();
    let result = r.run(&p);
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        let x = row.assignment.get("x").expect("x should be bound");
        assert!(
            matches!(x, gqlrust::model::value::PathValue::Group(_)),
            "x should be a List, got {:?}",
            x
        );
    }
}

#[test]
fn test_repetition_list_length() {
    // -[x]->{2,2} should bind x to a List of exactly 2 edges
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = gqlrust::compile("-[x]->{2,2}").unwrap();
    let result = r.run(&p);
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        match row.assignment.get("x").unwrap() {
            gqlrust::model::value::PathValue::Group(l) => assert_eq!(l.len(), 2),
            other => panic!("expected List of len 2, got {:?}", other),
        }
    }
}

#[test]
fn test_repetition_nested_list() {
    // (-[x]->{1,2}){1,2} should bind x to a List of Lists
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = gqlrust::compile("(-[x]->{1,2}){1,2}").unwrap();
    let result = r.run(&p);
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        match row.assignment.get("x").unwrap() {
            gqlrust::model::value::PathValue::Group(outer) => {
                for item in outer {
                    assert!(
                        matches!(item, gqlrust::model::value::PathValue::Group(_)),
                        "inner items should be Lists, got {:?}",
                        item
                    );
                }
            }
            other => panic!("expected List of Lists, got {:?}", other),
        }
    }
}

#[test]
fn test_repetition_zero_gives_empty_list() {
    // -[x]->{0,1} with 0 repetitions should give x = []
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = gqlrust::compile("-[x]->{0,1}").unwrap();
    let result = r.run(&p);
    // Some rows should have x = [] (the 0-repetition case)
    let empty_list_rows: Vec<_> = result.rows.iter()
        .filter(|row| matches!(row.assignment.get("x"), Some(gqlrust::model::value::PathValue::Group(l)) if l.is_empty()))
        .collect();
    assert!(!empty_list_rows.is_empty(), "should have rows with x = []");
}

/// Test join on the full "all edges" pattern.
/// (x) -[]-> (y), (x) -[]-> (z) — star with any label.
/// a1 has 2 outgoing edges (t4→p1, t5→d1), so it produces 2×2=4 combos.
/// p1, p2, a2 each have 1 outgoing, so 1×1=1 each.
/// d1 has 0 outgoing.
/// Total: 4 + 1 + 1 + 1 = 7.
#[test]
fn test_join_star_any_label() {
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let p = gqlrust::compile("(x) -[]-> (y), (x) -[]-> (z)").unwrap();
    assert_eq!(r.run(&p).rows.len(), 7);
}

// ==================== LIMIT runtime tests ============================

#[test]
fn test_query_limit_caps_rows() {
    // Without LIMIT, this returns 4 rows (one per outgoing Transfer);
    // LIMIT 2 in the query should cap that to 2.
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH (x) -[:Transfer]-> (y) RETURN y.owner LIMIT 2").unwrap();
    let result = r.run_query(&q, 0);
    assert_eq!(result.row_count(), 2);
}

#[test]
fn test_query_limit_smaller_than_results_no_op() {
    // LIMIT bigger than the natural result size has no effect.
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let q =
        gqlrust::compile_query("MATCH (x) -[:Transfer]-> (y) RETURN y.owner LIMIT 1000").unwrap();
    let result = r.run_query(&q, 0);
    assert_eq!(result.row_count(), 4);
}

#[test]
fn test_query_limit_min_with_runtime_cap() {
    // When both an in-query LIMIT and a runtime cap are set, the smaller wins.
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH (x) -[:Transfer]-> (y) RETURN y.owner LIMIT 3").unwrap();
    // runtime cap=1 is stricter than query LIMIT 3 → expect 1 row
    let result = r.run_query(&q, 1);
    assert_eq!(result.row_count(), 1);
}

#[test]
fn test_query_limit_no_return_caps_raw() {
    // LIMIT without RETURN produces a Raw result truncated to LIMIT
    // rows. fraud_graph has 4 Account nodes; LIMIT 2 caps to 2.
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query_unchecked("MATCH (x: Account) LIMIT 2").unwrap();
    assert_eq!(q.limit, Some(2));
    let result = r.run_query(&q, 0);
    match result {
        gqlrust::runtime::result::QueryResult::Raw(ir) => {
            assert_eq!(ir.rows.len(), 2);
        }
        _ => panic!("expected Raw"),
    }
}

#[test]
fn test_query_limit_zero_returns_empty_projected() {
    // ISO/IEC 39075:2024: `LIMIT 0` is valid and returns an empty
    // binding table. Without the short-circuit in run_query, the
    // runtime's `0 = unbounded` convention would silently swallow the
    // cap and return all rows — this test guards against that bug.
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH (x) -[:Transfer]-> (y) RETURN y.owner LIMIT 0").unwrap();
    let result = r.run_query(&q, 0);
    assert_eq!(result.row_count(), 0);
    match result {
        gqlrust::runtime::result::QueryResult::Projected(rows) => {
            assert!(rows.is_empty());
        }
        _ => panic!("expected Projected (LIMIT 0 with RETURN)"),
    }
}

#[test]
fn test_query_limit_zero_returns_empty_raw() {
    // Same guard for the no-RETURN path: LIMIT 0 must produce an empty
    // Raw result, not the unbounded "all 4 Accounts" the runtime would
    // produce if `0 = unbounded` leaked through.
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query_unchecked("MATCH (x: Account) LIMIT 0").unwrap();
    let result = r.run_query(&q, 0);
    match result {
        gqlrust::runtime::result::QueryResult::Raw(ir) => {
            assert_eq!(ir.rows.len(), 0);
        }
        _ => panic!("expected Raw"),
    }
}

#[test]
fn test_query_limit_zero_overrides_runtime_cap() {
    // If the user wrote `LIMIT 0` in the query, no caller-supplied
    // runtime cap should override it — the user explicitly asked for
    // zero rows. Pass a runtime cap of 100 (which would normally
    // return 4 rows from this query) and verify we still get 0.
    let g = fraud_graph();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH (x) -[:Transfer]-> (y) RETURN y.owner LIMIT 0").unwrap();
    let result = r.run_query(&q, 100);
    assert_eq!(result.row_count(), 0);
}

/// DISTINCT + a row cap must dedup before truncating, not after.
/// Without the fix, `input_limit = limit` truncates the scan to 2 duplicate
/// Alices before dedup, yielding 1 row instead of 2.
#[test]
fn test_distinct_limit_does_not_truncate_input_before_dedup() {
    let g = MemoryGraphStore::from_json_str(
        r#"{
            "nodes": [
                {"id": "a", "labels": ["Person"], "props": {"name": "Alice"}},
                {"id": "b", "labels": ["Person"], "props": {"name": "Alice"}},
                {"id": "c", "labels": ["Person"], "props": {"name": "Bob"}}
            ],
            "edges": []
        }"#,
    )
    .unwrap();
    let r = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH (x: Person) RETURN DISTINCT x.name").unwrap();
    let result = r.run_query(&q, 2);
    assert_eq!(result.row_count(), 2);
}
