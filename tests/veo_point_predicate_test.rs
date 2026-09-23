//! A variable carrying a point equality must bind early even when it sits
//! in a single triple.
//!
//! The VEO holds "lonely" variables — those in one triple — to the end,
//! because such a variable adds no join constraint and binding it early
//! multiplies the search without narrowing it. A point predicate inverts
//! that: the variable collapses to one value which, through its triple,
//! constrains everything sharing it.
//!
//! Until this was fixed the selectivity was ranked only *within* the
//! lonely group, where it could not help. Measured on a 459 127-node
//! OpenStreetMap graph with the secondary-index fold off, a five-variable
//! chain anchored by one `WHERE m.nombre = '…'` took 1 045 117 candidate
//! visits and 27.8 s where the right order takes 451 and 0.19 s — and an
//! index happening to exist hid it completely, which is why it was first
//! reported as "the missing index costs 30 seconds".
//!
//! The assertions are on the *order*, not on a time: a timing test would
//! be flaky, and the order is the thing that was wrong.

use frogql::runtime::ltj::veo::{order_is_forced, Veo, VeoSimple, POINT_WEIGHT};

/// `(var_id, weight, is_lonely)` — the shape the VEO is built from.
const LABEL: usize = 1000;
const UNFILTERED: usize = 4000;

#[test]
fn a_lonely_point_predicate_binds_before_unfiltered_joined_vars() {
    // var 0: lonely, but pinned by an equality — the shape of
    // `(m:Lugar WHERE m.nombre = '…')` at the end of a chain.
    // vars 1..3: joined to each other, label filter only.
    let order = VeoSimple::new(vec![
        (0, POINT_WEIGHT, true),
        (1, LABEL, false),
        (2, LABEL, false),
        (3, UNFILTERED, false),
    ]);
    assert_eq!(
        order.var_at(0),
        0,
        "the equality-pinned variable must bind first, not last"
    );
}

#[test]
fn a_lonely_variable_without_a_point_predicate_is_still_held_back() {
    // The rule the fix must not erase: an unconstrained lonely variable
    // still belongs at the end.
    let order = VeoSimple::new(vec![
        (0, UNFILTERED, true),
        (1, LABEL, false),
        (2, LABEL, false),
    ]);
    assert_eq!(
        order.var_at(2),
        0,
        "a lonely variable with no point predicate stays last"
    );
    // And a label filter does not earn the promotion either.
    let order = VeoSimple::new(vec![(0, LABEL, true), (1, UNFILTERED, false)]);
    assert_eq!(order.var_at(1), 0, "a label filter is not a point lookup");
}

#[test]
fn among_several_the_lightest_still_wins() {
    let order = VeoSimple::new(vec![
        (0, UNFILTERED, false),
        (1, POINT_WEIGHT, true),
        (2, LABEL, false),
    ]);
    assert_eq!(order.var_at(0), 1);
    assert_eq!(order.var_at(1), 2);
    assert_eq!(order.var_at(2), 0);
}

/// `order_is_forced` short-circuits the adaptive VEO to the static one.
/// It runs once per LTJ run, and a correlated subquery issues one per
/// outer row — 148 323 of them in LDBC IC8 — so reporting "not forced"
/// for an order that cannot actually move costs 13% there for nothing.
#[test]
fn a_point_variable_does_not_make_the_order_look_free() {
    // One point variable + one free variable: the point one sorts first
    // whatever the measured sizes say, so the order is decided.
    assert!(order_is_forced(&[
        (0, POINT_WEIGHT, true),
        (1, LABEL, false)
    ]));
    assert!(order_is_forced(&[
        (0, POINT_WEIGHT, false),
        (1, LABEL, false)
    ]));
    // Two genuinely free variables can still be permuted.
    assert!(!order_is_forced(&[
        (0, LABEL, false),
        (1, UNFILTERED, false),
        (2, LABEL, false)
    ]));
}
