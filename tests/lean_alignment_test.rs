//! Alignment with the Lean 4 mechanisation of FPPC (`latex/fppcLean.tex`).
//!
//! Each test pins one divergence the Lean development found between the
//! printed rules and the ones that actually make the metatheory go
//! through. Section numbers refer to `lean/PAPER-GAPS.md` as indexed by
//! `latex/fppcLean.tex`.
//!
//! Scope of this file: the four repairs that are local to the lattice and
//! to expression typing. The repetition rule (Trep, §26/32/33/46/64) and
//! the refinement normal form (§37) are larger and live elsewhere.

use std::collections::BTreeMap;

use frogql::elaborate;
use frogql::parser;
use frogql::syntax::query::Query;
use frogql::typing::checker::{TypecheckResult, Typechecker};
use frogql::typing::descriptor_type::DescriptorType;
use frogql::typing::label_type::LabelType;
use frogql::typing::property_type::PropertyType;
use frogql::typing::simple_type::SimpleType;
use frogql::typing::variable_type::{Schema, VariableType};

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

fn check_with(schema: Schema, query: &str) -> (TypecheckResult, Vec<String>, Vec<String>) {
    let ast = parser::parse(query).expect("parse failed");
    let q = Query::pattern_only(ast);
    let q = elaborate::elaborate_query(q);
    let mut tc = Typechecker::new(schema);
    let r = tc.check_query(&q);
    (r, tc.errors.clone(), tc.warnings.clone())
}

fn label(s: &str) -> LabelType {
    LabelType::Label(s.into())
}

fn closed(props: &[(&str, SimpleType)]) -> PropertyType {
    let m: BTreeMap<String, SimpleType> = props
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    PropertyType::Closed(m)
}

fn node_dt(l: LabelType, props: PropertyType) -> VariableType {
    VariableType::Node(DescriptorType::new(l, props))
}

fn directed_edge(
    desc_label: LabelType,
    desc_props: PropertyType,
    left: VariableType,
    right: VariableType,
) -> VariableType {
    VariableType::EdgeDirectional {
        desc: DescriptorType::new(desc_label, desc_props),
        left: Box::new(left),
        right: Box::new(right),
    }
}

/// Two closed edge types out of the same node type, one of which carries
/// `rating` and one of which does not. A union over the two is what makes
/// the missing-key projection observable.
fn split_schema() -> Schema {
    let person = node_dt(label("Person"), closed(&[("name", SimpleType::S)]));
    let plain = directed_edge(label("Plain"), closed(&[]), person.clone(), person.clone());
    let rated = directed_edge(
        label("Rated"),
        closed(&[("rating", SimpleType::Z)]),
        person.clone(),
        person.clone(),
    );
    Schema::from_parts(vec![person], vec![plain, rated])
}

fn union_of(a: SimpleType, b: SimpleType) -> SimpleType {
    SimpleType::Union(Box::new(a), Box::new(b))
}

// =======================================================================
// §8 — a closed record that does not mention `a` projects to Nothing
// =======================================================================

#[test]
fn closed_record_missing_key_projects_to_null() {
    // A closed record says the element definitely has no such key, so the
    // projection is definitely *null*, not an error. Returning ⊥ makes
    // `empty(·)` report as unsatisfiable a projection that merely
    // evaluates to null.
    let r = closed(&[("name", SimpleType::S)]);
    assert_eq!(r.get("nosuch"), SimpleType::Null);
    assert_eq!(r.get("name"), SimpleType::S);
}

#[test]
fn open_record_missing_key_still_projects_to_star() {
    // Unchanged by §8: an open record admits the key, it just does not
    // know its type.
    let m: BTreeMap<String, SimpleType> = BTreeMap::new();
    assert_eq!(PropertyType::Open(m).get("nosuch"), SimpleType::Star);
}

#[test]
fn zero_record_still_projects_to_zero() {
    assert_eq!(PropertyType::Zero.get("nosuch"), SimpleType::Zero);
}

