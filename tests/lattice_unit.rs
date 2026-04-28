//! Hand-picked unit tests for the typing lattice.
//!
//! Companion to `tests/lattice_proptest.rs`. The split:
//!
//! - **`lattice_proptest.rs`** — randomized property tests on
//!   algebraic invariants and spec rules. Uses `proptest`,
//!   canonicalization, and constructive generators.
//! - **`lattice_unit.rs`** (this file) — hand-picked `assert_eq!`
//!   pairs that pin specific input → output behaviors of the impl.
//!   No randomness, no canon, no proptest dependency.
//!
//! Three layers, each independent of the others (and of canon):
//!
//! 1. `predicate_locks` — ground-truth tests for `is_empty` /
//!    `is_unsatisfiable`. Canon depends on these predicates; without
//!    direct locks a subtle predicate regression could slip past
//!    everything else.
//! 2. `normalization_locks` — assertions on the impl's normalizing
//!    behavior in `union` / `meet` / `join` (Zero-drop, Star-drop,
//!    equal-collapse). Caught directly here, independent of canon.
//! 3. `meet_locks` — backstop against Star / Zero collapse regressions
//!    that consistency-based proptests would hide.
//!
//! Spec references (`§3.1`, etc.) point at `docs/rules.md`.

use std::collections::BTreeMap;

use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::path_type::PathType;
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::simple_type::SimpleType;
use gqlrust::typing::variable_type::{Schema, VariableType};

// =======================================================================
// Predicate locks — ground-truth tests for `is_empty` / `is_unsatisfiable`
// =======================================================================
//
// `canon` (in the proptest file) depends on these predicates. The
// `union_empty_iff_both_components_empty` proptest checks `is_empty`
// against itself — tautological. A subtle predicate regression could
// slip past everything else if proptest doesn't generate the triggering
// shape. These hand-picked assertions pin specific input → bool pairs
// covering each match arm in each predicate.

mod predicate_locks {
    use super::*;

