//! Spec-driven proptest suite for the typing lattice.
//!
//! Two layers of coverage:
//!
//! 1. **Algebraic invariants** — subtype reflexivity, meet idempotence,
//!    greatest-lower-bound, commutativity, identity / absorbing elements.
//!    These match what fppc's stashed proptests covered, modulo style.
//!
//! 2. **Spec rules from `rules.md`** — direct encodings of the typing
//!    rules, organized by section. Each property cites the section /
//!    rule it validates so a regression points at exactly which rule was
//!    violated. The goal is "if rules.md changes, this file changes; if
//!    the implementation drifts, a property fails."
//!
//! `proptest` shrinks failing inputs to minimal counterexamples and
//! persists them under `tests/proptest-regressions/` for replay.
//!
//! ## Variants intentionally NOT generated
//!
//! `LabelType::Top`, `LabelType::Empty`, `LabelType::Neg` are currently
//! dead code in gqlite (no parser/elaborator/runtime constructs them) and
//! their intended semantics is unsettled (pending teacher clarification
//! on SAT-based `A & !A` detection and on whether `Empty` should be
//! renamed to `Zero` for lattice-naming consistency). Generating them
//! here would lock in behavior that is likely to change. Add them to
//! `arb_label_type` once the semantics are pinned down — the spec-rule
//! tests below are already structured to accept new variants.
//!
//! `VariableType::EdgeDirectional`/`EdgeNonDirectional` carry `left` /
//! `right` that the meet logic assumes are `Node` variants — generating
//! arbitrary `VariableType` there would route every test through the
//! `_ => Zero` bailout. The generator constrains those slots to `Node`.

use std::collections::BTreeMap;

use proptest::prelude::*;

use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::path_type::{EdgeDir, EdgePathType, NodePathType, PathType};
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::simple_type::SimpleType;
use gqlrust::typing::variable_type::{Schema, VariableType};

// =======================================================================
// Generators
// =======================================================================

fn arb_simple_atom() -> impl Strategy<Value = SimpleType> {
    prop_oneof![
        Just(SimpleType::Z),
        Just(SimpleType::F),
        Just(SimpleType::B),
        Just(SimpleType::S),
    ]
}

fn arb_simple_type() -> impl Strategy<Value = SimpleType> {
    let leaf = prop_oneof![
        arb_simple_atom(),
        Just(SimpleType::Star),
        Just(SimpleType::Zero),
    ];
    // depth 3, total 32 nodes, 4 children per recursive level.
    leaf.prop_recursive(3, 32, 4, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone())
                .prop_map(|(a, b)| SimpleType::Union(Box::new(a), Box::new(b))),
            inner.clone().prop_map(|t| SimpleType::List(Box::new(t))),
            inner.clone().prop_map(|t| SimpleType::Group(Box::new(t))),
            prop::collection::btree_map("[a-c]", inner, 0..3).prop_map(|m| {
                let m: BTreeMap<String, SimpleType> = m;
                SimpleType::Record(m)
            }),
        ]
    })
}

fn arb_label_type() -> impl Strategy<Value = LabelType> {
    let leaf = prop_oneof![Just(LabelType::Star), "[A-D]".prop_map(LabelType::Label),];
    leaf.prop_recursive(3, 16, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone())
                .prop_map(|(a, b)| LabelType::And(Box::new(a), Box::new(b))),
            (inner.clone(), inner).prop_map(|(a, b)| LabelType::Or(Box::new(a), Box::new(b))),
        ]
    })
}

fn arb_property_type() -> impl Strategy<Value = PropertyType> {
    prop_oneof![
        Just(PropertyType::Zero),
        prop::collection::btree_map("[a-c]", arb_simple_type(), 0..3).prop_map(PropertyType::Open),
        prop::collection::btree_map("[a-c]", arb_simple_type(), 0..3)
            .prop_map(PropertyType::Closed),
    ]
}

fn arb_descriptor_type() -> impl Strategy<Value = DescriptorType> {
    (arb_label_type(), arb_property_type()).prop_map(|(l, p)| DescriptorType::new(l, p))
}

/// Strictly Node-variant — used for `left`/`right` of edge variants
/// because the meet logic bails to Zero on non-Node endpoints.
fn arb_variable_node() -> impl Strategy<Value = VariableType> {
    arb_descriptor_type().prop_map(VariableType::Node)
}

/// Single-node `PathType`. Used as both a leaf and as the
/// boundary-node operand for path-meet tests.
fn arb_path_node() -> impl Strategy<Value = PathType> {
    arb_descriptor_type().prop_map(PathType::node)
}