#[test]
fn union_of_edge_types_keeps_the_null_in_the_attribute_type() {
    // The observable consequence. `Plain` has no `rating`, `Rated` has an
    // int one, so the union's projection must be `int | null`. With the
    // ⊥ filler the join swallows the summand that carried the null, the
    // type says plain `int`, and the runtime hands back NULL.
    let plain = directed_edge(
        label("Plain"),
        closed(&[]),
        node_dt(label("Person"), closed(&[("name", SimpleType::S)])),
        node_dt(label("Person"), closed(&[("name", SimpleType::S)])),
    );
    let rated = directed_edge(
        label("Rated"),
        closed(&[("rating", SimpleType::Z)]),
        node_dt(label("Person"), closed(&[("name", SimpleType::S)])),
        node_dt(label("Person"), closed(&[("name", SimpleType::S)])),
    );
    let u = VariableType::join(plain, rated);
    let t = u.get_attribute("rating");
    assert!(
        t.has_null(),
        "a branch without the key contributes a null, got {t}"
    );
}

// =======================================================================
// §38 — Nothing(a) = Nothing
// =======================================================================

#[test]
fn null_variable_type_projects_to_null() {
    // `T(a)` on the null type is the equation the paper's figure omits;
    // the ⊥ filler makes type safety for expressions false, because ⊔
    // then drops exactly the summand that had to carry the null.
    assert_eq!(
        VariableType::Null.get_attribute("anything"),
        SimpleType::Null
    );
}

#[test]
fn optional_binding_keeps_the_null_in_the_attribute_type() {
    // `Γ₁ ⊔ Γ₂` binds a one-sided variable to `T ⊔ Null`, which is the
    // shape OPTIONAL MATCH produces. Projecting through it must still
    // report that the value can be null.
    let person = node_dt(label("Person"), closed(&[("name", SimpleType::S)]));
    let optional = VariableType::join(person, VariableType::Null);
    let t = optional.get_attribute("name");
    assert!(
        t.has_null(),
        "an optionally-bound variable projects to a nullable type, got {t}"
    );
}

// =======================================================================
// empty([T]ₙ) = empty(T) ∧ n > 0 — the list index on variable types
// =======================================================================

#[test]
fn group_with_zero_lower_bound_is_never_empty() {
    // `[T]₀` is inhabited by the empty list whatever T is, so it cannot
    // be empty. This is what keeps `P{0,m}` matching when P matches
    // nothing: zero repetitions is still a match.
    let g = VariableType::group(VariableType::Zero, 0);
    assert!(!g.is_empty());
}

#[test]
fn group_with_positive_lower_bound_inherits_emptiness() {
    let g = VariableType::group(VariableType::Zero, 1);
    assert!(g.is_empty());

    let ok = VariableType::group(
        node_dt(label("Person"), closed(&[("name", SimpleType::S)])),
        1,
    );
    assert!(!ok.is_empty());
}

#[test]
fn group_meet_takes_the_larger_index() {
    // `[T₁]n₁ ⊓ [T₂]n₂ = [T₁ ⊓ T₂]max(n₁,n₂)`. The index is a *minimal*
    // cardinality, so the stronger guarantee is the longer one.
    let person = node_dt(label("Person"), closed(&[("name", SimpleType::S)]));
    let a = VariableType::group(person.clone(), 1);
    let b = VariableType::group(person.clone(), 3);
    let m = VariableType::meet(&a, &b);
    assert_eq!(m, VariableType::group(person, 3));
}

#[test]
fn group_subtyping_is_contravariant_in_the_index() {
    // `[T₁]n₁ <: [T₂]n₂` needs `n₂ ≤ n₁`: a longer guarantee is the
    // stronger type. This is what makes `[T₁ ⊓ T₂]max(n₁,n₂)` a lower
    // bound.
    let person = node_dt(label("Person"), closed(&[("name", SimpleType::S)]));
    let long = VariableType::group(person.clone(), 3);
    let short = VariableType::group(person, 1);
    assert!(VariableType::is_subtype(&long, &short));
    assert!(!VariableType::is_subtype(&short, &long));
}

