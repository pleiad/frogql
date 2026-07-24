//! Abstraction-soundness suite for `PathSummary` (the checker's
//! evaluation representation) against the inductive `PathType` (the
//! paper's spec type, `rules.md` §4/§8).
//!
//! The claim pinned here is that `summarize : PathType → PathSummary` is a
//! lattice homomorphism on the checker-reachable domain:
//!
//!   summarize(meet_S(p, q))  =  meet_S(summarize(p), summarize(q))
//!   summarize(union(p, q))   =  union(summarize(p), summarize(q))
//!
//! and that the judgments the checker consumes factor through it:
//! `is_unsatisfiable` agrees everywhere on the domain, and
//! `is_empty` / `len` agree on satisfiable values (the spec type's length
//! functions are satisfiability-blind on dead arms — e.g.
//! `Edge{p1: Zero, ..}` reports len 1 — but the checker never holds such a
//! value: meets prune them and constructors cannot produce them).
//!
//! Domain restriction: descriptors are generated non-empty (no
//! `PropertyType::Zero`), matching the checker-reachable invariant
//! documented in `path_summary.rs`.

use proptest::prelude::*;
use std::collections::BTreeMap;

use frogql::typing::descriptor_type::DescriptorType;
use frogql::typing::label_type::LabelType;
use frogql::typing::path_summary::PathSummary;
use frogql::typing::path_type::{EdgePathType, NodePathType, PathType};
use frogql::typing::property_type::PropertyType;
use frogql::typing::simple_type::SimpleType;
use frogql::typing::variable_type::{Schema, VariableType};

fn arb_label() -> impl Strategy<Value = LabelType> {
    prop_oneof![
        Just(LabelType::Star),
        Just(LabelType::Label("A".into())),
        Just(LabelType::Label("B".into())),
        Just(LabelType::Label("C".into())),
    ]
}

fn arb_props() -> impl Strategy<Value = PropertyType> {
    let kv = prop_oneof![
        Just(("k1".to_string(), SimpleType::Z)),
        Just(("k2".to_string(), SimpleType::S)),
        Just(("k3".to_string(), SimpleType::B)),
    ];
    proptest::collection::vec(kv, 0..3)
        .prop_map(|kvs| PropertyType::Open(kvs.into_iter().collect::<BTreeMap<_, _>>()))
}

fn arb_desc() -> impl Strategy<Value = DescriptorType> {
    (arb_label(), arb_props()).prop_map(|(l, p)| DescriptorType::new(l, p))
}

// Domain-faithful generator: the checker only ever builds unions through
// the `PathType::union` smart constructor (which drops `Zero` arms and
// collapses exact duplicates) — `from_variable`, `meet`'s distribution,
// and `union_from_list` all route through it. Raw `Union(_, Zero)` trees
// are unreachable and sit outside the abstraction's domain (the spec
// `len`/`is_empty` are satisfiability-blind on such dead arms), so the
// generator composes unions the same way the checker does.
fn arb_path() -> impl Strategy<Value = PathType> {
    let leaf = prop_oneof![
        2 => arb_desc().prop_map(PathType::node),
        1 => Just(PathType::Zero),
    ];
    leaf.prop_recursive(4, 24, 3, |inner| {
        prop_oneof![
            (inner.clone(), arb_desc()).prop_map(|(p1, d)| PathType::Edge(EdgePathType {
                p1: Box::new(p1),
                n2: NodePathType::new(d),
            })),
            (inner.clone(), inner).prop_map(|(a, b)| PathType::union(a, b)),
        ]
    })
}