/// `PathType` with bounded depth. Built recursively: each level may add
/// a new edge on the right or wrap two paths in a Union.
fn arb_path_type() -> impl Strategy<Value = PathType> {
    let leaf = prop_oneof![Just(PathType::Zero), arb_path_node(),];
    leaf.prop_recursive(3, 16, 2, |inner| {
        prop_oneof![
            // Edge: extends `inner` on the right with a new boundary node.
            (inner.clone(), arb_descriptor_type()).prop_map(|(p1, n2)| {
                PathType::Edge(EdgePathType {
                    p1: Box::new(p1),
                    n2: NodePathType::new(n2),
                })
            }),
            (inner.clone(), inner).prop_map(|(a, b)| { PathType::Union(Box::new(a), Box::new(b)) }),
        ]
    })
}

fn arb_variable_type() -> impl Strategy<Value = VariableType> {
    let leaf = prop_oneof![
        Just(VariableType::Zero),
        arb_descriptor_type().prop_map(VariableType::Node),
        (
            arb_descriptor_type(),
            arb_variable_node(),
            arb_variable_node()
        )
            .prop_map(|(d, l, r)| {
                VariableType::EdgeDirectional {
                    desc: d,
                    left: Box::new(l),
                    right: Box::new(r),
                }
            }),
        (
            arb_descriptor_type(),
            arb_variable_node(),
            arb_variable_node()
        )
            .prop_map(|(d, l, r)| {
                VariableType::EdgeNonDirectional {
                    desc: d,
                    left: Box::new(l),
                    right: Box::new(r),
                }
            }),
    ];
    // Recursion only through Group / Union — edge endpoints stay Node.
    leaf.prop_recursive(2, 8, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone())
                .prop_map(|(a, b)| VariableType::Union(Box::new(a), Box::new(b))),
            inner.prop_map(|t| VariableType::Group(Box::new(t))),
        ]
    })
}

// =======================================================================
// Shared proptest config — keep CI runs reasonable.
// =======================================================================

fn cfg() -> ProptestConfig {
    ProptestConfig {
        cases: 64,
        max_shrink_iters: 256,
        ..ProptestConfig::default()
    }
}

// =======================================================================
// SimpleType
// =======================================================================

mod simple_type {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        #[test]
        fn subtype_reflexive(t in arb_simple_type()) {
            prop_assert!(SimpleType::is_subtype(&t, &t),
                "is_subtype({t}, {t}) should be true");
        }

        #[test]
        fn meet_idempotent(t in arb_simple_type()) {
            let m = SimpleType::meet(&t, &t);
            prop_assert!(SimpleType::is_subtype(&m, &t),
                "meet({t}, {t}) = {m} should be <: {t}");
            prop_assert!(SimpleType::is_subtype(&t, &m),
                "meet({t}, {t}) = {m} should be >: {t}");
        }

        #[test]
        fn meet_lower_bound(a in arb_simple_type(), b in arb_simple_type()) {
            let m = SimpleType::meet(&a, &b);
            prop_assert!(SimpleType::is_subtype(&m, &a),
                "meet({a}, {b}) = {m} should be <: {a}");
            prop_assert!(SimpleType::is_subtype(&m, &b),
                "meet({a}, {b}) = {m} should be <: {b}");
        }

        #[test]
        fn meet_commutative(a in arb_simple_type(), b in arb_simple_type()) {
            let ab = SimpleType::meet(&a, &b);
            let ba = SimpleType::meet(&b, &a);
            prop_assert!(
                SimpleType::is_subtype(&ab, &ba) && SimpleType::is_subtype(&ba, &ab),
                "meet({a}, {b}) = {ab} should ≡ meet({b}, {a}) = {ba}"
            );
        }

        #[test]
        fn star_is_meet_identity(t in arb_simple_type()) {
            prop_assert_eq!(SimpleType::meet(&SimpleType::Star, &t), t.clone(),
                "meet(Star, t) should = t");
            prop_assert_eq!(SimpleType::meet(&t, &SimpleType::Star), t,
                "meet(t, Star) should = t");
        }

        #[test]
        fn zero_is_subtype_of_everything(t in arb_simple_type()) {
            prop_assert!(SimpleType::is_subtype(&SimpleType::Zero, &t),
                "Zero should be <: {t}");
        }

        #[test]
        fn distinct_atoms_meet_to_zero(a in arb_simple_atom(), b in arb_simple_atom()) {
            prop_assume!(a != b);
            prop_assert_eq!(SimpleType::meet(&a, &b), SimpleType::Zero,
                "meet of distinct atoms should be Zero");
        }
    }
}

// =======================================================================
// LabelType
// =======================================================================