#[test]
fn zero_lower_bound_repetition_over_an_impossible_inner_is_not_empty() {
    // End to end. The inner edge label is not in the schema, so the inner
    // refines to ⊥; with `{0,2}` the pattern still matches every Person
    // by taking zero laps, so the query must not be pruned.
    let (r, _errs, _warns) = check_with(split_schema(), "(x:Person)-[e:Nosuch]->{0,2}(y)");
    assert!(
        !r.empty,
        "a {{0,m}} repetition over an impossible inner still matches zero laps"
    );
}

#[test]
fn positive_lower_bound_repetition_over_an_impossible_inner_is_empty() {
    // The complement: with `{1,2}` there is no zero-lap escape, so the
    // short-circuit is correct.
    let (r, _errs, _warns) = check_with(split_schema(), "(x:Person)-[e:Nosuch]->{1,2}(y)");
    assert!(
        r.empty,
        "a {{1,m}} repetition over an impossible inner is empty"
    );
}

// =======================================================================
// §47, §48 — the label meet has no special case for ★
// =======================================================================

#[test]
fn label_meet_keeps_the_unknown_conjunct() {
    // `⊓(ℓ₁, ℓ₂) = ℓ₁ & ℓ₂`, always. The clause `⊓(ℓ, ★) = ℓ` is what
    // makes the static gradual guarantee false in two instances.
    let m = LabelType::meet(&LabelType::Star, &label("Person"));
    assert_eq!(
        m,
        LabelType::And(Box::new(LabelType::Star), Box::new(label("Person")))
    );

    let m2 = LabelType::meet(&label("Person"), &LabelType::Star);
    assert_eq!(
        m2,
        LabelType::And(Box::new(label("Person")), Box::new(LabelType::Star))
    );
}

#[test]
fn label_meet_does_not_shortcut_on_consistent_subtyping() {
    // `is_subtype` here is *consistent* subtyping, which is optimistic:
    // `A|B <: A` holds. Using it to shortcut the meet reproduces the ★
    // mistake — `(A|B) ⊓ A` would come out as `A|B`, which is not below
    // `A`.
    let or = LabelType::Or(Box::new(label("A")), Box::new(label("B")));
    let m = LabelType::meet(&or, &label("A"));
    assert_eq!(
        m,
        LabelType::And(Box::new(or), Box::new(label("A"))),
        "the meet must not collapse to either operand"
    );
}

#[test]
fn label_meet_keeps_top_as_an_identity() {
    // `1` is the genuine top of label subtyping, so `ℓ & 1 = ℓ` stays a
    // sound simplification. Only ★ loses its clause.
    assert_eq!(
        LabelType::meet(&LabelType::Top, &label("Person")),
        label("Person")
    );
    assert_eq!(
        LabelType::meet(&label("Person"), &LabelType::Top),
        label("Person")
    );
}

#[test]
fn label_meet_is_still_a_lower_bound_of_both_operands() {
    // The property the ★ clause broke.
    let cases = [
        (LabelType::Star, label("Person")),
        (label("Person"), LabelType::Star),
        (
            LabelType::Or(Box::new(label("A")), Box::new(label("B"))),
            label("A"),
        ),
        (label("A"), label("B")),
    ];
    for (a, b) in cases {
        let m = LabelType::meet(&a, &b);
        assert!(
            LabelType::is_subtype(&m, &a),
            "meet({a}, {b}) = {m} is not below {a}"
        );
        assert!(
            LabelType::is_subtype(&m, &b),
            "meet({a}, {b}) = {m} is not below {b}"
        );
    }
}

// =======================================================================
// (Tas) — `as` types as the meet of the target and the operand
// =======================================================================

#[test]
fn as_types_as_the_meet_with_the_operand() {
    // `e as τ : τ ⊓ τ'`. Returning the target alone lets a value survive
    // a cast to a type that rejects it, which is the `ListCast`
    // counterexample: the nested cast `(x.a as [int]) as [bool]` must
    // come out as `[⊥]`, and today the outer cast reports `[bool]`.
    let (r, _errs, _warns) = check_with(split_schema(), "(x:Person WHERE ((x.name as int) = 1))");
    assert!(
        r.empty,
        "casting a str-typed attribute to int has an empty type, so the filter is (Tfail)"
    );
}

