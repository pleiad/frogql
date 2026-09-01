//! `SimpleType::Null` — the static type of the `null` literal, and the
//! precision it buys the `=` / `<>` rule.
//!
//! Before this, `null` typed as `Star` (the wildcard). That kept `WHERE x =
//! null` from collapsing, but it conflated two different things: "this is the
//! null value" and "I do not know this type". It also made `=` always type as
//! plain `B`, which under-reports — ISO 3VL says comparing anything against
//! null yields *null*, so the honest result type is `B ∪ Null`.
//!
//! The lattice position is deliberately minimal (`Null <: Star`, `Null <:
//! Null`, nothing else): `Null` is terminal, so it never meets a base type and
//! never refines. `has_null` is the recursive predicate that decides whether a
//! null can reach a comparison — recursive because equality descends into
//! composites (`runtime::eq_3vl`), so a null nested in a list or record is
//! just as reachable as one at the top.

use frogql::syntax::expr::BinOp;
use frogql::typing::simple_type::SimpleType;

fn union(a: SimpleType, b: SimpleType) -> SimpleType {
    SimpleType::union(&a, &b)
}

fn list(t: SimpleType) -> SimpleType {
    SimpleType::List(Box::new(t))
}

fn record(fields: &[(&str, SimpleType)]) -> SimpleType {
    SimpleType::Record(
        fields
            .iter()
            .map(|(k, t)| ((*k).to_string(), t.clone()))
            .collect(),
    )
}

/// The result type of `lhs = rhs`, which is what the precision is about.
fn eq_result(lhs: &SimpleType, rhs: &SimpleType) -> SimpleType {
    BinOp::Eq.delta(lhs, rhs).2
}

/// Whether the checker would accept `lhs = rhs` — it accepts when each
/// operand meets its expected type. Mirrors `check_expr`'s `defined`.
fn eq_defined(lhs: &SimpleType, rhs: &SimpleType) -> bool {
    let (e1, e2, _) = BinOp::Eq.delta(lhs, rhs);
    !SimpleType::meet(lhs, &e1).is_empty() && !SimpleType::meet(rhs, &e2).is_empty()
}

// --- Subtyping: Null is terminal, just below Star ---

#[test]
fn test_null_subtyping_is_only_star_and_itself() {
    assert!(SimpleType::is_subtype(&SimpleType::Null, &SimpleType::Null));
    assert!(SimpleType::is_subtype(&SimpleType::Null, &SimpleType::Star));
    // Bottom is below everything, including Null.
    assert!(SimpleType::is_subtype(&SimpleType::Zero, &SimpleType::Null));

    // Nothing else, in either direction. Null is not a bool, not an int, and
    // no base type is a null.
    for t in [
        SimpleType::Z,
        SimpleType::F,
        SimpleType::B,
        SimpleType::S,
        SimpleType::Node,
        SimpleType::Edge,
        list(SimpleType::Z),
    ] {
        assert!(
            !SimpleType::is_subtype(&SimpleType::Null, &t),
            "Null must not be a subtype of {t}"
        );
        assert!(
            !SimpleType::is_subtype(&t, &SimpleType::Null),
            "{t} must not be a subtype of Null"
        );
    }
}

#[test]
fn test_null_meets_nothing_but_itself_and_star() {
    assert_eq!(
        SimpleType::meet(&SimpleType::Null, &SimpleType::Null),
        SimpleType::Null
    );
    assert_eq!(
        SimpleType::meet(&SimpleType::Null, &SimpleType::Star),
        SimpleType::Null
    );
    assert_eq!(
        SimpleType::meet(&SimpleType::Null, &SimpleType::Z),
        SimpleType::Zero
    );
    assert_eq!(
        SimpleType::meet(&SimpleType::Null, &SimpleType::B),
        SimpleType::Zero
    );
}

#[test]
fn test_null_is_inhabited() {
    // `Null` is a type with exactly one value in it, so it is not empty. A
    // `Zero` here would let `guaranteed_empty` prune a live derivation.
    assert!(!SimpleType::Null.is_empty());
    assert!(!union(SimpleType::B, SimpleType::Null).is_empty());
}

// --- has_null: the recursive predicate ---