    // SimpleType::is_empty — covers each arm of the impl.
    #[test]
    fn simple_zero_is_empty() {
        assert!(SimpleType::Zero.is_empty());
    }
    #[test]
    fn simple_atoms_are_not_empty() {
        assert!(!SimpleType::Z.is_empty());
        assert!(!SimpleType::F.is_empty());
        assert!(!SimpleType::B.is_empty());
        assert!(!SimpleType::S.is_empty());
        assert!(!SimpleType::Star.is_empty());
    }
    #[test]
    fn simple_union_empty_iff_both_empty() {
        // Match arm: Union(a, b) => a.is_empty() && b.is_empty()
        let zero = || SimpleType::Zero;
        let one = || SimpleType::Z;
        assert!(SimpleType::Union(Box::new(zero()), Box::new(zero())).is_empty());
        assert!(!SimpleType::Union(Box::new(zero()), Box::new(one())).is_empty());
        assert!(!SimpleType::Union(Box::new(one()), Box::new(zero())).is_empty());
        assert!(!SimpleType::Union(Box::new(one()), Box::new(one())).is_empty());
    }
    #[test]
    fn simple_group_empty_iff_inner_empty() {
        assert!(SimpleType::Group(Box::new(SimpleType::Zero)).is_empty());
        assert!(!SimpleType::Group(Box::new(SimpleType::Z)).is_empty());
    }
    #[test]
    fn simple_list_empty_iff_inner_empty() {
        assert!(SimpleType::List(Box::new(SimpleType::Zero)).is_empty());
        assert!(!SimpleType::List(Box::new(SimpleType::Z)).is_empty());
    }
    #[test]
    fn simple_record_empty_iff_any_field_empty() {
        // Match arm: Record(fields) => fields.values().any(is_empty).
        // NOTE: this is `any`, not `all` — different from Union.
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), SimpleType::Z);
        m.insert("b".to_string(), SimpleType::Zero);
        assert!(
            SimpleType::Record(m).is_empty(),
            "record with one Zero field should be empty (any-semantics)"
        );

        let mut m2 = BTreeMap::new();
        m2.insert("a".to_string(), SimpleType::Z);
        m2.insert("b".to_string(), SimpleType::F);
        assert!(!SimpleType::Record(m2).is_empty());

        // Empty record (no fields): not empty per the impl.
        assert!(!SimpleType::Record(BTreeMap::new()).is_empty());
    }

    // PropertyType::is_empty — quirk: empty record is NOT empty,
    // but a non-empty record with any empty field IS.
    #[test]
    fn property_zero_is_empty() {
        assert!(PropertyType::Zero.is_empty());
    }
    #[test]
    fn property_empty_record_is_not_empty() {
        // Per impl: `!m.is_empty() && m.values().any(...)`.
        // Empty maps fail the first conjunct, so are NOT empty.
        assert!(!PropertyType::open_empty().is_empty());
        assert!(!PropertyType::closed_empty().is_empty());
    }
    #[test]
    fn property_record_with_empty_field_is_empty() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), SimpleType::Zero);
        assert!(PropertyType::Open(m.clone()).is_empty());
        assert!(PropertyType::Closed(m).is_empty());
    }
    #[test]
    fn property_record_with_only_full_fields_is_not_empty() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), SimpleType::Z);
        assert!(!PropertyType::Open(m.clone()).is_empty());
        assert!(!PropertyType::Closed(m).is_empty());
    }

    // VariableType::is_empty — Edge variants use OR over (desc, left,
    // right) — different from Union which uses AND.
    #[test]
    fn variable_zero_is_empty() {
        assert!(VariableType::Zero.is_empty());
    }
    #[test]
    fn variable_node_empty_iff_descriptor_empty() {
        let empty_d = DescriptorType::new(LabelType::Star, PropertyType::Zero);
        assert!(VariableType::Node(empty_d).is_empty());
        assert!(!VariableType::node_star().is_empty());
    }
    #[test]
    fn variable_edge_directional_empty_iff_any_component_empty() {
        // Match arm: desc.is_empty() || left.is_empty() || right.is_empty()
        let star_node = || Box::new(VariableType::node_star());
        let empty_node = || {
            Box::new(VariableType::Node(DescriptorType::new(
                LabelType::Star,
                PropertyType::Zero,
            )))
        };
        // All full: not empty.
        assert!(!VariableType::EdgeDirectional {
            desc: DescriptorType::star(),
            left: star_node(),
            right: star_node(),
        }
        .is_empty());
        // Empty left → empty edge.
        assert!(VariableType::EdgeDirectional {
            desc: DescriptorType::star(),
            left: empty_node(),
            right: star_node(),
        }
        .is_empty());
        // Empty right → empty edge.
        assert!(VariableType::EdgeDirectional {
            desc: DescriptorType::star(),
            left: star_node(),
            right: empty_node(),
        }
        .is_empty());
    }
    #[test]
    fn variable_union_empty_iff_both_empty() {
        // Sanity: complement to Edge's OR semantics.
        let zero = || Box::new(VariableType::Zero);
        let n = || Box::new(VariableType::node_star());
        assert!(VariableType::Union(zero(), zero()).is_empty());
        assert!(!VariableType::Union(zero(), n()).is_empty());
        assert!(!VariableType::Union(n(), zero()).is_empty());
    }

    // PathType::is_unsatisfiable.
    #[test]
    fn path_zero_is_unsatisfiable() {
        assert!(PathType::Zero.is_unsatisfiable());
    }
    #[test]
    fn path_node_unsat_iff_descriptor_empty() {
        let empty_d = DescriptorType::new(LabelType::Star, PropertyType::Zero);
        assert!(PathType::node(empty_d).is_unsatisfiable());
        assert!(!PathType::node(DescriptorType::star()).is_unsatisfiable());
    }
    #[test]
    fn path_union_unsat_iff_both_unsat() {
        // Different from path is_empty (which uses OR for length min) —
        // is_unsatisfiable uses AND for Union per §8.4.
        let n = || PathType::node(DescriptorType::star());
        assert!(
            PathType::Union(Box::new(PathType::Zero), Box::new(PathType::Zero)).is_unsatisfiable()
        );
        assert!(!PathType::Union(Box::new(PathType::Zero), Box::new(n())).is_unsatisfiable());
        assert!(!PathType::Union(Box::new(n()), Box::new(PathType::Zero)).is_unsatisfiable());
    }
}

// =======================================================================
// Normalization locks — independent assertions on the impl's normalizing
// behavior
// =======================================================================
//
// `canon`'s collapse-empty step uses gqlite's `is_empty` /
// `is_unsatisfiable` predicates. If gqlite's `union`/`join`/`meet`
// regress on identity-dropping (e.g. `union(Zero, X)` returns
// `Union(Zero, X)`), canon would still equate that with `X` — the
// regression is silent on canon-based tests.
//
// These hand-picked locks assert specific input → output pairs on
// `union/meet/join` directly. They don't go through canon, so a
// regression is caught even when canon's predicate-based collapse
// would mask it. The two layers depend on different parts of the
// impl and detect disjoint regression classes.