#[test]
fn as_to_a_compatible_type_still_types() {
    // The complement: a cast whose target meets the operand keeps the
    // pattern alive.
    let (r, _errs, _warns) = check_with(split_schema(), "(x:Person WHERE ((x.name as str) = 'a'))");
    assert!(!r.empty, "a str-to-str cast is fine");
}

#[test]
fn as_meets_a_union_operand_down_to_the_named_branch() {
    // `(int|str) as int` is `int`, not `int|str`: the cast narrows.
    let t = SimpleType::meet(&SimpleType::Z, &union_of(SimpleType::Z, SimpleType::S));
    assert_eq!(t, SimpleType::Z);
}

// =======================================================================
// §40 — `Null ⇒ τ` holds exactly when Nothing <: τ
// =======================================================================

#[test]
fn null_inhabits_the_null_type() {
    use frogql::model::value::Value;
    use frogql::syntax::expr::Expr;
    assert!(Expr::value_is_type(&Value::Null, &SimpleType::Null));
    assert!(Expr::value_is_type(&Value::Null, &SimpleType::Star));
    assert!(Expr::value_is_type(
        &Value::Null,
        &union_of(SimpleType::Z, SimpleType::Null)
    ));
    assert!(!Expr::value_is_type(&Value::Null, &SimpleType::Z));
}

// =======================================================================
// The diagnostic §8 must not cost: a key a closed record cannot have is
// still worth reporting, it is just not an *empty* type any more.
// =======================================================================

#[test]
fn missing_key_on_a_closed_record_still_warns() {
    // The projection types `null` now instead of `⊥`, so the old
    // "attribute is empty" test no longer fires. The typo is still a typo,
    // so the warning has to key off the null instead.
    let (_r, _errs, warns) = check_with(split_schema(), "(a)-[r:Rated]->(b) WHERE r.nosuch = 1");
    assert!(
        warns.iter().any(|w| w.contains("nosuch")),
        "a key the closed record cannot hold should still warn, got {warns:?}"
    );
}

#[test]
fn present_key_on_a_closed_record_does_not_warn() {
    let (_r, _errs, warns) = check_with(split_schema(), "(a)-[r:Rated]->(b) WHERE r.rating = 1");
    assert!(
        !warns.iter().any(|w| w.contains("rating")),
        "a key the record does hold must not warn, got {warns:?}"
    );
}

#[test]
fn missing_key_on_an_open_record_does_not_warn() {
    // An open record admits any key, so there is nothing to report.
    let open_person = VariableType::Node(DescriptorType::new(
        label("Person"),
        PropertyType::Open(BTreeMap::new()),
    ));
    let schema = Schema::from_parts(vec![open_person], vec![]);
    let (_r, _errs, warns) = check_with(schema, "(x:Person WHERE (x.nosuch = 1))");
    assert!(
        !warns.iter().any(|w| w.contains("nosuch")),
        "an open record admits the key, got {warns:?}"
    );
}

// =======================================================================
// §37 — the refinement fold skips an empty contribution, and the path
// meet normalises, so `empty(P)` and `P = ⊥` are the same thing on every
// type the system derives.
// =======================================================================

/// A schema whose node type has a *consistently* compatible but
/// empty-meeting property. Consistent subtyping is optimistic, so
/// `[⊥] | int <: [bool]` holds by the `⊥ <: bool` branch, while the meet
/// is `[⊥]` — non-empty as a term, empty as a type. This is the shape the
/// fold has to drop.
fn optimistic_schema() -> Schema {
    let list_zero = SimpleType::List(Box::new(SimpleType::Zero));
    let person = node_dt(
        label("Person"),
        closed(&[("a", union_of(list_zero, SimpleType::Z))]),
    );
    Schema::from_parts(vec![person], vec![])
}

/// The query-side type: `(x:Person {a is [bool]})`.
fn probe_node() -> VariableType {
    let mut props = BTreeMap::new();
    props.insert("a".to_string(), SimpleType::List(Box::new(SimpleType::B)));
    VariableType::Node(DescriptorType::new(
        label("Person"),
        PropertyType::Open(props),
    ))
}