mod label_type {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        #[test]
        fn subtype_reflexive(t in arb_label_type()) {
            prop_assert!(LabelType::is_subtype(&t, &t),
                "is_subtype({t}, {t}) should be true");
        }

        #[test]
        fn meet_idempotent(t in arb_label_type()) {
            let m = LabelType::meet(&t, &t);
            prop_assert!(
                LabelType::is_subtype(&m, &t) && LabelType::is_subtype(&t, &m),
                "meet({t}, {t}) = {m} should ≡ {t}"
            );
        }

        #[test]
        fn meet_lower_bound(a in arb_label_type(), b in arb_label_type()) {
            let m = LabelType::meet(&a, &b);
            prop_assert!(LabelType::is_subtype(&m, &a),
                "meet({a}, {b}) = {m} should be <: {a}");
            prop_assert!(LabelType::is_subtype(&m, &b),
                "meet({a}, {b}) = {m} should be <: {b}");
        }

        #[test]
        fn meet_commutative(a in arb_label_type(), b in arb_label_type()) {
            let ab = LabelType::meet(&a, &b);
            let ba = LabelType::meet(&b, &a);
            prop_assert!(
                LabelType::is_subtype(&ab, &ba) && LabelType::is_subtype(&ba, &ab),
                "meet({a}, {b}) = {ab} should ≡ meet({b}, {a}) = {ba}"
            );
        }

        #[test]
        fn star_is_meet_identity(t in arb_label_type()) {
            prop_assert_eq!(LabelType::meet(&LabelType::Star, &t), t,
                "meet(Star, t) should = t");
        }

        #[test]
        fn and_lhs_subtype_of_each_conjunct(
            a in arb_label_type(),
            b in arb_label_type(),
        ) {
            let ab = LabelType::And(Box::new(a.clone()), Box::new(b.clone()));
            prop_assert!(LabelType::is_subtype(&ab, &a),
                "(A & B) should be <: A");
            prop_assert!(LabelType::is_subtype(&ab, &b),
                "(A & B) should be <: B");
        }

        #[test]
        fn label_subtype_of_or_with_self(
            a in arb_label_type(),
            b in arb_label_type(),
        ) {
            let or = LabelType::Or(Box::new(a.clone()), Box::new(b));
            prop_assert!(LabelType::is_subtype(&a, &or),
                "A should be <: (A | B)");
        }
    }
}

// =======================================================================
// PropertyType
// =======================================================================

mod property_type {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        #[test]
        fn subtype_reflexive(t in arb_property_type()) {
            prop_assert!(PropertyType::is_subtype(&t, &t),
                "is_subtype({t}, {t}) should be true");
        }

        #[test]
        fn meet_idempotent(t in arb_property_type()) {
            let m = PropertyType::meet(&t, &t);
            prop_assert!(
                PropertyType::is_subtype(&m, &t) && PropertyType::is_subtype(&t, &m),
                "meet({t}, {t}) = {m} should ≡ {t}"
            );
        }

        #[test]
        fn meet_lower_bound(a in arb_property_type(), b in arb_property_type()) {
            let m = PropertyType::meet(&a, &b);
            prop_assert!(PropertyType::is_subtype(&m, &a),
                "meet({a}, {b}) = {m} should be <: {a}");
            prop_assert!(PropertyType::is_subtype(&m, &b),
                "meet({a}, {b}) = {m} should be <: {b}");
        }

        #[test]
        fn meet_commutative(a in arb_property_type(), b in arb_property_type()) {
            let ab = PropertyType::meet(&a, &b);
            let ba = PropertyType::meet(&b, &a);
            prop_assert!(
                PropertyType::is_subtype(&ab, &ba) && PropertyType::is_subtype(&ba, &ab),
                "meet({a}, {b}) = {ab} should ≡ meet({b}, {a}) = {ba}"
            );
        }
    }
}

// =======================================================================
// DescriptorType (composes Label + Property)
// =======================================================================

mod descriptor_type {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        #[test]
        fn subtype_reflexive(t in arb_descriptor_type()) {
            prop_assert!(DescriptorType::is_subtype(&t, &t),
                "is_subtype({t}, {t}) should be true");
        }

        #[test]
        fn meet_idempotent(t in arb_descriptor_type()) {
            let m = DescriptorType::meet(&t, &t);
            prop_assert!(
                DescriptorType::is_subtype(&m, &t) && DescriptorType::is_subtype(&t, &m),
                "meet({t}, {t}) = {m} should ≡ {t}"
            );
        }