mod normalization_locks {
    use super::*;

    // SimpleType::union — Zero is identity for join.
    #[test]
    fn simple_union_drops_left_zero() {
        assert_eq!(
            SimpleType::union(&SimpleType::Zero, &SimpleType::Z),
            SimpleType::Z
        );
    }
    #[test]
    fn simple_union_drops_right_zero() {
        assert_eq!(
            SimpleType::union(&SimpleType::Z, &SimpleType::Zero),
            SimpleType::Z
        );
    }
    #[test]
    fn simple_union_collapses_equal_operands() {
        assert_eq!(
            SimpleType::union(&SimpleType::Z, &SimpleType::Z),
            SimpleType::Z
        );
    }

    // LabelType::meet — Star is identity for And. (Also covered in
    // `meet_locks::label_meet_star_with_atom_returns_atom`; pinned
    // here so the normalization-by-normalization audit is in one place.)
    #[test]
    fn label_meet_drops_left_star() {
        let a = LabelType::Label("A".into());
        assert_eq!(LabelType::meet(&LabelType::Star, &a), a);
    }
    #[test]
    fn label_meet_drops_right_star() {
        let a = LabelType::Label("A".into());
        assert_eq!(LabelType::meet(&a, &LabelType::Star), a);
    }

    // VariableType::join — Zero is identity for join.
    #[test]
    fn variable_join_drops_left_zero() {
        let n = VariableType::node_star();
        assert_eq!(VariableType::join(&VariableType::Zero, &n), n);
    }
    #[test]
    fn variable_join_drops_right_zero() {
        let n = VariableType::node_star();
        assert_eq!(VariableType::join(&n, &VariableType::Zero), n);
    }
    #[test]
    fn variable_join_collapses_equal_operands() {
        let n = VariableType::node_star();
        assert_eq!(VariableType::join(&n, &n), n);
    }

    // PathType::union — Zero is identity. (Also covered by the
    // `path_type::zero_is_path_union_identity_*` proptests in the
    // proptest file; pinned explicitly here for the audit.)
    #[test]
    fn path_union_drops_left_zero() {
        let n = PathType::node(DescriptorType::star());
        assert_eq!(PathType::union(PathType::Zero, n.clone()), n);
    }
    #[test]
    fn path_union_drops_right_zero() {
        let n = PathType::node(DescriptorType::star());
        assert_eq!(PathType::union(n.clone(), PathType::Zero), n);
    }
}

// =======================================================================
// Meet output locks — backstop against Star/Zero-collapse regressions
// =======================================================================
//
// The `assert_consistent!` assertions in the proptest file's
// `mod label_type` / `mod descriptor_type` / `mod variable_type` use
// mutual subtype, which is loose: a buggy `meet` returning `Star` for
// everything would still satisfy consistency (Star is bidirectional
// under §3 plausibility). These hand-picked tests pin specific outputs
// so the regression fails.

mod meet_locks {
    use super::*;

    // SimpleType — atomic preservation.
    #[test]
    fn simple_meet_atom_with_self_returns_atom() {
        // meet(Z, Z) should be Z, not Star or anything wider.
        assert_eq!(
            SimpleType::meet(&SimpleType::Z, &SimpleType::Z),
            SimpleType::Z
        );
        assert_eq!(
            SimpleType::meet(&SimpleType::F, &SimpleType::F),
            SimpleType::F
        );
        assert_eq!(
            SimpleType::meet(&SimpleType::B, &SimpleType::B),
            SimpleType::B
        );
        assert_eq!(
            SimpleType::meet(&SimpleType::S, &SimpleType::S),
            SimpleType::S
        );
    }

    #[test]
    fn simple_meet_distinct_atoms_yields_zero() {
        // §4.1: distinct atoms have no common subtype other than Zero.
        // A regression returning Star would satisfy mutual-subtype tests.
        assert_eq!(
            SimpleType::meet(&SimpleType::Z, &SimpleType::F),
            SimpleType::Zero
        );
        assert_eq!(
            SimpleType::meet(&SimpleType::Z, &SimpleType::B),
            SimpleType::Zero
        );
        assert_eq!(
            SimpleType::meet(&SimpleType::S, &SimpleType::Z),
            SimpleType::Zero
        );
    }