#[test]
fn refine_drops_an_empty_contribution() {
    // Without the `¬empty(T ⊓ T')` side condition the fold keeps a branch
    // that describes nothing, and the result is a type that `is_empty`
    // calls empty while it is not syntactically `⊥`. Everything
    // downstream that tests `== Zero` then disagrees with `is_empty`.
    let refined = VariableType::refine(&optimistic_schema(), &probe_node());
    assert_eq!(
        refined,
        VariableType::Zero,
        "an empty contribution must not survive the fold"
    );
}

#[test]
fn refine_keeps_a_non_empty_contribution() {
    // The complement: a branch that does describe something survives.
    let schema = Schema::from_parts(
        vec![node_dt(label("Person"), closed(&[("a", SimpleType::S)]))],
        vec![],
    );
    let mut props = BTreeMap::new();
    props.insert("a".to_string(), SimpleType::S);
    let probe = VariableType::Node(DescriptorType::new(
        label("Person"),
        PropertyType::Open(props),
    ));
    let refined = VariableType::refine(&schema, &probe);
    assert_ne!(refined, VariableType::Zero);
    assert!(!refined.is_empty());
}

#[test]
fn refine_makes_emptiness_and_bottom_coincide() {
    // The invariant §37 buys, stated directly: nothing the refinement
    // returns is empty without being `⊥`.
    for (schema, probe) in [
        (optimistic_schema(), probe_node()),
        (split_schema(), VariableType::node_star()),
    ] {
        let refined = VariableType::refine(&schema, &probe);
        assert_eq!(
            refined.is_empty(),
            refined == VariableType::Zero,
            "refined {refined} disagrees about emptiness"
        );
    }
}

#[test]
fn path_meet_normalises_an_empty_result_to_bottom() {
    use frogql::typing::path_type::PathType;
    // Same invariant one level up. `⌊·⌋nf` sends an empty path type to
    // `⊥`, so a `Concat` whose meet describes nothing produces `⊥` rather
    // than a live-looking `Edge` with a dead node inside it.
    let dead = DescriptorType::new(label("Person"), PropertyType::Zero);

    // The `(Edge, Node)` arm already normalises: it folds through
    // `refine_to_nodes`, which returns nothing and unions to `⊥`.
    let edge_then_node = PathType::meet(
        &Schema::star(),
        &PathType::Edge(frogql::typing::path_type::EdgePathType {
            p1: Box::new(PathType::node(DescriptorType::star())),
            n2: frogql::typing::path_type::NodePathType::new(dead.clone()),
        }),
        &PathType::node(dead.clone()),
    );
    assert_eq!(edge_then_node, PathType::Zero);

    // The `(_, Edge)` arm is the one that needs `⌊·⌋nf`: it rebuilds an
    // `Edge` around a dead prefix and hands back a live-looking term that
    // `is_unsatisfiable` nonetheless calls dead.
    let node_then_edge = PathType::meet(
        &Schema::star(),
        &PathType::node(dead),
        &PathType::Edge(frogql::typing::path_type::EdgePathType {
            p1: Box::new(PathType::node(DescriptorType::star())),
            n2: frogql::typing::path_type::NodePathType::new(DescriptorType::star()),
        }),
    );
    assert!(node_then_edge.is_unsatisfiable());
    assert_eq!(
        node_then_edge,
        PathType::Zero,
        "an unsatisfiable meet normalises to ⊥"
    );
}

#[test]
fn path_meet_keeps_a_satisfiable_result() {
    use frogql::typing::path_type::PathType;
    let schema = Schema::star();
    let p1 = PathType::Edge(frogql::typing::path_type::EdgePathType {
        p1: Box::new(PathType::node(DescriptorType::star())),
        n2: frogql::typing::path_type::NodePathType::new(DescriptorType::star()),
    });
    let p2 = PathType::node(DescriptorType::star());
    let m = PathType::meet(&schema, &p1, &p2);
    assert_ne!(m, PathType::Zero);
    assert!(!m.is_unsatisfiable());
}