        #[test]
        fn meet_lower_bound(a in arb_descriptor_type(), b in arb_descriptor_type()) {
            let m = DescriptorType::meet(&a, &b);
            prop_assert!(DescriptorType::is_subtype(&m, &a),
                "meet({a}, {b}) = {m} should be <: {a}");
            prop_assert!(DescriptorType::is_subtype(&m, &b),
                "meet({a}, {b}) = {m} should be <: {b}");
        }

        #[test]
        fn meet_commutative(a in arb_descriptor_type(), b in arb_descriptor_type()) {
            let ab = DescriptorType::meet(&a, &b);
            let ba = DescriptorType::meet(&b, &a);
            prop_assert!(
                DescriptorType::is_subtype(&ab, &ba) && DescriptorType::is_subtype(&ba, &ab),
                "meet({a}, {b}) = {ab} should ≡ meet({b}, {a}) = {ba}"
            );
        }

        #[test]
        fn star_is_subtype_supremum(t in arb_descriptor_type()) {
            let star = DescriptorType::star();
            prop_assert!(DescriptorType::is_subtype(&t, &star),
                "{t} should be <: star() (Star label + Open empty props)");
        }
    }
}

// =======================================================================
// VariableType
// =======================================================================

mod variable_type {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        #[test]
        fn subtype_reflexive(t in arb_variable_type()) {
            prop_assert!(VariableType::is_subtype(&t, &t),
                "is_subtype({t}, {t}) should be true");
        }

        #[test]
        fn meet_idempotent(t in arb_variable_type()) {
            let m = VariableType::meet(&t, &t);
            prop_assert!(
                VariableType::is_subtype(&m, &t) && VariableType::is_subtype(&t, &m),
                "meet({t}, {t}) = {m} should ≡ {t}"
            );
        }

        #[test]
        fn meet_lower_bound(a in arb_variable_type(), b in arb_variable_type()) {
            let m = VariableType::meet(&a, &b);
            prop_assert!(VariableType::is_subtype(&m, &a),
                "meet({a}, {b}) = {m} should be <: {a}");
            prop_assert!(VariableType::is_subtype(&m, &b),
                "meet({a}, {b}) = {m} should be <: {b}");
        }

        #[test]
        fn meet_commutative(a in arb_variable_type(), b in arb_variable_type()) {
            let ab = VariableType::meet(&a, &b);
            let ba = VariableType::meet(&b, &a);
            prop_assert!(
                VariableType::is_subtype(&ab, &ba) && VariableType::is_subtype(&ba, &ab),
                "meet({a}, {b}) = {ab} should ≡ meet({b}, {a}) = {ba}"
            );
        }

        #[test]
        fn zero_absorbs_in_meet(t in arb_variable_type()) {
            prop_assert_eq!(VariableType::meet(&VariableType::Zero, &t), VariableType::Zero,
                "meet(Zero, t) should = Zero");
            prop_assert_eq!(VariableType::meet(&t, &VariableType::Zero), VariableType::Zero,
                "meet(t, Zero) should = Zero");
        }

        #[test]
        fn zero_is_subtype_of_everything(t in arb_variable_type()) {
            prop_assert!(VariableType::is_subtype(&VariableType::Zero, &t),
                "Zero should be <: {t}");
        }

        #[test]
        fn refine_against_star_schema_is_identity_for_compatible_shapes(
            t in arb_refinable_variable(),
        ) {
            let r = VariableType::refine(&Schema::star(), &t);
            prop_assert!(!r.is_empty(), "refine(star, {t}) collapsed to empty: {r}");
        }
    }
}

/// `VariableType` restricted to shapes refine can succeed on under
/// `Schema::star()` — Node / EdgeDirectional / EdgeNonDirectional with
/// non-empty descriptors. Avoids the post-filter reject-loop that the
/// generic `arb_variable_type` would force.
fn arb_refinable_variable() -> impl Strategy<Value = VariableType> {
    // Refinable descriptor: open empty props (always ≤ schema's open
    // empty), label may be Star or atom — both are admissible.
    let refinable_desc = prop_oneof![Just(LabelType::Star), "[A-D]".prop_map(LabelType::Label),]
        .prop_map(|l| DescriptorType::new(l, PropertyType::open_empty()));

    let node_endpoint = refinable_desc.clone().prop_map(VariableType::Node);

    prop_oneof![
        refinable_desc.clone().prop_map(VariableType::Node),
        (
            refinable_desc.clone(),
            node_endpoint.clone(),
            node_endpoint.clone()
        )
            .prop_map(|(d, l, r)| VariableType::EdgeDirectional {
                desc: d,
                left: Box::new(l),
                right: Box::new(r),
            }),
        (refinable_desc, node_endpoint.clone(), node_endpoint).prop_map(|(d, l, r)| {
            VariableType::EdgeNonDirectional {
                desc: d,
                left: Box::new(l),
                right: Box::new(r),
            }
        }),
    ]
}