/// A small non-trivial schema: three labeled node types with props, and
/// directed A→B, B→C plus an undirected B~B edge entry.
fn labeled_schema() -> Schema {
    let node = |l: &str, k: &str, t: SimpleType| {
        VariableType::Node(DescriptorType::new(
            LabelType::Label(l.into()),
            PropertyType::Open([(k.to_string(), t)].into_iter().collect()),
        ))
    };
    let a = node("A", "k1", SimpleType::Z);
    let b = node("B", "k2", SimpleType::S);
    let c = node("C", "k3", SimpleType::B);
    let edge = |desc_label: &str, l: &VariableType, r: &VariableType, directed: bool| {
        let desc = DescriptorType::new(
            LabelType::Label(desc_label.into()),
            PropertyType::open_empty(),
        );
        if directed {
            VariableType::EdgeDirectional {
                desc,
                left: Box::new(l.clone()),
                right: Box::new(r.clone()),
            }
        } else {
            VariableType::EdgeNonDirectional {
                desc,
                left: Box::new(l.clone()),
                right: Box::new(r.clone()),
            }
        }
    };
    let edges = vec![
        edge("ab", &a, &b, true),
        edge("bc", &b, &c, true),
        edge("bb", &b, &b, false),
    ];
    Schema::from_parts(vec![a, b, c], edges)
}

/// Satisfiability-aware length on the spec type: minimum edge count over
/// *live* (satisfiable) arms, `None` when no arm survives. This is the
/// reference for `PathSummary::len`/`is_empty` — the spec type's own
/// `len()`/`is_empty()` are satisfiability-blind (they count dead arms
/// like `Edge{p1: Zero, ..}`), which made the old repeat-length warning
/// inconsistent: whether it fired for a dead inner depended on how the
/// inner died. The summary implements the live semantics uniformly.
fn live_len(p: &PathType) -> Option<usize> {
    match p {
        PathType::Zero => None,
        PathType::Node(n) => {
            if n.desc.is_empty() {
                None
            } else {
                Some(0)
            }
        }
        PathType::Edge(e) => {
            if e.n2.desc.is_empty() {
                None
            } else {
                live_len(&e.p1).map(|l| l + 1)
            }
        }
        PathType::Union(a, b) => match (live_len(a), live_len(b)) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (x, None) => x,
            (None, y) => y,
        },
    }
}

fn hom_checks(schema: &Schema, p: &PathType, q: &PathType) -> Result<(), TestCaseError> {
    let sp = PathSummary::summarize(p);
    let sq = PathSummary::summarize(q);

    // Homomorphism: meet and union commute with summarize.
    let spec_meet = PathSummary::summarize(&PathType::meet(schema, p, q));
    let eval_meet = PathSummary::meet(schema, &sp, &sq);
    prop_assert_eq!(
        spec_meet,
        eval_meet,
        "meet hom failed\n p={:?}\n q={:?}",
        p,
        q
    );

    let spec_union = PathSummary::summarize(&PathType::union(p.clone(), q.clone()));
    let eval_union = PathSummary::union(sp.clone(), sq.clone());
    prop_assert_eq!(
        spec_union,
        eval_union,
        "union hom failed\n p={:?}\n q={:?}",
        p,
        q
    );

    // Judgment agreement against the live reference.
    let live = live_len(p);
    prop_assert_eq!(
        p.is_unsatisfiable(),
        live.is_none(),
        "spec unsat vs live: {:?}",
        p
    );
    prop_assert_eq!(
        sp.is_unsatisfiable(),
        live.is_none(),
        "summary unsat: {:?}",
        p
    );
    prop_assert_eq!(sp.len(), live.unwrap_or(0), "summary len vs live: {:?}", p);
    prop_assert_eq!(
        sp.is_empty(),
        live.map_or(true, |l| l == 0),
        "summary is_empty vs live: {:?}",
        p
    );
    // On fully-live paths the blind and live semantics coincide, so the
    // spec's own judgments agree too.
    if live.is_some() && p.len() == live.unwrap() {
        prop_assert_eq!(
            p.is_empty(),
            sp.is_empty(),
            "is_empty on live path: {:?}",
            p
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn hom_star_schema(p in arb_path(), q in arb_path()) {
        hom_checks(&Schema::star(), &p, &q)?;
    }

    #[test]
    fn hom_labeled_schema(p in arb_path(), q in arb_path()) {
        hom_checks(&labeled_schema(), &p, &q)?;
    }
}