// =======================================================================
// (Trep) — §26, §32, §33, §46, §64
//
// Two changes, both forced. The premise reads the length off the
// *pattern* and is guarded at `n = 0`; and the result is the body's
// endpoints gated by an emptiness test, not `Path^min(n,2)`.
// =======================================================================

use frogql::syntax::path_pattern::PathPattern;

fn pat(q: &str) -> PathPattern {
    parser::parse(q).expect("parse failed")
}

// ----- len(·) on the pattern ------------------------------------------

#[test]
fn min_len_counts_edges_syntactically() {
    assert_eq!(pat("(x)").min_len(), 0);
    assert_eq!(pat("()-[]->()").min_len(), 1);
    assert_eq!(pat("()<-[]-()").min_len(), 1);
    assert_eq!(pat("()~[]~()").min_len(), 1);
    assert_eq!(pat("()-[]-()").min_len(), 1);
    assert_eq!(pat("()-[]->()-[]->()").min_len(), 2);
}

#[test]
fn min_len_of_a_union_is_the_minimum() {
    assert_eq!(pat("(()-[]->()) | (x)").min_len(), 0);
    assert_eq!(pat("(()-[]->()) | (()-[]->()-[]->())").min_len(), 1);
}

#[test]
fn min_len_of_a_repetition_scales_by_the_lower_bound() {
    assert_eq!(pat("(()-[]->()){3,5}").min_len(), 3);
    assert_eq!(pat("(()-[]->()){0,5}").min_len(), 0);
    assert_eq!(pat("(x){3,5}").min_len(), 0);
}

#[test]
fn min_len_ignores_the_schema() {
    // The whole point of reading it off the pattern: `len(·)` carries no
    // schema and is invariant under precision, which is what makes the
    // (Trep) case of the static gradual guarantee a rewrite. A label the
    // schema rejects still contributes its edge.
    assert_eq!(pat("()-[:Nosuch]->()").min_len(), 1);
}

// ----- the premise ----------------------------------------------------

#[test]
fn edge_body_with_an_unknown_label_does_not_report_a_length_problem() {
    // The observable difference between reading `len` off the pattern and
    // off the path type. `-[e:Nosuch]->` refines to ⊥, so its *path type*
    // has no edges and the old gate complained about the length; the
    // *pattern* plainly traverses one edge. The repetition is empty, but
    // it is empty because the label is unknown, not because the body has
    // zero length.
    let (r, _errs, warns) = check_with(split_schema(), "(x:Person)-[e:Nosuch]->{1,2}(y)");
    assert!(
        r.empty,
        "an unknown edge label still empties the repetition"
    );
    assert!(
        !warns.iter().any(|w| w.contains("length")),
        "the pattern traverses an edge, so there is no length problem: {warns:?}"
    );
}

#[test]
fn node_body_with_a_positive_lower_bound_is_rejected() {
    // `len(patts) = 0` and `n ≥ 1`: every lap would have to consume an
    // edge and the body consumes none, so no `i` in `[n, m]` produces a
    // result and the pattern returns nothing in every graph. The premise
    // `n = 0 ∨ len(patts) > 0` rejects it outright rather than deriving a
    // type for a query that cannot match.
    let (_r, errs, _warns) = check_with(split_schema(), "(x:Person){1,3}");
    assert!(
        !errs.is_empty(),
        "a zero-length body under a positive lower bound has no derivation"
    );
}

#[test]
fn node_body_with_a_zero_lower_bound_is_accepted() {
    // The guard. At `n = 0` the empty path is a branch of its own, so the
    // repetition is perfectly typeable and matches by taking zero laps.
    let (r, errs, _warns) = check_with(split_schema(), "(x:Person){0,3}");
    assert!(errs.is_empty(), "errors: {errs:?}");
    assert!(!r.empty);
}

#[test]
fn questioned_is_a_zero_lower_bound() {
    // `P?` is `P{0,1}`, so the guard covers it too.
    let (r, errs, _warns) = check_with(split_schema(), "(x:Person)?");
    assert!(errs.is_empty(), "errors: {errs:?}");
    assert!(!r.empty);
}