    #[test]
    fn simple_meet_star_returns_specific_operand() {
        // §4.1: ★ ⊓ τ = τ (specific). A regression returning Star would
        // pass mutual subtype but fail this lock.
        assert_eq!(
            SimpleType::meet(&SimpleType::Star, &SimpleType::Z),
            SimpleType::Z
        );
        assert_eq!(
            SimpleType::meet(&SimpleType::Z, &SimpleType::Star),
            SimpleType::Z
        );
    }

    // LabelType — atomic preservation.
    #[test]
    fn label_meet_same_atom_returns_that_atom() {
        let a = LabelType::Label("A".into());
        assert_eq!(LabelType::meet(&a, &a), a);
    }

    #[test]
    fn label_meet_distinct_atoms_yields_and() {
        // §4.1: 1₁ ⊓ 1₂ = 1₁ & 1₂. A regression returning Star, or Or,
        // or just one operand, would all fail this lock.
        let a = LabelType::Label("A".into());
        let b = LabelType::Label("B".into());
        let m = LabelType::meet(&a, &b);
        assert!(
            matches!(&m, LabelType::And(_, _)),
            "meet of distinct atoms should be And, got {m:?}"
        );
    }

    #[test]
    fn label_meet_star_with_atom_returns_atom() {
        // §4.1 explicit: ★ ⊓ ℓ = ℓ.
        let a = LabelType::Label("A".into());
        assert_eq!(LabelType::meet(&LabelType::Star, &a), a);
        assert_eq!(LabelType::meet(&a, &LabelType::Star), a);
    }

    // PropertyType — record preservation.
    #[test]
    fn property_meet_same_closed_returns_same() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), SimpleType::Z);
        let r = PropertyType::Closed(m);
        assert_eq!(PropertyType::meet(&r, &r), r);
    }

    #[test]
    fn property_meet_different_closed_keys_yields_zero() {
        // gqlite treats records-with-different-keys as incompatible.
        // A regression returning Open or one of the operands would fail.
        let mut m1 = BTreeMap::new();
        m1.insert("a".to_string(), SimpleType::Z);
        let mut m2 = BTreeMap::new();
        m2.insert("b".to_string(), SimpleType::Z);
        assert_eq!(
            PropertyType::meet(&PropertyType::Closed(m1), &PropertyType::Closed(m2)),
            PropertyType::Zero
        );
    }

    // VariableType — Node meet preservation.
    #[test]
    fn variable_meet_same_node_returns_same() {
        let n = VariableType::Node(DescriptorType::new(
            LabelType::Label("Person".into()),
            PropertyType::open_empty(),
        ));
        // Idempotence under syntactic equality for Node (no orientation).
        assert_eq!(VariableType::meet(&n, &n), n);
    }

    #[test]
    fn variable_meet_distinct_node_atoms_collapses_descriptor() {
        // meet of two label-distinct Nodes should produce a Node whose
        // descriptor's label is the And. A regression that returned a
        // single atom or Star would fail.
        let na = VariableType::Node(DescriptorType::new(
            LabelType::Label("A".into()),
            PropertyType::open_empty(),
        ));
        let nb = VariableType::Node(DescriptorType::new(
            LabelType::Label("B".into()),
            PropertyType::open_empty(),
        ));
        match VariableType::meet(&na, &nb) {
            VariableType::Node(d) => {
                assert!(
                    matches!(d.label, LabelType::And(_, _)),
                    "meet of (:A) and (:B) should have And label, got {d}"
                );
            }
            other => panic!("meet of two Nodes should be Node, got {other:?}"),
        }
    }

    // Refinement — schema admission.
    #[test]
    fn refine_node_against_schema_with_no_matching_label_returns_zero() {
        // Schema with only `Person`; query for `Animal` → ⊥.
        let schema = Schema {
            nodes: vec![VariableType::Node(DescriptorType::new(
                LabelType::Label("Person".into()),
                PropertyType::open_empty(),
            ))],
            edges: vec![],
        };
        let q = VariableType::Node(DescriptorType::new(
            LabelType::Label("Animal".into()),
            PropertyType::open_empty(),
        ));
        let r = VariableType::refine(&schema, &q);
        assert_eq!(
            r,
            VariableType::Zero,
            "refining (:Animal) against schema with only (:Person) should be ⊥"
        );
    }
}