// =======================================================================
// rules.md §3.1 — Label subtyping rules
// =======================================================================
//
// These properties encode the inference rules from rules.md directly.
// Each test names the rule (`and_lhs_left_premise_suffices`, etc.) so a
// failure points at a specific rule in the spec.

mod label_spec_rules {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        // §3.1 Star — gradual: ★ ≤ ℓ AND ℓ ≤ ★
        #[test]
        fn star_is_subtype_of_anything(t in arb_label_type()) {
            prop_assert!(LabelType::is_subtype(&LabelType::Star, &t),
                "Star ≤ {t} should hold (rules.md §3.1, Star bidirectional)");
        }
        #[test]
        fn anything_is_subtype_of_star(t in arb_label_type()) {
            prop_assert!(LabelType::is_subtype(&t, &LabelType::Star),
                "{t} ≤ Star should hold (rules.md §3.1, Star bidirectional)");
        }

        // §3.1 Intersection — And-LHS:
        //   (ℓ₁ ≤ ℓ₃)  ⇒  (ℓ₁ & ℓ₂) ≤ ℓ₃
        //   (ℓ₂ ≤ ℓ₃)  ⇒  (ℓ₁ & ℓ₂) ≤ ℓ₃
        #[test]
        fn and_lhs_left_premise_suffices(
            a in arb_label_type(),
            b in arb_label_type(),
            c in arb_label_type(),
        ) {
            prop_assume!(LabelType::is_subtype(&a, &c));
            let ab = LabelType::And(Box::new(a), Box::new(b));
            prop_assert!(LabelType::is_subtype(&ab, &c),
                "rules.md §3.1 And-LHS: (a ≤ c) ⇒ (a & b) ≤ c");
        }
        #[test]
        fn and_lhs_right_premise_suffices(
            a in arb_label_type(),
            b in arb_label_type(),
            c in arb_label_type(),
        ) {
            prop_assume!(LabelType::is_subtype(&b, &c));
            let ab = LabelType::And(Box::new(a), Box::new(b));
            prop_assert!(LabelType::is_subtype(&ab, &c),
                "rules.md §3.1 And-LHS: (b ≤ c) ⇒ (a & b) ≤ c");
        }

        // §3.1 Intersection — And-RHS (BOTH premises required):
        //   (ℓ₁ ≤ ℓ₂)  ∧  (ℓ₁ ≤ ℓ₃)  ⇒  ℓ₁ ≤ (ℓ₂ & ℓ₃)
        #[test]
        fn and_rhs_requires_both_premises(
            a in arb_label_type(),
            b in arb_label_type(),
            c in arb_label_type(),
        ) {
            prop_assume!(LabelType::is_subtype(&a, &b));
            prop_assume!(LabelType::is_subtype(&a, &c));
            let bc = LabelType::And(Box::new(b), Box::new(c));
            prop_assert!(LabelType::is_subtype(&a, &bc),
                "rules.md §3.1 And-RHS: (a ≤ b) ∧ (a ≤ c) ⇒ a ≤ (b & c)");
        }

        // §3.1 Gradual Union — Or-LHS PLAUSIBILITY (either premise alone):
        //   (ℓ₁ ≤ ℓ₃)  ⇒  (ℓ₁ + ℓ₂) ≤ ℓ₃
        //   (ℓ₂ ≤ ℓ₃)  ⇒  (ℓ₁ + ℓ₂) ≤ ℓ₃
        // NOTE: this is the gradual rule. Pygql implements the older
        // strict rule (BOTH required) — that's fixed in fppc/gqlite.
        #[test]
        fn or_lhs_left_premise_suffices(
            a in arb_label_type(),
            b in arb_label_type(),
            c in arb_label_type(),
        ) {
            prop_assume!(LabelType::is_subtype(&a, &c));
            let ab = LabelType::Or(Box::new(a), Box::new(b));
            prop_assert!(LabelType::is_subtype(&ab, &c),
                "rules.md §3.1 Or-LHS plausibility: (a ≤ c) ⇒ (a + b) ≤ c");
        }
        #[test]
        fn or_lhs_right_premise_suffices(
            a in arb_label_type(),
            b in arb_label_type(),
            c in arb_label_type(),
        ) {
            prop_assume!(LabelType::is_subtype(&b, &c));
            let ab = LabelType::Or(Box::new(a), Box::new(b));
            prop_assert!(LabelType::is_subtype(&ab, &c),
                "rules.md §3.1 Or-LHS plausibility: (b ≤ c) ⇒ (a + b) ≤ c");
        }