#[test]
fn test_has_null_direct_and_through_unions() {
    assert!(SimpleType::Null.has_null());
    // The wildcard admits every value, null included, so it must report true
    // or the comparison rule would under-report on imprecisely typed operands.
    assert!(SimpleType::Star.has_null());
    assert!(union(SimpleType::Z, SimpleType::Null).has_null());
    assert!(!SimpleType::Z.has_null());
    assert!(!SimpleType::Zero.has_null());
    assert!(!union(SimpleType::Z, SimpleType::S).has_null());
}

#[test]
fn test_has_null_descends_into_composites() {
    // The point of the recursion: equality descends into lists and records,
    // so a null buried in one is just as reachable as a top-level null.
    assert!(list(SimpleType::Null).has_null());
    assert!(list(union(SimpleType::Z, SimpleType::Null)).has_null());
    assert!(list(list(SimpleType::Null)).has_null());
    assert!(!list(SimpleType::Z).has_null());

    assert!(record(&[("a", SimpleType::Z), ("b", SimpleType::Null)]).has_null());
    assert!(!record(&[("a", SimpleType::Z), ("b", SimpleType::S)]).has_null());

    // Group is the repetition-grouping constructor; same reasoning.
    assert!(SimpleType::Group(Box::new(SimpleType::Null)).has_null());
}

// --- The payoff: `=` reports B ∪ Null exactly when a null can reach it ---

#[test]
fn test_eq_without_nulls_is_plain_bool() {
    assert_eq!(eq_result(&SimpleType::Z, &SimpleType::Z), SimpleType::B);
    assert_eq!(
        eq_result(&list(SimpleType::Z), &list(SimpleType::Z)),
        SimpleType::B
    );
}

#[test]
fn test_eq_against_null_is_bool_or_null() {
    let expected = union(SimpleType::B, SimpleType::Null);
    assert_eq!(eq_result(&SimpleType::Z, &SimpleType::Null), expected);
    assert_eq!(eq_result(&SimpleType::Null, &SimpleType::Null), expected);
}

#[test]
fn test_eq_over_a_list_holding_a_null_is_bool_or_null() {
    // The case that started this: `[1, null] = [1, null]` is
    // `1 = 1 AND null = null`, so the type must admit null.
    let t = list(union(SimpleType::Z, SimpleType::Null));
    assert_eq!(eq_result(&t, &t), union(SimpleType::B, SimpleType::Null));
}

#[test]
fn test_eq_over_a_record_holding_a_null_is_bool_or_null() {
    let t = record(&[("a", SimpleType::Z), ("b", SimpleType::Null)]);
    assert_eq!(eq_result(&t, &t), union(SimpleType::B, SimpleType::Null));
}

#[test]
fn test_ne_follows_eq() {
    let t = list(union(SimpleType::Z, SimpleType::Null));
    assert_eq!(
        BinOp::Ne.delta(&t, &t).2,
        union(SimpleType::B, SimpleType::Null)
    );
}

// --- Comparing against null must stay well-typed ---

#[test]
fn test_comparing_a_typed_operand_to_null_is_well_typed() {
    // `WHERE x.age = null` must not collapse the derivation. Under a ⊤ domain
    // "well-typed" means the *result* is not ⊥ — the domain cannot reject
    // anything, so it is no longer where this is decided.
    for (l, r) in [
        (SimpleType::Z, SimpleType::Null),
        (SimpleType::Null, SimpleType::S),
        (SimpleType::Null, SimpleType::Null),
    ] {
        assert!(
            !eq_result(&l, &r).is_empty(),
            "{l} = {r} must stay well-typed"
        );
    }
}

#[test]
fn test_genuinely_incompatible_operands_are_bottom_not_rejected() {
    // Nulls must not blanket-license every comparison: two operands sharing no
    // value are still a type error. It now shows up as a ⊥ *result* rather
    // than a failed domain check, which is what `check_expr` warns on.
    for (l, r) in [
        (SimpleType::Z, SimpleType::S),
        (list(union(SimpleType::Z, SimpleType::Null)), SimpleType::S),
    ] {
        assert!(eq_result(&l, &r).is_empty(), "{l} = {r} must be ⊥");
        // ...and the domain had nothing to do with it.
        assert!(eq_defined(&l, &r), "the ⊤ domain still accepts {l} = {r}");
    }
}