// ----- the result type ------------------------------------------------

/// Does this path type have a branch that traverses an edge?
fn has_edge_branch(p: &frogql::typing::path_type::PathType) -> bool {
    use frogql::typing::path_type::PathType;
    match p {
        PathType::Edge(_) => true,
        PathType::Union(a, b) => has_edge_branch(a) || has_edge_branch(b),
        _ => false,
    }
}

fn has_node_branch(p: &frogql::typing::path_type::PathType) -> bool {
    use frogql::typing::path_type::PathType;
    match p {
        PathType::Node(_) => true,
        PathType::Union(a, b) => has_node_branch(a) || has_node_branch(b),
        _ => false,
    }
}

#[test]
fn zero_lower_bound_keeps_both_branches() {
    // `Path^min(n,2)` at `n = 0` is just the empty path, so the type
    // forgets that the repetition can also traverse edges and a matched
    // three-edge path does not conform to it. The repaired rule joins the
    // empty-path branch with the body's endpoints.
    let (r, errs, _warns) = check_with(split_schema(), "(x:Person)-[e:Rated]->{0,3}(y:Person)");
    assert!(errs.is_empty(), "errors: {errs:?}");
    assert!(
        has_node_branch(&r.path),
        "the zero-lap branch must be in the type: {:?}",
        r.path
    );
    assert!(
        has_edge_branch(&r.path),
        "the edge-traversing branch must be in the type: {:?}",
        r.path
    );
}

#[test]
fn positive_lower_bound_has_no_empty_path_branch() {
    let (r, errs, _warns) = check_with(split_schema(), "(x:Person)-[e:Rated]->{1,3}(y:Person)");
    assert!(errs.is_empty(), "errors: {errs:?}");
    assert!(has_edge_branch(&r.path), "{:?}", r.path);
    assert!(
        !has_node_branch(&r.path),
        "with n >= 1 the empty path is not a branch: {:?}",
        r.path
    );
}

#[test]
fn the_gate_still_empties_an_unreachable_repetition() {
    // `Rated` goes Person -> Person, so a two-lap chain exists and the
    // gate admits it.
    let (r, errs, _warns) = check_with(split_schema(), "(x:Person)-[e:Rated]->{2,5}(y:Person)");
    assert!(errs.is_empty(), "errors: {errs:?}");
    assert!(!r.empty);
}

#[test]
fn the_gate_empties_a_repetition_whose_second_lap_is_impossible() {
    // `Leaf` goes Person -> Terminal, and nothing leaves Terminal, so no
    // two-lap chain exists and `{2,5}` is empty — while `{1,5}` is not.
    let person = node_dt(label("Person"), closed(&[("name", SimpleType::S)]));
    let terminal = node_dt(label("Terminal"), closed(&[("name", SimpleType::S)]));
    let leaf = directed_edge(label("Leaf"), closed(&[]), person.clone(), terminal.clone());
    let schema = Schema::from_parts(vec![person, terminal], vec![leaf]);

    let (r2, _e, _w) = check_with(schema.clone(), "(x)-[e:Leaf]->{2,5}(y)");
    assert!(r2.empty, "no two-lap chain exists: {:?}", r2.path);

    let (r1, _e, _w) = check_with(schema, "(x)-[e:Leaf]->{1,5}(y)");
    assert!(!r1.empty, "a one-lap chain does exist");
}

#[test]
fn an_unbounded_upper_bound_costs_no_iteration() {
    // The gate clamps at `min(m, 2)`, so `{1,}` computes exactly the same
    // powers as `{1,2}` and an unbounded upper bound is free.
    let (a, ea, _) = check_with(split_schema(), "ALL SHORTEST (x)-[e:Rated]->{1,}(y)");
    let (b, eb, _) = check_with(split_schema(), "(x)-[e:Rated]->{1,2}(y)");
    assert!(ea.is_empty(), "errors: {ea:?}");
    assert!(eb.is_empty(), "errors: {eb:?}");
    assert_eq!(a.empty, b.empty);
}