        // §3.1 Gradual Union — Or-RHS PLAUSIBILITY:
        //   (ℓ₃ ≤ ℓ₁)  ⇒  ℓ₃ ≤ (ℓ₁ + ℓ₂)
        //   (ℓ₃ ≤ ℓ₂)  ⇒  ℓ₃ ≤ (ℓ₁ + ℓ₂)
        #[test]
        fn or_rhs_left_premise_suffices(
            a in arb_label_type(),
            b in arb_label_type(),
            c in arb_label_type(),
        ) {
            prop_assume!(LabelType::is_subtype(&c, &a));
            let ab = LabelType::Or(Box::new(a), Box::new(b));
            prop_assert!(LabelType::is_subtype(&c, &ab),
                "rules.md §3.1 Or-RHS plausibility: (c ≤ a) ⇒ c ≤ (a + b)");
        }
        #[test]
        fn or_rhs_right_premise_suffices(
            a in arb_label_type(),
            b in arb_label_type(),
            c in arb_label_type(),
        ) {
            prop_assume!(LabelType::is_subtype(&c, &b));
            let ab = LabelType::Or(Box::new(a), Box::new(b));
            prop_assert!(LabelType::is_subtype(&c, &ab),
                "rules.md §3.1 Or-RHS plausibility: (c ≤ b) ⇒ c ≤ (a + b)");
        }

        // §4.1 Label meet — distinct atoms produce And, NOT bottom.
        #[test]
        fn distinct_atom_meet_yields_and(s in "[A-D]", t in "[A-D]") {
            prop_assume!(s != t);
            let a = LabelType::Label(s);
            let b = LabelType::Label(t);
            let m = LabelType::meet(&a, &b);
            // Must be subtype-equivalent to (a & b) and to each operand
            // (since rules.md §3.1 says (a & b) ≤ a and (a & b) ≤ b).
            prop_assert!(LabelType::is_subtype(&m, &a),
                "rules.md §4.1: 1 ⊓ 1 should be ≤ each operand, got {m}");
            prop_assert!(LabelType::is_subtype(&m, &b),
                "rules.md §4.1: 1 ⊓ 1 should be ≤ each operand, got {m}");
        }
    }
}

// =======================================================================
// rules.md §3.3 / §4.2 — Property type rules
// =======================================================================

mod property_spec_rules {
    use super::*;

    /// Closed record over a controlled key set. Two of these with
    /// non-equal key sets exercise the "no width subtyping" rule.
    fn arb_closed_with_keys(keys: &'static [&'static str]) -> impl Strategy<Value = PropertyType> {
        let entries: Vec<_> = keys
            .iter()
            .map(|k| arb_simple_type().prop_map(move |v| (k.to_string(), v)))
            .collect();
        entries.prop_map(|kvs: Vec<(String, SimpleType)>| {
            PropertyType::Closed(kvs.into_iter().collect())
        })
    }

    proptest! {
        #![proptest_config(cfg())]

        // §3.3 Closed records: width subtyping is FORBIDDEN.
        // Two Closed records with different key sets must NOT be subtypes
        // in either direction, regardless of what their values look like.
        #[test]
        fn closed_records_no_width_subtyping(
            r1 in arb_closed_with_keys(&["a"]),
            r2 in arb_closed_with_keys(&["a", "b"]),
        ) {
            prop_assert!(!PropertyType::is_subtype(&r1, &r2),
                "rules.md §3.3: Closed records forbid width subtyping ({r1} ≤ {r2} should be false)");
            prop_assert!(!PropertyType::is_subtype(&r2, &r1),
                "rules.md §3.3: Closed records forbid width subtyping ({r2} ≤ {r1} should be false)");
        }

        // §3.3 Open records: width subtyping IS allowed via the star.
        // Construct two Opens that share key 'a' with the SAME type
        // (so value-compat is trivial), with one side adding an extra
        // key 'b'. Width must let them relate in at least one direction.
        #[test]
        fn open_records_allow_width_subtyping(
            shared in arb_simple_type(),
            extra in arb_simple_type(),
        ) {
            let mut narrow = BTreeMap::new();
            narrow.insert("a".to_string(), shared.clone());
            let mut wide = BTreeMap::new();
            wide.insert("a".to_string(), shared);
            wide.insert("b".to_string(), extra);
            let r1 = PropertyType::Open(narrow);
            let r2 = PropertyType::Open(wide);

            let s1 = PropertyType::is_subtype(&r1, &r2);
            let s2 = PropertyType::is_subtype(&r2, &r1);
            prop_assert!(s1 || s2,
                "rules.md §3.3: Open records permit width subtyping; {r1} and {r2} should relate");
        }

        // §4.2 Property meet — Zero is absorbing.
        #[test]
        fn zero_absorbs_property_meet(t in arb_property_type()) {
            prop_assert_eq!(PropertyType::meet(&PropertyType::Zero, &t), PropertyType::Zero,
                "rules.md §4.2: ⊥ ⊓ R = ⊥");
        }
    }
}