#[test]
fn test_bool_or_null_still_passes_as_a_filter_condition() {
    // A WHERE condition is accepted when it meets `B`. `B ∪ Null` does, so
    // making `=` more precise must not start rejecting every filter.
    let cond = union(SimpleType::B, SimpleType::Null);
    assert!(!SimpleType::meet(&cond, &SimpleType::B).is_empty());
}

// --- The domain is ⊤; the whole decision rides in the result -------------
//
// `=` is total on values, so nothing is ever rejected by the domain check.
// "These operands share nothing" is said by a ⊥ *result*, which mirrors the
// runtime's `EqVerdict::Mismatch`. These pin that shape, since `check_expr`
// consumes the pair as a pass/fail and would not otherwise notice it drifting
// back to an operand-domain formulation.

fn eq_domain(lhs: &SimpleType, rhs: &SimpleType) -> (SimpleType, SimpleType) {
    let (a, b, _) = BinOp::Eq.delta(lhs, rhs);
    (a, b)
}

#[test]
fn test_eq_domain_is_top_and_rejects_nothing() {
    for (l, r) in [
        (SimpleType::Z, SimpleType::Z),
        (SimpleType::Z, SimpleType::Null),
        (SimpleType::Z, SimpleType::S),
        (list(SimpleType::Null), SimpleType::B),
    ] {
        let (a, b) = eq_domain(&l, &r);
        assert_eq!(a, SimpleType::Star, "domain of {l} = {r}");
        assert_eq!(b, SimpleType::Star, "the domain is symmetric");
        // Whatever the operands, they meet ⊤, so the domain never decides.
        assert!(eq_defined(&l, &r), "the domain must never reject {l} = {r}");
    }
}

#[test]
fn test_inconsistent_operands_give_a_bottom_result() {
    // No shared type and neither side is wholly null: nothing to compare, so
    // the *result* is ⊥. That is the type error, and it is what `check_expr`
    // warns on now that the domain cannot.
    assert_eq!(eq_result(&SimpleType::Z, &SimpleType::S), SimpleType::Zero);
    assert_eq!(
        eq_result(&SimpleType::Z, &list(SimpleType::Z)),
        SimpleType::Zero
    );
}

#[test]
fn test_a_wholly_null_operand_is_consistent_with_anything() {
    // `meet(int, Null)` is ⊥ because `Null` is terminal, yet `x.age = null` is
    // well-typed and simply yields null. The consistency test carves that out
    // explicitly.
    let want = union(SimpleType::B, SimpleType::Null);
    assert_eq!(eq_result(&SimpleType::Z, &SimpleType::Null), want);
    assert_eq!(eq_result(&SimpleType::Null, &SimpleType::S), want);
    assert_eq!(eq_result(&SimpleType::Null, &list(SimpleType::Z)), want);
}

#[test]
fn test_a_null_buried_in_a_composite_does_not_make_it_comparable() {
    // The carve-out tests for the type *being* `Null`, not for `has_null`.
    // `[1, null]` and a string share nothing, so this is ⊥ — matching the
    // runtime, where the pair is `EqVerdict::Mismatch` however the list is
    // filled. Reading `has_null` here instead would wrongly call it `B | Null`.
    let t = list(union(SimpleType::Z, SimpleType::Null));
    assert_eq!(eq_result(&t, &SimpleType::S), SimpleType::Zero);
    assert!(
        t.has_null(),
        "the premise: the list really does carry a null"
    );
}

#[test]
fn test_two_nullable_operands_meet_on_their_null_halves() {
    // `int?` vs `string?` share only null, so they are comparable (both can be
    // null, and then the answer is null) even though their non-null halves are
    // disjoint. The meet finds this without needing the carve-out.
    let l = union(SimpleType::Z, SimpleType::Null);
    let r = union(SimpleType::S, SimpleType::Null);
    assert_eq!(eq_result(&l, &r), union(SimpleType::B, SimpleType::Null));
}

#[test]
fn test_nested_lists_without_nulls_stay_definite() {
    // `[[1]] = [[1]]` is a definite bool: the recursion in `has_null` must not
    // manufacture a null just because it descended two levels.
    let t = list(list(SimpleType::Z));
    assert_eq!(eq_result(&t, &t), SimpleType::B);
}