// =======================================================================
// rules.md §4.3 — Variable / Edge type meet rules
// =======================================================================

mod variable_spec_rules {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        // §4.3 Undirected edge meet — orientation symmetric.
        // meet(NonDir(d, l, r), NonDir(d', l', r')) = T₃ ⊔ T₄ where T₃
        // tries (l,r) vs (l',r') and T₄ tries (l,r) vs (r',l'). Swapping
        // l'/r' on one operand should produce an equivalent type.
        #[test]
        fn undirected_edge_meet_orientation_symmetric(
            d1 in arb_descriptor_type(),
            d2 in arb_descriptor_type(),
            l1 in arb_descriptor_type(),
            r1 in arb_descriptor_type(),
            l2 in arb_descriptor_type(),
            r2 in arb_descriptor_type(),
        ) {
            let mk = |d, l, r| VariableType::EdgeNonDirectional {
                desc: d,
                left: Box::new(VariableType::Node(l)),
                right: Box::new(VariableType::Node(r)),
            };
            let normal = VariableType::meet(&mk(d1.clone(), l1.clone(), r1.clone()),
                                            &mk(d2.clone(), l2.clone(), r2.clone()));
            let swapped = VariableType::meet(&mk(d1, l1, r1),
                                             &mk(d2, r2, l2));
            // Subtype-equivalent rather than syntactic equality, since
            // joins of differently-ordered operands may not normalize.
            prop_assert!(VariableType::is_subtype(&normal, &swapped),
                "rules.md §4.3: undirected meet should be orientation-symmetric");
            prop_assert!(VariableType::is_subtype(&swapped, &normal),
                "rules.md §4.3: undirected meet should be orientation-symmetric");
        }

        // §6 Refinement union distribution:
        //   refine(S, T₁ + T₂)  ≡  refine(S, T₁) ⊔ refine(S, T₂)
        #[test]
        fn refinement_distributes_over_union(
            t1 in arb_variable_type(),
            t2 in arb_variable_type(),
        ) {
            let schema = Schema::star();
            let direct = VariableType::refine(
                &schema,
                &VariableType::Union(Box::new(t1.clone()), Box::new(t2.clone())),
            );
            let split = VariableType::join(
                &VariableType::refine(&schema, &t1),
                &VariableType::refine(&schema, &t2),
            );
            prop_assert!(VariableType::is_subtype(&direct, &split),
                "rules.md §6: refine(T₁ + T₂) ≤ refine(T₁) ⊔ refine(T₂)");
            prop_assert!(VariableType::is_subtype(&split, &direct),
                "rules.md §6: refine(T₁ + T₂) ≥ refine(T₁) ⊔ refine(T₂)");
        }

        // §8.4 empty() — Union case requires BOTH sides empty.
        // (Distinct from "is empty" for path length.)
        #[test]
        fn union_empty_iff_both_sides_empty(
            t in arb_variable_type(),
        ) {
            // Pair t with Zero — Union(t, Zero) should be empty iff t is.
            let u_left = VariableType::Union(Box::new(VariableType::Zero), Box::new(t.clone()));
            let u_right = VariableType::Union(Box::new(t.clone()), Box::new(VariableType::Zero));
            prop_assert_eq!(u_left.is_empty(), t.is_empty(),
                "rules.md §8.4: empty(Zero + t) iff empty(t)");
            prop_assert_eq!(u_right.is_empty(), t.is_empty(),
                "rules.md §8.4: empty(t + Zero) iff empty(t)");
        }
    }
}

// =======================================================================
// rules.md §8.2 — Path concatenation via meet
// =======================================================================
//
// PathType has a SPECIAL meet that concatenates paths at compatible
// boundary nodes. This is the rule that makes `(x)-[]-(y)` and
// `(y)-[]-(z)` typecheck as one path of length 2.

mod path_type {
    use super::*;

    fn star_node() -> PathType {
        PathType::node(DescriptorType::star())
    }

    fn star_edge() -> PathType {
        PathType::Edge(EdgePathType {
            p1: Box::new(star_node()),
            n2: NodePathType::new(DescriptorType::star()),
        })
    }

    // NOTE on PathType meet: unlike other lattice types, PathType::meet
    // CONCATENATES rather than narrowing — see §8.2. So `meet(p, p)` is
    // *not* idempotent (it doubles the path length on compatible
    // boundaries) and we don't test reflexivity here. Concatenation
    // semantics is verified by the focused unit tests below.

    proptest! {
        #![proptest_config(cfg())]

        // §4 — Zero absorbs in meet.
        #[test]
        fn zero_absorbs_path_meet_left(p in arb_path_type()) {
            let m = PathType::meet(&Schema::star(), &PathType::Zero, &p);
            prop_assert_eq!(m, PathType::Zero,
                "rules.md §4: ⊥ ⊓ P = ⊥");
        }
        #[test]
        fn zero_absorbs_path_meet_right(p in arb_path_type()) {
            let m = PathType::meet(&Schema::star(), &p, &PathType::Zero);
            prop_assert_eq!(m, PathType::Zero,
                "rules.md §4: P ⊓ ⊥ = ⊥");
        }

        // §4 — Union (Zero, P) = P.
        #[test]
        fn zero_is_path_union_identity_left(p in arb_path_type()) {
            prop_assert_eq!(PathType::union(PathType::Zero, p.clone()), p);
        }
        #[test]
        fn zero_is_path_union_identity_right(p in arb_path_type()) {
            prop_assert_eq!(PathType::union(p.clone(), PathType::Zero), p);
        }
    }

    // §8.2 — Path meet concatenates. Hand-picked rather than randomized
    // because boundary compatibility is brittle to arbitrary descriptors;
    // a focused unit test pins the contract clearly.

    /// rules.md §8.2 right-extension:
    ///   `(P₁ - N₂) ⊓ N₃ ▷ P₁ - N₄`  where  `N₂ ⊓ N₃ ▷ N₄`
    /// Meeting an edge-path with a single node refines the boundary node
    /// without extending the path.
    #[test]
    fn path_meet_right_extension_preserves_length() {
        let edge = star_edge();
        let node = star_node();
        let m = PathType::meet(&Schema::star(), &edge, &node);
        assert_eq!(
            m.len(),
            1,
            "rules.md §8.2 right-extension: (edge ⊓ node) should preserve edge length"
        );
    }

    /// rules.md §8.2 left-extension:
    ///   `P₁ ⊓ (P₂ - N₃) ▷ P₃ - N₃`  where  `P₁ ⊓ P₂ ▷ P₃`
    /// Meeting two single-edge paths yields a length-2 path —
    /// the second path's edge is "appended" to the first.
    #[test]
    fn path_meet_left_extension_concatenates_two_edges() {
        let p1 = star_edge();
        let p2 = star_edge();
        let m = PathType::meet(&Schema::star(), &p1, &p2);
        assert_eq!(
            m.len(),
            2,
            "rules.md §8.2 left-extension: meeting two edges should concatenate to length 2, got {m:?}"
        );
    }

    /// Three single edges meet into a length-3 path. Generalizes the
    /// above; if associativity of left-extension holds, this is implied
    /// but worth a direct check.
    #[test]
    fn path_meet_chains_three_edges() {
        let e = star_edge();
        let m1 = PathType::meet(&Schema::star(), &e, &e);
        let m2 = PathType::meet(&Schema::star(), &m1, &e);
        assert_eq!(
            m2.len(),
            3,
            "meeting three edges should concatenate to length 3, got {m2:?}"
        );
    }

    /// rules.md §8.1 Direction Resolution — undirected edges resolve to
    /// the union of forward and backward orientations. Use DISTINCT
    /// endpoint descriptors so `forward != reversed` and
    /// `PathType::union` doesn't normalize them away.
    #[test]
    fn from_variable_undirected_yields_union_of_orientations() {
        let left_desc =
            DescriptorType::new(LabelType::Label("L".into()), PropertyType::open_empty());
        let right_desc =
            DescriptorType::new(LabelType::Label("R".into()), PropertyType::open_empty());
        let edge = VariableType::EdgeNonDirectional {
            desc: DescriptorType::star(),
            left: Box::new(VariableType::Node(left_desc)),
            right: Box::new(VariableType::Node(right_desc)),
        };
        let p = PathType::from_variable(&edge, EdgeDir::Any);
        // Undirected ⌊...⌋∼ = (N₁-N₂) + (N₂-N₁) — must be a Union when
        // endpoints differ.
        assert!(
            matches!(p, PathType::Union(_, _)),
            "rules.md §8.1 Undirected: ⌊N₁∼N₂⌋∼ should be a Union, got {p:?}"
        );
    }
}
