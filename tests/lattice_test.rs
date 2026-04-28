//! Spec-driven proptest suite for the typing lattice.
//!
//! The spec is `docs/rules.md` (FPPC formal rules). Section references
//! in this file (`§3.1`, `§4.2`, etc.) point there. The goal: if
//! `rules.md` changes, this file changes; if the implementation drifts
//! from the spec, a property fails — and the failure message names the
//! rule that was violated.
//!
//! ## Three equivalence relations
//!
//! rules.md / FPPC use three relations between types:
//!
//! - **Subtyping** `A ≤ B` — plausibility-based (§3): "it is *possible*
//!   for A to match B".
//! - **Consistency** `A ~ B` — `A ≤ B AND B ≤ A` (§3): "the gradual
//!   version of equality".
//! - **Precision** `A ⊑ B` — information content (§9): "A is more
//!   informative / less fuzzy than B".
//!
//! Consistency is too loose to serve as a test equivalence:
//!
//! ```text
//! is_subtype(Z, Star) = true       // §3.2 Star bidirectional
//! is_subtype(Star, Z) = true
//! ⇒ Z ~ Star (consistent)          // …but Z and Star are not the same type
//!
//! is_subtype(A, Or(A, B)) = true       // §3.1 Or-RHS plausibility
//! is_subtype(Or(A, B), A) = true       // §3.1 Or-LHS plausibility
//! ⇒ A ~ Or(A, B) (consistent)          // …but Or(A, B) is wider than A
//! ```
//!
//! So a regression where `meet(A, B)` collapses to `Star` or to one of
//! its operands slips through any consistency check. We use **canonical
//! form equality** (`canon_eq`) as a tighter relation for tests where
//! shape genuinely matters (symmetries, distributions, refinements).
//! For tests where the impl is allowed to pick any representative of an
//! equivalence class — `LabelType::meet` returning either operand when
//! they're consistent — we use `assert_consistent!` plus hand-picked
//! `meet_locks` to backstop against Star-collapse regressions.
//!
//! `canon_eq` is closer to **non-gradual equality**: same shape modulo
//! commutativity / idempotence / identity. It has its own caveat — see
//! the `canon` module's drift discussion.
//!
//! ## Layers of coverage
//!
//! 1. **Algebraic invariants** — subtype reflexivity, meet idempotence,
//!    greatest-lower-bound, commutativity, identity / absorbing elements.
//! 2. **Spec rules from `rules.md`** — direct encodings, organized by
//!    section. Each property names the rule it validates.
//! 3. **Schema-diverse refinement** — refines against generated schemas
//!    (not just `Schema::star()`). Restrictive-schema dispatch is where
//!    bugs like the `EdgeDir::Any → directional-only` port regression
//!    (commit `9ec4975`) live; lattice-level proptests against custom
//!    schemas exercise that surface.
//!
//! ## Variants intentionally NOT generated
//!
//! `LabelType::Top`, `LabelType::Empty`, `LabelType::Neg` are dead code
//! in gqlite (no parser/elaborator/runtime constructs them) and their
//! intended semantics is unsettled. Adding them to the generators here
//! would lock in behavior that is likely to change. Re-include them once
//! semantics are pinned down — the spec-rule tests below are structured
//! to accept new variants.

use std::collections::BTreeMap;

use proptest::prelude::*;

use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::path_type::{EdgeDir, EdgePathType, NodePathType, PathType};
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::simple_type::SimpleType;
use gqlrust::typing::variable_type::{Schema, VariableType};

// =======================================================================
// Canonicalization
// =======================================================================
//
// `canon` does four things to give a spec-aligned canonical form:
//
//  1. **Collapse semantically empty types to ⊥.** `Group(Zero)`,
//     `Record({a: Zero})`, `Union(Zero, Zero)`, etc. all reduce to
//     the bottom representative. Per rules.md §3.2, these are all
//     consistent with ⊥, and the spec treats them interchangeably.
//  2. **Sort** children of commutative operators (And, Or, Union) by
//     `Debug` repr. Pure algebra — commutativity is a true lattice law.
//  3. **Dedup** adjacent equal children (idempotence: `A & A = A`).
//  4. **Drop identity elements from joins/meets** — bottom from Union,
//     Star from And. Lattice identities, also what gqlite's `union/meet`
//     drops in its match arms.
//
// **Drift coupling.** Steps (1) and (4) depend on gqlite's predicates:
// `is_empty` (SimpleType, PropertyType, VariableType) and
// `is_unsatisfiable` (PathType). If those predicates regress, canon
// silently mis-equates types. The defense is in two layers:
//   - `meet_locks` / `normalization_locks` pin specific behaviors of
//     `union/meet/is_empty/is_unsatisfiable` directly.
//   - `equivalence_relations` documents the looseness in code so a
//     future reader understands what canon promises (and doesn't).
//
// **What canon does NOT do.** No absorption (`Union(Star, X) → Star`),
// no Star-from-Or removal. Going further would create false positives
// against gqlite's actual representation.

mod canon {
    use super::*;

    fn flatten_simple_union(t: &SimpleType, out: &mut Vec<SimpleType>) {
        if let SimpleType::Union(a, b) = t {
            flatten_simple_union(a, out);
            flatten_simple_union(b, out);
        } else {
            out.push(simple(t));
        }
    }

    pub fn simple(t: &SimpleType) -> SimpleType {
        // Any semantically-empty SimpleType collapses to Zero. Per
        // §3.2 `⊥ ≤ τ AND τ ≤ ⊥` for empty τ — they're consistent.
        // Concretely: `Group(Zero)`, `Record({a: Zero})`,
        // `Union(Zero, Zero)`, etc. all reduce to Zero. Without this,
        // canon would be internally inconsistent (drops Group(Zero)
        // from a Union, but keeps it standalone).
        if t.is_empty() {
            return SimpleType::Zero;
        }
        match t {
            SimpleType::Union(_, _) => {
                let mut leaves = Vec::new();
                flatten_simple_union(t, &mut leaves);
                // Drop semantically-empty leaves. Per rules.md §3.2,
                // `⊥ ≤ τ`, and Union is the gradual join — bottom is
                // the identity. We drop anything `is_empty` (Zero,
                // Group(Zero), Record({a: Zero}), Union(Zero, Zero), …)
                // not just literal Zero, because the spec treats them
                // as semantically equivalent. NOTE: gqlite's `union`
                // impl only drops literal Zero — meet on nested types
                // can produce Group(Zero) terms in the result, which
                // canon-eq treats as redundant per the spec.
                leaves.retain(|l| !l.is_empty());
                leaves.sort_by_key(|l| format!("{l:?}"));
                leaves.dedup();
                leaves
                    .into_iter()
                    .reduce(|acc, x| SimpleType::Union(Box::new(acc), Box::new(x)))
                    .unwrap_or(SimpleType::Zero)
            }
            SimpleType::List(inner) => SimpleType::List(Box::new(simple(inner))),
            SimpleType::Group(inner) => SimpleType::Group(Box::new(simple(inner))),
            SimpleType::Record(m) => {
                let canon: BTreeMap<String, SimpleType> =
                    m.iter().map(|(k, v)| (k.clone(), simple(v))).collect();
                SimpleType::Record(canon)
            }
            // Atoms / Star / Zero
            other => other.clone(),
        }
    }

    fn flatten_label_and(t: &LabelType, out: &mut Vec<LabelType>) {
        if let LabelType::And(a, b) = t {
            flatten_label_and(a, out);
            flatten_label_and(b, out);
        } else {
            out.push(label(t));
        }
    }

    fn flatten_label_or(t: &LabelType, out: &mut Vec<LabelType>) {
        if let LabelType::Or(a, b) = t {
            flatten_label_or(a, out);
            flatten_label_or(b, out);
        } else {
            out.push(label(t));
        }
    }

    pub fn label(t: &LabelType) -> LabelType {
        match t {
            LabelType::And(_, _) => {
                let mut leaves = Vec::new();
                flatten_label_and(t, &mut leaves);
                // Drop Star — gqlite's `LabelType::meet(Star, x) = x`
                // (Star is the identity for And/meet).
                leaves.retain(|l| !matches!(l, LabelType::Star));
                leaves.sort_by_key(|l| format!("{l:?}"));
                leaves.dedup();
                leaves
                    .into_iter()
                    .reduce(|acc, x| LabelType::And(Box::new(acc), Box::new(x)))
                    .unwrap_or(LabelType::Star)
            }
            LabelType::Or(_, _) => {
                let mut leaves = Vec::new();
                flatten_label_or(t, &mut leaves);
                leaves.sort_by_key(|l| format!("{l:?}"));
                leaves.dedup();
                leaves
                    .into_iter()
                    .reduce(|acc, x| LabelType::Or(Box::new(acc), Box::new(x)))
                    .expect("flatten yields ≥1 leaf for an Or node")
            }
            LabelType::Neg(inner) => LabelType::Neg(Box::new(label(inner))),
            // Label / Star / Top / Empty
            other => other.clone(),
        }
    }

    pub fn property(t: &PropertyType) -> PropertyType {
        if t.is_empty() {
            return PropertyType::Zero;
        }
        match t {
            PropertyType::Open(m) => {
                let canon: BTreeMap<String, SimpleType> =
                    m.iter().map(|(k, v)| (k.clone(), simple(v))).collect();
                PropertyType::Open(canon)
            }
            PropertyType::Closed(m) => {
                let canon: BTreeMap<String, SimpleType> =
                    m.iter().map(|(k, v)| (k.clone(), simple(v))).collect();
                PropertyType::Closed(canon)
            }
            PropertyType::Zero => PropertyType::Zero,
        }
    }

    pub fn descriptor(d: &DescriptorType) -> DescriptorType {
        DescriptorType::new(label(&d.label), property(&d.props))
    }

    fn flatten_variable_union(t: &VariableType, out: &mut Vec<VariableType>) {
        if let VariableType::Union(a, b) = t {
            flatten_variable_union(a, out);
            flatten_variable_union(b, out);
        } else {
            out.push(variable(t));
        }
    }

    pub fn variable(t: &VariableType) -> VariableType {
        if t.is_empty() {
            return VariableType::Zero;
        }
        match t {
            VariableType::Union(_, _) => {
                let mut leaves = Vec::new();
                flatten_variable_union(t, &mut leaves);
                // Drop semantically-empty leaves (Zero, Group(Zero),
                // Node-with-empty-descriptor, etc.). See `canon::simple`
                // for the spec rationale.
                leaves.retain(|v| !v.is_empty());
                leaves.sort_by_key(|v| format!("{v:?}"));
                leaves.dedup();
                leaves
                    .into_iter()
                    .reduce(|acc, x| VariableType::Union(Box::new(acc), Box::new(x)))
                    .unwrap_or(VariableType::Zero)
            }
            VariableType::Node(d) => VariableType::Node(descriptor(d)),
            VariableType::EdgeDirectional { desc, left, right } => VariableType::EdgeDirectional {
                desc: descriptor(desc),
                left: Box::new(variable(left)),
                right: Box::new(variable(right)),
            },
            VariableType::EdgeNonDirectional { desc, left, right } => {
                VariableType::EdgeNonDirectional {
                    desc: descriptor(desc),
                    left: Box::new(variable(left)),
                    right: Box::new(variable(right)),
                }
            }
            VariableType::Group(inner) => VariableType::Group(Box::new(variable(inner))),
            VariableType::Zero => VariableType::Zero,
        }
    }

    fn flatten_path_union(t: &PathType, out: &mut Vec<PathType>) {
        if let PathType::Union(a, b) = t {
            flatten_path_union(a, out);
            flatten_path_union(b, out);
        } else {
            out.push(path(t));
        }
    }

    pub fn path(t: &PathType) -> PathType {
        if t.is_unsatisfiable() {
            return PathType::Zero;
        }
        match t {
            PathType::Union(_, _) => {
                let mut leaves = Vec::new();
                flatten_path_union(t, &mut leaves);
                // Drop semantically-unsatisfiable leaves. PathType has
                // `is_unsatisfiable` (the bottom check) rather than
                // `is_empty` (which on PathType means "no edges").
                leaves.retain(|p| !p.is_unsatisfiable());
                leaves.sort_by_key(|p| format!("{p:?}"));
                leaves.dedup();
                leaves
                    .into_iter()
                    .reduce(|acc, x| PathType::Union(Box::new(acc), Box::new(x)))
                    .unwrap_or(PathType::Zero)
            }
            PathType::Node(n) => PathType::Node(NodePathType::new(descriptor(&n.desc))),
            PathType::Edge(e) => PathType::Edge(EdgePathType {
                p1: Box::new(path(&e.p1)),
                n2: NodePathType::new(descriptor(&e.n2.desc)),
            }),
            PathType::Zero => PathType::Zero,
        }
    }
}

/// Canonical-form equality. Closer to the **non-gradual** equality you'd
/// have without plausibility — two types are `canon_eq` iff they have
/// the same shape after the canonicalization steps in the `canon`
/// module (collapse-empty, sort commutative children, dedup, drop
/// identity elements).
///
/// Tighter than `consistent` (mutual-subtype). Catches Star-collapse
/// regressions that consistency would hide. Use where the output's
/// SHAPE genuinely matters (symmetries, distributions, refinements).
///
/// **Drift caveat.** canon's collapse-empty step depends on gqlite's
/// `is_empty` / `is_unsatisfiable` predicates — not on `union`/`meet`'s
/// internal normalization. If those predicates regress, canon mis-
/// classifies. The `meet_locks` / `normalization_locks` modules pin
/// the impl behaviors directly so a predicate regression is caught at
/// a different layer.
macro_rules! assert_canon_eq {
    ($kind:ident, $lhs:expr, $rhs:expr $(, $msg:tt)?) => {{
        let lhs = $lhs;
        let rhs = $rhs;
        let cl = canon::$kind(&lhs);
        let cr = canon::$kind(&rhs);
        prop_assert_eq!(&cl, &cr $(, $msg)?);
    }};
}

/// Consistency (rules.md §3 — `A ~ B` in paper notation): `A ≤ B AND
/// B ≤ A`. The paper calls this "the gradual version of equality":
/// two types are consistent if they could plausibly match.
///
/// **Loose by design.** Under plausibility, `Star ≤ τ` and `τ ≤ Star`
/// for any τ — so anything is consistent with `Star`. A regression
/// where meet collapses to `Star` would slip past consistency tests.
/// Pair with hand-picked `meet_locks` to catch that.
///
/// Use this for properties on types where gqlite's impl is allowed to
/// return any representative of an equivalence class (e.g., `LabelType
/// ::meet` returning either operand when both are mutually consistent).
macro_rules! assert_consistent {
    ($subtype_path:path, $lhs:expr, $rhs:expr $(, $msg:tt)?) => {{
        let lhs = $lhs;
        let rhs = $rhs;
        prop_assert!($subtype_path(&lhs, &rhs) && $subtype_path(&rhs, &lhs)
            $(, $msg)?);
    }};
}

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
    leaf.prop_recursive(3, 32, 4, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone())
                .prop_map(|(a, b)| SimpleType::Union(Box::new(a), Box::new(b))),
            inner.clone().prop_map(|t| SimpleType::List(Box::new(t))),
            inner.clone().prop_map(|t| SimpleType::Group(Box::new(t))),
            prop::collection::btree_map("[a-c]", inner, 0..3).prop_map(SimpleType::Record),
        ]
    })
}

fn arb_label_atom() -> impl Strategy<Value = LabelType> {
    prop_oneof![Just(LabelType::Star), "[A-D]".prop_map(LabelType::Label),]
}

fn arb_label_type() -> impl Strategy<Value = LabelType> {
    arb_label_atom().prop_recursive(3, 16, 2, |inner| {
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

/// Strictly Node-variant — used for `left`/`right` of edge variants.
fn arb_variable_node() -> impl Strategy<Value = VariableType> {
    arb_descriptor_type().prop_map(VariableType::Node)
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
    leaf.prop_recursive(3, 16, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone())
                .prop_map(|(a, b)| VariableType::Union(Box::new(a), Box::new(b))),
            inner.prop_map(|t| VariableType::Group(Box::new(t))),
        ]
    })
}

fn arb_path_node() -> impl Strategy<Value = PathType> {
    arb_descriptor_type().prop_map(PathType::node)
}

fn arb_path_type() -> impl Strategy<Value = PathType> {
    let leaf = prop_oneof![Just(PathType::Zero), arb_path_node(),];
    leaf.prop_recursive(3, 16, 2, |inner| {
        prop_oneof![
            (inner.clone(), arb_descriptor_type()).prop_map(|(p1, n2)| {
                PathType::Edge(EdgePathType {
                    p1: Box::new(p1),
                    n2: NodePathType::new(n2),
                })
            }),
            (inner.clone(), inner).prop_map(|(a, b)| PathType::Union(Box::new(a), Box::new(b))),
        ]
    })
}

/// `VariableType` restricted to refinable shapes under `Schema::star()`:
/// Node / Edge with non-empty descriptors. Avoids the post-filter
/// reject-loop the generic generator would force.
fn arb_refinable_variable() -> BoxedStrategy<VariableType> {
    let refinable_desc = arb_label_atom()
        .prop_map(|l| DescriptorType::new(l, PropertyType::open_empty()))
        .boxed();
    let node_endpoint = refinable_desc.clone().prop_map(VariableType::Node).boxed();

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
    .boxed()
}

// =======================================================================
// Constructive generators (premise satisfied by construction)
// =======================================================================
//
// Conditional properties of the form `assume(P) ⇒ Q` waste cases when
// `P` is rare under random generation. These generators construct
// inputs that satisfy the premise BY CONSTRUCTION, so every generated
// case is a valid test case (zero rejects).

/// Returns `(child, parent)` such that `is_subtype(child, parent)` holds
/// by construction. Uses three constructions: `child` itself (refl),
/// `Or(child, anything)` (Or-RHS), `And(child, anything)` (And-LHS).
fn arb_label_with_supertype() -> impl Strategy<Value = (LabelType, LabelType)> {
    let kind = 0u8..3;
    (kind, arb_label_type(), arb_label_type()).prop_map(|(k, child, sibling)| {
        let parent = match k {
            0 => child.clone(),
            1 => LabelType::Or(Box::new(child.clone()), Box::new(sibling)),
            _ => LabelType::Or(Box::new(sibling), Box::new(child.clone())),
        };
        (child, parent)
    })
}

/// Returns `(child, parent)` such that `is_subtype(child, parent)` holds
/// for SimpleType. Uses Star, Or-with-self, and reflexivity.
fn arb_simple_with_supertype() -> impl Strategy<Value = (SimpleType, SimpleType)> {
    let kind = 0u8..3;
    (kind, arb_simple_type(), arb_simple_type()).prop_map(|(k, child, sibling)| {
        let parent = match k {
            0 => child.clone(),
            1 => SimpleType::Star,
            _ => SimpleType::Union(Box::new(child.clone()), Box::new(sibling)),
        };
        (child, parent)
    })
}

// =======================================================================
// Schema generators
// =======================================================================
//
// `Schema::star()` is the most permissive possible schema; refinement
// against it never returns Zero, so it only exercises a tiny slice of
// the dispatch logic. Real bugs (e.g. the `EdgeDir::Any` regression
// fixed in commit `9ec4975`) live in restrictive-schema dispatch.
// These generators exercise that surface.

/// Schema with a controlled mix of node / directed-edge / undirected-
/// edge entries. The flags pin which kinds of entries appear so a
/// failing test's input space is interpretable.
fn arb_schema_with(
    has_directed_edges: bool,
    has_undirected_edges: bool,
) -> impl Strategy<Value = Schema> {
    let nodes = prop::collection::vec(
        arb_label_atom()
            .prop_map(|l| VariableType::Node(DescriptorType::new(l, PropertyType::open_empty()))),
        1..4,
    );

    let directed = prop::collection::vec(
        arb_label_atom().prop_map(|l| {
            VariableType::edge_directional(DescriptorType::new(l, PropertyType::open_empty()))
        }),
        if has_directed_edges { 1..4 } else { 0..1 },
    );

    let undirected = prop::collection::vec(
        arb_label_atom().prop_map(|l| {
            VariableType::edge_non_directional(DescriptorType::new(l, PropertyType::open_empty()))
        }),
        if has_undirected_edges { 1..4 } else { 0..1 },
    );

    (nodes, directed, undirected).prop_map(|(nodes, dir, undir)| {
        let mut edges = dir;
        edges.extend(undir);
        Schema { nodes, edges }
    })
}

/// Schema containing ONLY EdgeNonDirectional edges. This is the shape
/// LDBC `knows` lives in, and the shape that was silently dropped by
/// the EdgeDir::Any bug.
fn arb_schema_only_undirected() -> impl Strategy<Value = Schema> {
    arb_schema_with(false, true)
}

// =======================================================================
// Shared proptest config
// =======================================================================

fn cfg() -> ProptestConfig {
    // 256 cases per property is proptest's own default — chosen for
    // genuine input-space coverage. Total runtime stays under a second
    // even at this rate (most properties run in microseconds; the
    // bottleneck is generator allocation, not the assertions).
    ProptestConfig {
        cases: 256,
        max_shrink_iters: 1024,
        ..ProptestConfig::default()
    }
}

// =======================================================================
// Documentation: equivalence-relation looseness
// =======================================================================
//
// Hand-picked tests that demonstrate, in code, why this file uses
// canonical-form equality instead of mutual-subtype. If a future reader
// is tempted to "simplify" the canon module away, these tests will fail
// the moment they swap canon for mutual-subtype.

mod equivalence_relations {
    use super::*;

    #[test]
    fn mutual_subtype_collapses_simple_to_star() {
        // §3.2: `Star ≤ τ` AND `τ ≤ Star` for any τ, so anything is
        // mutually-subtype-equivalent to Star. A regression where
        // meet returns Star instead of the expected atom would slip
        // past mutual-subtype-based equality.
        assert!(SimpleType::is_subtype(&SimpleType::Z, &SimpleType::Star));
        assert!(SimpleType::is_subtype(&SimpleType::Star, &SimpleType::Z));
        // …but the canonical forms differ:
        assert_ne!(
            canon::simple(&SimpleType::Z),
            canon::simple(&SimpleType::Star)
        );
    }

    #[test]
    fn mutual_subtype_collapses_label_to_star() {
        let a = LabelType::Label("A".into());
        assert!(LabelType::is_subtype(&a, &LabelType::Star));
        assert!(LabelType::is_subtype(&LabelType::Star, &a));
        assert_ne!(canon::label(&a), canon::label(&LabelType::Star));
    }

    #[test]
    fn mutual_subtype_collapses_or_under_plausibility() {
        // §3.1 Or-LHS: `(A ≤ A) ⇒ Or(A, B) ≤ A`
        // §3.1 Or-RHS: `(A ≤ A) ⇒ A ≤ Or(A, B)`
        // ⇒ Or(A, B) ≡ A under mutual-subtype, even though Or(A, B)
        // is wider than A.
        let a = LabelType::Label("A".into());
        let or_ab = LabelType::Or(Box::new(a.clone()), Box::new(LabelType::Label("B".into())));
        assert!(LabelType::is_subtype(&or_ab, &a));
        assert!(LabelType::is_subtype(&a, &or_ab));
        // …but the canonical forms differ:
        assert_ne!(canon::label(&a), canon::label(&or_ab));
    }

    #[test]
    fn canon_treats_reordered_unions_as_equal() {
        // The whole point: two ASTs that differ in only the order of
        // a commutative operator MUST canonicalize to the same form.
        let a = LabelType::Label("A".into());
        let b = LabelType::Label("B".into());
        let ab = LabelType::And(Box::new(a.clone()), Box::new(b.clone()));
        let ba = LabelType::And(Box::new(b), Box::new(a));
        assert_eq!(canon::label(&ab), canon::label(&ba));
    }

    #[test]
    fn canon_dedups_idempotent_operators() {
        // And and Or are idempotent: `A & A = A`. Canonicalize should
        // collapse duplicates, mirroring what gqlite's lattice ops do.
        let a = LabelType::Label("A".into());
        let aa = LabelType::And(Box::new(a.clone()), Box::new(a.clone()));
        assert_eq!(canon::label(&aa), canon::label(&a));
    }
}

// =======================================================================
// SimpleType — algebraic invariants + spec rules (§3.2, §4.1)
// =======================================================================

mod simple_type {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        // §3 Reflexivity.
        #[test]
        fn subtype_reflexive(t in arb_simple_type()) {
            prop_assert!(SimpleType::is_subtype(&t, &t),
                "is_subtype({t}, {t}) should be true");
        }

        // §4.1 Meet idempotent — canonical equality, not mutual subtype.
        #[test]
        fn meet_idempotent(t in arb_simple_type()) {
            let m = SimpleType::meet(&t, &t);
            assert_canon_eq!(simple, m, t.clone(),
                "meet(t, t) should be canonically equal to t");
        }

        // §4 Meet is greatest lower bound: `meet(a, b) ≤ a` AND `≤ b`.
        #[test]
        fn meet_lower_bound(a in arb_simple_type(), b in arb_simple_type()) {
            let m = SimpleType::meet(&a, &b);
            prop_assert!(SimpleType::is_subtype(&m, &a),
                "meet({a}, {b}) = {m} should be ≤ {a}");
            prop_assert!(SimpleType::is_subtype(&m, &b),
                "meet({a}, {b}) = {m} should be ≤ {b}");
        }

        // §4 Meet commutative — canonical equality.
        #[test]
        fn meet_commutative(a in arb_simple_type(), b in arb_simple_type()) {
            let ab = SimpleType::meet(&a, &b);
            let ba = SimpleType::meet(&b, &a);
            assert_canon_eq!(simple, ab, ba,
                "meet should be commutative under canonical equality");
        }

        // §4.1 Star is meet identity.
        #[test]
        fn star_is_meet_identity(t in arb_simple_type()) {
            prop_assert_eq!(SimpleType::meet(&SimpleType::Star, &t), t.clone(),
                "Star ⊓ t = t");
            prop_assert_eq!(SimpleType::meet(&t, &SimpleType::Star), t,
                "t ⊓ Star = t");
        }

        // §3.2 Bottom: `⊥ ≤ τ`.
        #[test]
        fn zero_is_subtype_of_everything(t in arb_simple_type()) {
            prop_assert!(SimpleType::is_subtype(&SimpleType::Zero, &t),
                "Zero ≤ {t}");
        }

        // §3.2 Star bidirectional (the looseness — but tested as a spec
        // invariant since the spec EXPLICITLY mandates it).
        #[test]
        fn star_is_subtype_of_anything(t in arb_simple_type()) {
            prop_assert!(SimpleType::is_subtype(&SimpleType::Star, &t),
                "rules.md §3.2: Star ≤ {t}");
        }
        #[test]
        fn anything_is_subtype_of_star(t in arb_simple_type()) {
            prop_assert!(SimpleType::is_subtype(&t, &SimpleType::Star),
                "rules.md §3.2: {t} ≤ Star");
        }

        // §4.1 distinct atoms meet to Zero (gqlite-specific: Records
        // with different shapes go to Zero, atoms ditto).
        #[test]
        fn distinct_atoms_meet_to_zero(a in arb_simple_atom(), b in arb_simple_atom()) {
            prop_assume!(a != b);
            prop_assert_eq!(SimpleType::meet(&a, &b), SimpleType::Zero,
                "meet of distinct atoms = Zero");
        }
    }
}

// =======================================================================
// SimpleType — §3.2 plausibility unions (CONSTRUCTIVE generators)
// =======================================================================

mod simple_spec_rules {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        // §3.2 Or-LHS plausibility (mirroring §3.1 for labels):
        //   (τ₁ ≤ τ₃) ⇒ (τ₁ + τ₂) ≤ τ₃
        #[test]
        fn union_lhs_left_premise_suffices(
            (child, parent) in arb_simple_with_supertype(),
            sibling in arb_simple_type(),
        ) {
            let ab = SimpleType::Union(Box::new(child), Box::new(sibling));
            prop_assert!(SimpleType::is_subtype(&ab, &parent),
                "rules.md §3.2 Union-LHS plausibility: (a ≤ p) ⇒ (a + b) ≤ p");
        }

        // Symmetric: right premise.
        #[test]
        fn union_lhs_right_premise_suffices(
            (child, parent) in arb_simple_with_supertype(),
            sibling in arb_simple_type(),
        ) {
            let ab = SimpleType::Union(Box::new(sibling), Box::new(child));
            prop_assert!(SimpleType::is_subtype(&ab, &parent),
                "rules.md §3.2 Union-LHS plausibility: (b ≤ p) ⇒ (a + b) ≤ p");
        }

        // §3.2 Or-RHS plausibility:
        //   (τ₃ ≤ τ₁) ⇒ τ₃ ≤ (τ₁ + τ₂)
        #[test]
        fn union_rhs_left_premise_suffices(
            (child, parent) in arb_simple_with_supertype(),
            sibling in arb_simple_type(),
        ) {
            let ab = SimpleType::Union(Box::new(parent), Box::new(sibling));
            prop_assert!(SimpleType::is_subtype(&child, &ab),
                "rules.md §3.2 Union-RHS plausibility: (c ≤ a) ⇒ c ≤ (a + b)");
        }

        #[test]
        fn union_rhs_right_premise_suffices(
            (child, parent) in arb_simple_with_supertype(),
            sibling in arb_simple_type(),
        ) {
            let ab = SimpleType::Union(Box::new(sibling), Box::new(parent));
            prop_assert!(SimpleType::is_subtype(&child, &ab),
                "rules.md §3.2 Union-RHS plausibility: (c ≤ b) ⇒ c ≤ (a + b)");
        }

        // §4.1 Union meet distributivity: `(τ₁ + τ₂) ⊓ τ = (τ₁ ⊓ τ) ⊔ (τ₂ ⊓ τ)`.
        //
        // NOTE: this test is largely TAUTOLOGICAL because the impl
        // directly encodes this rule in the `(Union, _)` arm of
        // `SimpleType::meet`. Both `direct` and `split` reduce to the
        // same expression. It survives as a regression lock against
        // the arm being modified — *not* as an independent verification
        // that the rule holds. If you change the meet arm, this fires.
        #[test]
        fn union_meet_distributes(
            t1 in arb_simple_type(),
            t2 in arb_simple_type(),
            t3 in arb_simple_type(),
        ) {
            let direct = SimpleType::meet(
                &SimpleType::Union(Box::new(t1.clone()), Box::new(t2.clone())),
                &t3,
            );
            let split = SimpleType::union(
                &SimpleType::meet(&t1, &t3),
                &SimpleType::meet(&t2, &t3),
            );
            assert_canon_eq!(simple, direct, split,
                "rules.md §4.1: (τ₁ + τ₂) ⊓ τ = (τ₁ ⊓ τ) ⊔ (τ₂ ⊓ τ)");
        }

        // §4.1 Union meet distributivity — RHS variant. Tests the
        // *symmetric* dispatch through `(_, Union)` arm. Catches if the
        // two arms diverge.
        #[test]
        fn union_meet_distributes_rhs(
            t1 in arb_simple_type(),
            t2 in arb_simple_type(),
            t3 in arb_simple_type(),
        ) {
            let direct = SimpleType::meet(
                &t3,
                &SimpleType::Union(Box::new(t1.clone()), Box::new(t2.clone())),
            );
            let split = SimpleType::union(
                &SimpleType::meet(&t3, &t1),
                &SimpleType::meet(&t3, &t2),
            );
            assert_canon_eq!(simple, direct, split,
                "rules.md §4.1: τ ⊓ (τ₁ + τ₂) = (τ ⊓ τ₁) ⊔ (τ ⊓ τ₂)");
        }
    }
}

// =======================================================================
// LabelType — algebraic invariants
// =======================================================================

mod label_type {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        #[test]
        fn subtype_reflexive(t in arb_label_type()) {
            prop_assert!(LabelType::is_subtype(&t, &t));
        }

        // Idempotent under mutual-subtype (the gradual lattice's
        // equivalence). Star-collapse regressions are locked separately
        // by the `meet_locks` hand-picked tests below.
        #[test]
        fn meet_idempotent(t in arb_label_type()) {
            let m = LabelType::meet(&t, &t);
            assert_consistent!(LabelType::is_subtype, m, t.clone(),
                "meet(t, t) ≡ t under mutual-subtype");
        }

        #[test]
        fn meet_lower_bound(a in arb_label_type(), b in arb_label_type()) {
            let m = LabelType::meet(&a, &b);
            prop_assert!(LabelType::is_subtype(&m, &a));
            prop_assert!(LabelType::is_subtype(&m, &b));
        }

        // Commutative under mutual-subtype. gqlite's `LabelType::meet`
        // is order-dependent within mutual-subtype equivalence classes
        // (e.g. for `And(Star, Star)` and `And(Star, Label("A"))`,
        // which are mutually-subtype-equivalent under §3.1 plausibility,
        // it returns the first operand). Both sides therefore agree
        // semantically even though they differ syntactically.
        #[test]
        fn meet_commutative(a in arb_label_type(), b in arb_label_type()) {
            let ab = LabelType::meet(&a, &b);
            let ba = LabelType::meet(&b, &a);
            assert_consistent!(LabelType::is_subtype, ab, ba);
        }

        #[test]
        fn star_is_meet_identity(t in arb_label_type()) {
            prop_assert_eq!(LabelType::meet(&LabelType::Star, &t), t,
                "Star ⊓ t = t");
        }
    }
}

// =======================================================================
// LabelType — rules.md §3.1 spec rules (CONSTRUCTIVE)
// =======================================================================

mod label_spec_rules {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        // §3.1 Star bidirectional.
        #[test]
        fn star_is_subtype_of_anything(t in arb_label_type()) {
            prop_assert!(LabelType::is_subtype(&LabelType::Star, &t),
                "rules.md §3.1 Star: Star ≤ {t}");
        }
        #[test]
        fn anything_is_subtype_of_star(t in arb_label_type()) {
            prop_assert!(LabelType::is_subtype(&t, &LabelType::Star),
                "rules.md §3.1 Star: {t} ≤ Star");
        }

        // §3.1 And-LHS: `(ℓ₁ ≤ ℓ₃) ⇒ (ℓ₁ & ℓ₂) ≤ ℓ₃`.
        #[test]
        fn and_lhs_left_premise_suffices(
            (child, parent) in arb_label_with_supertype(),
            b in arb_label_type(),
        ) {
            let ab = LabelType::And(Box::new(child), Box::new(b));
            prop_assert!(LabelType::is_subtype(&ab, &parent),
                "rules.md §3.1 And-LHS: (a ≤ p) ⇒ (a & b) ≤ p");
        }
        #[test]
        fn and_lhs_right_premise_suffices(
            (child, parent) in arb_label_with_supertype(),
            a in arb_label_type(),
        ) {
            let ab = LabelType::And(Box::new(a), Box::new(child));
            prop_assert!(LabelType::is_subtype(&ab, &parent),
                "rules.md §3.1 And-LHS: (b ≤ p) ⇒ (a & b) ≤ p");
        }

        // §3.1 And-RHS: `(ℓ₁ ≤ ℓ₂) ∧ (ℓ₁ ≤ ℓ₃) ⇒ ℓ₁ ≤ (ℓ₂ & ℓ₃)`.
        // Both premises required — generate `c` once, derive two
        // supertypes from it via Or-with-sibling.
        #[test]
        fn and_rhs_requires_both_premises(
            c in arb_label_type(),
            sib1 in arb_label_type(),
            sib2 in arb_label_type(),
        ) {
            let p1 = LabelType::Or(Box::new(c.clone()), Box::new(sib1));
            let p2 = LabelType::Or(Box::new(c.clone()), Box::new(sib2));
            // c ≤ p1 by Or-RHS; c ≤ p2 by Or-RHS. Therefore by And-RHS:
            let bc = LabelType::And(Box::new(p1), Box::new(p2));
            prop_assert!(LabelType::is_subtype(&c, &bc),
                "rules.md §3.1 And-RHS: (c ≤ p1) ∧ (c ≤ p2) ⇒ c ≤ (p1 & p2)");
        }

        // §3.1 Or-LHS plausibility (gradual rule):
        //   (ℓ₁ ≤ ℓ₃) ⇒ (ℓ₁ + ℓ₂) ≤ ℓ₃.
        // pygql implements the older STRICT rule (both required) — a
        // bug there. fppc/gqlite implement the gradual rule correctly.
        #[test]
        fn or_lhs_left_premise_suffices(
            (child, parent) in arb_label_with_supertype(),
            sibling in arb_label_type(),
        ) {
            let ab = LabelType::Or(Box::new(child), Box::new(sibling));
            prop_assert!(LabelType::is_subtype(&ab, &parent),
                "rules.md §3.1 Or-LHS plausibility: (a ≤ p) ⇒ (a + b) ≤ p");
        }
        #[test]
        fn or_lhs_right_premise_suffices(
            (child, parent) in arb_label_with_supertype(),
            sibling in arb_label_type(),
        ) {
            let ab = LabelType::Or(Box::new(sibling), Box::new(child));
            prop_assert!(LabelType::is_subtype(&ab, &parent),
                "rules.md §3.1 Or-LHS plausibility: (b ≤ p) ⇒ (a + b) ≤ p");
        }

        // §3.1 Or-RHS plausibility: `(ℓ₃ ≤ ℓ₁) ⇒ ℓ₃ ≤ (ℓ₁ + ℓ₂)`.
        #[test]
        fn or_rhs_left_premise_suffices(
            (child, parent) in arb_label_with_supertype(),
            sibling in arb_label_type(),
        ) {
            let ab = LabelType::Or(Box::new(parent), Box::new(sibling));
            prop_assert!(LabelType::is_subtype(&child, &ab),
                "rules.md §3.1 Or-RHS plausibility: (c ≤ a) ⇒ c ≤ (a + b)");
        }
        #[test]
        fn or_rhs_right_premise_suffices(
            (child, parent) in arb_label_with_supertype(),
            sibling in arb_label_type(),
        ) {
            let ab = LabelType::Or(Box::new(sibling), Box::new(parent));
            prop_assert!(LabelType::is_subtype(&child, &ab),
                "rules.md §3.1 Or-RHS plausibility: (c ≤ b) ⇒ c ≤ (a + b)");
        }

        // §4.1 Distinct atoms meet to And — verifies the SHAPE, not just
        // lower-bound (which Zero would also satisfy).
        #[test]
        fn distinct_atom_meet_yields_and(s in "[A-D]", t in "[A-D]") {
            prop_assume!(s != t);
            let a = LabelType::Label(s);
            let b = LabelType::Label(t);
            let m = LabelType::meet(&a, &b);
            prop_assert!(matches!(&m, LabelType::And(_, _)),
                "rules.md §4.1: 1₁ ⊓ 1₂ should produce And, got {m:?}");
            prop_assert!(LabelType::is_subtype(&m, &a));
            prop_assert!(LabelType::is_subtype(&m, &b));
        }
    }
}

// =======================================================================
// PropertyType — algebraic invariants
// =======================================================================

mod property_type {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        #[test]
        fn subtype_reflexive(t in arb_property_type()) {
            prop_assert!(PropertyType::is_subtype(&t, &t));
        }

        #[test]
        fn meet_idempotent(t in arb_property_type()) {
            let m = PropertyType::meet(&t, &t);
            assert_canon_eq!(property, m, t.clone(),
                "meet(t, t) ≡ t");
        }

        #[test]
        fn meet_lower_bound(a in arb_property_type(), b in arb_property_type()) {
            let m = PropertyType::meet(&a, &b);
            prop_assert!(PropertyType::is_subtype(&m, &a));
            prop_assert!(PropertyType::is_subtype(&m, &b));
        }

        #[test]
        fn meet_commutative(a in arb_property_type(), b in arb_property_type()) {
            let ab = PropertyType::meet(&a, &b);
            let ba = PropertyType::meet(&b, &a);
            assert_canon_eq!(property, ab, ba);
        }

        // §3.3 Bottom: `⊥ ≤ R`.
        #[test]
        fn zero_is_subtype_of_everything(t in arb_property_type()) {
            prop_assert!(PropertyType::is_subtype(&PropertyType::Zero, &t));
        }
    }
}

// =======================================================================
// PropertyType — rules.md §3.3 / §4.2 spec rules
// =======================================================================

mod property_spec_rules {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        // §3.3 Closed records: width subtyping is FORBIDDEN.
        #[test]
        fn closed_records_no_width_subtyping(
            v_a in arb_simple_type(),
            v_b in arb_simple_type(),
        ) {
            let mut narrow = BTreeMap::new();
            narrow.insert("a".to_string(), v_a.clone());
            let mut wide = BTreeMap::new();
            wide.insert("a".to_string(), v_a);
            wide.insert("b".to_string(), v_b);
            let r1 = PropertyType::Closed(narrow);
            let r2 = PropertyType::Closed(wide);
            prop_assert!(!PropertyType::is_subtype(&r1, &r2),
                "rules.md §3.3: Closed forbids width — {r1} ≤ {r2} should be false");
            prop_assert!(!PropertyType::is_subtype(&r2, &r1),
                "rules.md §3.3: Closed forbids width — {r2} ≤ {r1} should be false");
        }

        // §3.3 Open records: width subtyping IS allowed via the star.
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
                "rules.md §3.3 Open: width subtyping should let {r1} and {r2} relate");
        }

        // §3.3 Mixed Closed-Open with shared keys.
        // A Closed with exactly the keys an Open promises (its
        // explicitly-named subset) IS a subtype of that Open.
        #[test]
        fn closed_subtype_of_open_with_same_keys(shared in arb_simple_type()) {
            let mut closed_map = BTreeMap::new();
            closed_map.insert("a".to_string(), shared.clone());
            let mut open_map = BTreeMap::new();
            open_map.insert("a".to_string(), shared);
            let c = PropertyType::Closed(closed_map);
            let o = PropertyType::Open(open_map);
            prop_assert!(PropertyType::is_subtype(&c, &o),
                "rules.md §3.3: Closed{{a}} ≤ Open{{a, *}}");
        }

        // §4.2 Property meet — Zero is absorbing.
        #[test]
        fn zero_absorbs_property_meet(t in arb_property_type()) {
            prop_assert_eq!(PropertyType::meet(&PropertyType::Zero, &t), PropertyType::Zero,
                "rules.md §4.2: ⊥ ⊓ R = ⊥");
        }

        // §4.2 Closed/Closed meet — same keys ⇒ pairwise meet.
        #[test]
        fn closed_meet_closed_same_keys_pairwise(
            v1 in arb_simple_type(),
            v2 in arb_simple_type(),
        ) {
            let mut m1 = BTreeMap::new();
            m1.insert("a".to_string(), v1.clone());
            let mut m2 = BTreeMap::new();
            m2.insert("a".to_string(), v2.clone());
            let r1 = PropertyType::Closed(m1);
            let r2 = PropertyType::Closed(m2);
            let m = PropertyType::meet(&r1, &r2);
            let mut expected_map = BTreeMap::new();
            expected_map.insert("a".to_string(), SimpleType::meet(&v1, &v2));
            let expected = PropertyType::Closed(expected_map);
            assert_canon_eq!(property, m, expected,
                "rules.md §4.2: Closed/Closed same-keys ⊓ = pairwise meet");
        }

        // §4.2 Closed/Closed meet — different keys ⇒ Zero.
        // (gqlite-specific: paper doesn't prescribe, gqlite returns Zero
        // for incompatible record shapes. Documenting here.)
        #[test]
        fn closed_meet_closed_different_keys_yields_zero(
            v1 in arb_simple_type(),
            v2 in arb_simple_type(),
        ) {
            let mut m1 = BTreeMap::new();
            m1.insert("a".to_string(), v1);
            let mut m2 = BTreeMap::new();
            m2.insert("b".to_string(), v2);
            let r1 = PropertyType::Closed(m1);
            let r2 = PropertyType::Closed(m2);
            prop_assert_eq!(PropertyType::meet(&r1, &r2), PropertyType::Zero,
                "Closed/Closed with non-matching keys → ⊥");
        }

        // §4.2 Open/Closed meet — Open's keys must be subset of Closed's.
        // The result is Closed with all of Closed's keys, meet'd on shared.
        #[test]
        fn open_meet_closed_with_subset_keys_yields_closed(
            shared in arb_simple_type(),
            extra_in_closed in arb_simple_type(),
        ) {
            let mut open_map = BTreeMap::new();
            open_map.insert("a".to_string(), shared.clone());
            let mut closed_map = BTreeMap::new();
            closed_map.insert("a".to_string(), shared.clone());
            closed_map.insert("b".to_string(), extra_in_closed.clone());
            let o = PropertyType::Open(open_map);
            let c = PropertyType::Closed(closed_map);
            let m = PropertyType::meet(&o, &c);
            // Should be Closed with keys {a, b}.
            match m {
                PropertyType::Closed(ref out) => {
                    prop_assert!(out.contains_key("a"));
                    prop_assert!(out.contains_key("b"));
                    prop_assert_eq!(out["b"].clone(), extra_in_closed,
                        "key from Closed not in Open should pass through unchanged");
                }
                _ => prop_assert!(false, "Open ⊓ Closed (subset keys) should be Closed, got {m}"),
            }
        }

        // §4.2 Open/Closed meet with non-subset Open keys → Zero.
        #[test]
        fn open_meet_closed_with_extra_open_keys_yields_zero(
            v1 in arb_simple_type(),
            v2 in arb_simple_type(),
        ) {
            let mut open_map = BTreeMap::new();
            open_map.insert("a".to_string(), v1);
            open_map.insert("b".to_string(), v2.clone());
            let mut closed_map = BTreeMap::new();
            closed_map.insert("a".to_string(), v2);
            // Open promises {a, b, *}, Closed promises exactly {a}.
            // The Open's b doesn't fit in Closed → Zero.
            let o = PropertyType::Open(open_map);
            let c = PropertyType::Closed(closed_map);
            prop_assert_eq!(PropertyType::meet(&o, &c), PropertyType::Zero,
                "rules.md §4.2: Open ⊓ Closed when Open has keys not in Closed → ⊥");
        }

        // §4.2 Open/Open meet — union of keys, recursive meet on shared.
        #[test]
        fn open_meet_open_unions_keys(
            v_a1 in arb_simple_type(),
            v_a2 in arb_simple_type(),
            v_b in arb_simple_type(),
            v_c in arb_simple_type(),
        ) {
            let mut m1 = BTreeMap::new();
            m1.insert("a".to_string(), v_a1.clone());
            m1.insert("b".to_string(), v_b.clone());
            let mut m2 = BTreeMap::new();
            m2.insert("a".to_string(), v_a2.clone());
            m2.insert("c".to_string(), v_c.clone());
            let r1 = PropertyType::Open(m1);
            let r2 = PropertyType::Open(m2);
            let m = PropertyType::meet(&r1, &r2);

            // Expected: {a: meet(v_a1, v_a2), b: v_b, c: v_c}, Open.
            let mut expected_map = BTreeMap::new();
            expected_map.insert("a".to_string(), SimpleType::meet(&v_a1, &v_a2));
            expected_map.insert("b".to_string(), v_b);
            expected_map.insert("c".to_string(), v_c);
            let expected = PropertyType::Open(expected_map);
            assert_canon_eq!(property, m, expected,
                "rules.md §4.2: Open/Open ⊓ unions keys, meets shared");
        }
    }
}

// =======================================================================
// DescriptorType — algebraic invariants + spec rules
// =======================================================================

mod descriptor_type {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        #[test]
        fn subtype_reflexive(t in arb_descriptor_type()) {
            prop_assert!(DescriptorType::is_subtype(&t, &t));
        }

        // Idempotent / commutative under mutual-subtype (inherits the
        // gradual looseness from LabelType — see meet_commutative there
        // for context).
        #[test]
        fn meet_idempotent(t in arb_descriptor_type()) {
            let m = DescriptorType::meet(&t, &t);
            assert_consistent!(DescriptorType::is_subtype, m, t.clone());
        }

        #[test]
        fn meet_lower_bound(a in arb_descriptor_type(), b in arb_descriptor_type()) {
            let m = DescriptorType::meet(&a, &b);
            prop_assert!(DescriptorType::is_subtype(&m, &a));
            prop_assert!(DescriptorType::is_subtype(&m, &b));
        }

        #[test]
        fn meet_commutative(a in arb_descriptor_type(), b in arb_descriptor_type()) {
            let ab = DescriptorType::meet(&a, &b);
            let ba = DescriptorType::meet(&b, &a);
            assert_consistent!(DescriptorType::is_subtype, ab, ba);
        }

        // §3.4 Descriptor sub: `(ℓ₁ ≤ ℓ₂) ∧ (R₁ ≤ R₂) ⇒ ℓ₁R₁ ≤ ℓ₂R₂`.
        #[test]
        fn descriptor_subtype_is_componentwise(
            (lc, lp) in arb_label_with_supertype(),
            r1 in arb_property_type(),
            r2 in arb_property_type(),
        ) {
            prop_assume!(PropertyType::is_subtype(&r1, &r2));
            let d1 = DescriptorType::new(lc, r1);
            let d2 = DescriptorType::new(lp, r2);
            prop_assert!(DescriptorType::is_subtype(&d1, &d2),
                "rules.md §3.4: descriptor sub is componentwise");
        }

        // §3.4 Descriptor star.
        #[test]
        fn star_descriptor_is_subtype_supremum(t in arb_descriptor_type()) {
            let star = DescriptorType::star();
            prop_assert!(DescriptorType::is_subtype(&t, &star),
                "rules.md §3.4: any descriptor ≤ DescriptorType::star()");
        }
    }
}

// =======================================================================
// VariableType — algebraic invariants
// =======================================================================

mod variable_type {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        #[test]
        fn subtype_reflexive(t in arb_variable_type()) {
            prop_assert!(VariableType::is_subtype(&t, &t));
        }

        // Idempotent / commutative under mutual-subtype. In addition
        // to gradual looseness, gqlite's `meet` for `EdgeNonDirectional`
        // tries both endpoint orientations and joins the results — so
        // `meet(E, E)` for an asymmetric undirected edge produces
        // `Union(E, swap(E))` rather than just `E`. The two are mutually
        // subtype-equivalent (each is subsumed by the other under the
        // symmetric undirected check) but not canonically equal.
        #[test]
        fn meet_idempotent(t in arb_variable_type()) {
            let m = VariableType::meet(&t, &t);
            assert_consistent!(VariableType::is_subtype, m, t.clone(),
                "meet(t, t) ≡ t under mutual-subtype");
        }

        #[test]
        fn meet_lower_bound(a in arb_variable_type(), b in arb_variable_type()) {
            let m = VariableType::meet(&a, &b);
            prop_assert!(VariableType::is_subtype(&m, &a));
            prop_assert!(VariableType::is_subtype(&m, &b));
        }

        #[test]
        fn meet_commutative(a in arb_variable_type(), b in arb_variable_type()) {
            let ab = VariableType::meet(&a, &b);
            let ba = VariableType::meet(&b, &a);
            assert_consistent!(VariableType::is_subtype, ab, ba);
        }

        #[test]
        fn zero_absorbs_in_meet(t in arb_variable_type()) {
            prop_assert_eq!(VariableType::meet(&VariableType::Zero, &t), VariableType::Zero);
            prop_assert_eq!(VariableType::meet(&t, &VariableType::Zero), VariableType::Zero);
        }

        #[test]
        fn zero_is_subtype_of_everything(t in arb_variable_type()) {
            prop_assert!(VariableType::is_subtype(&VariableType::Zero, &t));
        }

        #[test]
        fn refine_against_star_schema_is_identity_for_compatible_shapes(
            t in arb_refinable_variable(),
        ) {
            let r = VariableType::refine(&Schema::star(), &t);
            prop_assert!(!r.is_empty(),
                "refine(star, {t}) collapsed to empty: {r}");
        }
    }
}

// =======================================================================
// VariableType — rules.md §4.3 / §6 / §8.4
// =======================================================================

mod variable_spec_rules {
    use super::*;

    proptest! {
        #![proptest_config(cfg())]

        // §4.3 Undirected edge meet — orientation symmetric.
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
            let normal = VariableType::meet(
                &mk(d1.clone(), l1.clone(), r1.clone()),
                &mk(d2.clone(), l2.clone(), r2.clone()),
            );
            let swapped = VariableType::meet(&mk(d1, l1, r1), &mk(d2, r2, l2));
            assert_canon_eq!(variable, normal, swapped,
                "rules.md §4.3: undirected meet is orientation-symmetric");
        }

        // §6 Refinement union distribution.
        //
        // NOTE: tautological — `VariableType::refine`'s `Union` arm IS
        // `join(refine(t1), refine(t2))`. Both sides reduce to the same
        // expression. Regression lock against arm modification.
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
            assert_canon_eq!(variable, direct, split,
                "rules.md §6: refine(T₁ + T₂) ≡ refine(T₁) ⊔ refine(T₂)");
        }

        // §3.4 Node sub: `(L₁ ≤ L₂) ⇒ (|L₁|) ≤ (|L₂|)`.
        #[test]
        fn node_subtype_lifts_descriptor_subtype(
            (lc, lp) in arb_label_with_supertype(),
        ) {
            let dc = DescriptorType::new(lc, PropertyType::open_empty());
            let dp = DescriptorType::new(lp, PropertyType::open_empty());
            let nc = VariableType::Node(dc);
            let np = VariableType::Node(dp);
            prop_assert!(VariableType::is_subtype(&nc, &np),
                "rules.md §3.4: descriptor sub lifts to Node sub");
        }

        // §3.4 Edge sub: `desc ≤ desc' ∧ left ≤ left' ∧ right ≤ right'
        //                ⇒ EdgeDir ≤ EdgeDir'`.
        #[test]
        fn edge_directional_subtype_componentwise(
            (lc, lp) in arb_label_with_supertype(),
        ) {
            // Build two edges where the child's descriptor is sub of
            // the parent's descriptor, with star endpoints (which are
            // mutually subtype of anything via DescriptorType::star()).
            let dc = DescriptorType::new(lc, PropertyType::open_empty());
            let dp = DescriptorType::new(lp, PropertyType::open_empty());
            let ec = VariableType::edge_directional(dc);
            let ep = VariableType::edge_directional(dp);
            prop_assert!(VariableType::is_subtype(&ec, &ep),
                "rules.md §3.4: edge sub is componentwise on desc/left/right");
        }

        // §8.4 empty() — Union case: empty(T₁+T₂) iff empty(T₁) ∧ empty(T₂).
        #[test]
        fn union_empty_iff_both_components_empty(
            t1 in arb_variable_type(),
            t2 in arb_variable_type(),
        ) {
            let u = VariableType::Union(Box::new(t1.clone()), Box::new(t2.clone()));
            prop_assert_eq!(u.is_empty(), t1.is_empty() && t2.is_empty(),
                "rules.md §8.4: empty(T₁ + T₂) iff empty(T₁) ∧ empty(T₂)");
        }
    }
}

// =======================================================================
// Schema-diverse refinement (the EdgeDir::Any-bug class)
// =======================================================================
//
// Refinement against `Schema::star()` only exercises a permissive
// dispatch path. Bugs in restrictive-schema dispatch (the
// `EdgeDir::Any → directional-only` regression fixed in `9ec4975` is
// one) need the schema-diverse surface these tests provide. Each
// property is constructed so that the query SHOULD admit some schema
// entry; if dispatch silently drops the matching entries, refinement
// returns Zero and the property fails.

mod schema_refinement_rules {
    use super::*;

    /// Helper: a star-labeled `Node` query — admits any node label.
    fn node_star_query() -> VariableType {
        VariableType::Node(DescriptorType::star())
    }

    /// Helper: a star-labeled non-directional edge query (`(p)~[]~(q)`).
    fn edge_nondir_query() -> VariableType {
        VariableType::edge_non_directional(DescriptorType::star())
    }

    /// Helper: a star-labeled directional edge query (`(p)-[]->(q)`).
    fn edge_dir_query() -> VariableType {
        VariableType::edge_directional(DescriptorType::star())
    }

    proptest! {
        #![proptest_config(cfg())]

        // §6 Refinement: against an empty schema, every query → ⊥.
        #[test]
        fn refine_against_empty_schema_is_zero(t in arb_refinable_variable()) {
            let empty = Schema { nodes: vec![], edges: vec![] };
            let r = VariableType::refine(&empty, &t);
            prop_assert_eq!(r, VariableType::Zero,
                "rules.md §6: refine against empty schema = ⊥");
        }

        // §6 Refinement: a node query against a schema with at least
        // one node entry should NOT collapse to ⊥.
        #[test]
        fn refine_node_against_schema_with_nodes(schema in arb_schema_with(false, false)) {
            let r = VariableType::refine(&schema, &node_star_query());
            prop_assert!(!matches!(r, VariableType::Zero),
                "refining (x:*) against schema with nodes should not be ⊥");
        }

        // §6 Refinement: a directional-edge query (`(p)-[]->(q)`)
        // against a schema with at least one directional edge should
        // not collapse. (Locks: directional dispatch.)
        #[test]
        fn refine_directional_edge_against_directed_schema(
            schema in arb_schema_with(true, false),
        ) {
            let r = VariableType::refine(&schema, &edge_dir_query());
            prop_assert!(!matches!(r, VariableType::Zero),
                "refining a directional edge against schema with directed edges should not be ⊥");
        }

        // §6 Refinement: a non-directional-edge query (`(p)~[]~(q)`)
        // against a schema with at least one non-directional edge
        // should not collapse. (Locks: undirected dispatch.)
        #[test]
        fn refine_undirected_edge_against_undirected_schema(
            schema in arb_schema_only_undirected(),
        ) {
            let r = VariableType::refine(&schema, &edge_nondir_query());
            prop_assert!(!matches!(r, VariableType::Zero),
                "refining an undirected edge against schema with undirected edges should not be ⊥");
        }
    }

    // The EdgeDir::Any-class bug lived in the typechecker's dispatch,
    // not in `VariableType::refine` itself: `refine_pattern_edge` used
    // to bucket `Any` with `Right`/`Left` and refine only as directional,
    // silently dropping non-directional schema entries. Catching that
    // dispatch bug needs a typechecker-level test, not a lattice one.
    //
    // What can be locked at the lattice level: refinement-as-meet
    // against a schema with only non-directional edges should match a
    // non-directional query. That confirms the lattice op underneath
    // is sound, even if dispatch above it is buggy.

    #[test]
    fn nondirectional_edge_query_admits_nondirectional_schema_entries() {
        // Schema with one EdgeNonDirectional entry. A non-directional
        // edge query should match it (refinement is non-Zero).
        let schema = Schema {
            nodes: vec![VariableType::node_star()],
            edges: vec![VariableType::edge_non_directional(DescriptorType::new(
                LabelType::Label("knows".into()),
                PropertyType::open_empty(),
            ))],
        };
        let query = VariableType::edge_non_directional(DescriptorType::new(
            LabelType::Label("knows".into()),
            PropertyType::open_empty(),
        ));
        let r = VariableType::refine(&schema, &query);
        assert!(
            !matches!(r, VariableType::Zero),
            "non-directional `knows` query should refine non-trivially against \
             schema with non-directional `knows` (got ⊥)"
        );
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

    // LabelType::meet — Star is identity for And. (Already covered in
    // `meet_locks::label_meet_star_with_atom_returns_atom`; duplicating
    // here for completeness so the normalization-by-normalization audit
    // is in one place.)
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
    // `path_type::zero_is_path_union_identity_*` proptests; pinned
    // explicitly here for the audit.)
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
// The `assert_consistent!` assertions in `mod label_type` /
// `mod descriptor_type` / `mod variable_type` use mutual subtype, which
// is loose: a buggy `meet` returning `Star` for everything would still
// satisfy consistency (Star is bidirectional under §3 plausibility).
// These hand-picked tests pin specific outputs so the regression fails.

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

// =======================================================================
// PathType — rules.md §4 / §8.1 / §8.2
// =======================================================================
//
// PathType has a SPECIAL meet that CONCATENATES rather than narrowing
// (rules.md §8.2). So `meet(p, p)` is *not* idempotent — it doubles
// the path length on compatible boundaries. Reflexivity is therefore
// not a property of path meet; concatenation semantics is verified by
// focused unit tests below.

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

    proptest! {
        #![proptest_config(cfg())]

        // §4 Zero absorbs in meet.
        #[test]
        fn zero_absorbs_path_meet_left(p in arb_path_type()) {
            prop_assert_eq!(PathType::meet(&Schema::star(), &PathType::Zero, &p), PathType::Zero);
        }
        #[test]
        fn zero_absorbs_path_meet_right(p in arb_path_type()) {
            prop_assert_eq!(PathType::meet(&Schema::star(), &p, &PathType::Zero), PathType::Zero);
        }

        // §4 Union (Zero, P) = P (Zero is identity for join).
        #[test]
        fn zero_is_path_union_identity_left(p in arb_path_type()) {
            prop_assert_eq!(PathType::union(PathType::Zero, p.clone()), p);
        }
        #[test]
        fn zero_is_path_union_identity_right(p in arb_path_type()) {
            prop_assert_eq!(PathType::union(p.clone(), PathType::Zero), p);
        }

        // §4 Path union commutative under canonical form (Union children
        // are unordered semantically; gqlite's `union` doesn't sort).
        #[test]
        fn path_union_commutative(a in arb_path_type(), b in arb_path_type()) {
            let ab = PathType::union(a.clone(), b.clone());
            let ba = PathType::union(b, a);
            assert_canon_eq!(path, ab, ba);
        }

        // §4 Path union idempotent — `P ⊔ P = P` (impl collapses equals).
        #[test]
        fn path_union_idempotent(p in arb_path_type()) {
            prop_assert_eq!(PathType::union(p.clone(), p.clone()), p);
        }
    }

    // §8.2 Path meet — focused unit tests for the concatenation behavior.

    /// rules.md §8.2 right-extension:
    ///   `(P₁ - N₂) ⊓ N₃ ▷ P₁ - N₄`  where  `N₂ ⊓ N₃ ▷ N₄`
    #[test]
    fn path_meet_right_extension_preserves_length() {
        let m = PathType::meet(&Schema::star(), &star_edge(), &star_node());
        assert_eq!(m.len(), 1, "right-extension should preserve length");
    }

    /// rules.md §8.2 left-extension:
    ///   `P₁ ⊓ (P₂ - N₃) ▷ P₃ - N₃`  where  `P₁ ⊓ P₂ ▷ P₃`
    #[test]
    fn path_meet_left_extension_concatenates_two_edges() {
        let m = PathType::meet(&Schema::star(), &star_edge(), &star_edge());
        assert_eq!(
            m.len(),
            2,
            "two edges should concatenate to length 2, got {m:?}"
        );
    }

    /// Three single edges chain to length 3.
    #[test]
    fn path_meet_chains_three_edges() {
        let e = star_edge();
        let m1 = PathType::meet(&Schema::star(), &e, &e);
        let m2 = PathType::meet(&Schema::star(), &m1, &e);
        assert_eq!(m2.len(), 3, "three edges should chain to length 3");
    }

    /// rules.md §8.3 Path Length:
    ///   `len(P₁ + P₂) = min(len(P₁), len(P₂))`
    #[test]
    fn path_union_len_is_minimum() {
        let n = star_node();
        let e = star_edge();
        // len(node) = 0, len(edge) = 1. Union should pick min = 0.
        let u = PathType::Union(Box::new(n), Box::new(e));
        assert_eq!(
            u.len(),
            0,
            "rules.md §8.3: len(P₁ + P₂) = min(len(P₁), len(P₂))"
        );
    }

    // §8.1 Direction Resolution.

    /// `⌊N₁→N₂⌋→ = N₁ - N₂` — forward direction preserves left then right.
    #[test]
    fn from_variable_directional_forward() {
        let l = DescriptorType::new(LabelType::Label("L".into()), PropertyType::open_empty());
        let r = DescriptorType::new(LabelType::Label("R".into()), PropertyType::open_empty());
        let edge = VariableType::EdgeDirectional {
            desc: DescriptorType::star(),
            left: Box::new(VariableType::Node(l.clone())),
            right: Box::new(VariableType::Node(r.clone())),
        };
        let p = PathType::from_variable(&edge, EdgeDir::Right);
        match p {
            PathType::Edge(e) => {
                assert!(
                    matches!(*e.p1, PathType::Node(ref n) if n.desc == l),
                    "forward p1 should be left descriptor"
                );
                assert_eq!(e.n2.desc, r, "forward n2 should be right descriptor");
            }
            _ => panic!("forward direction should yield Edge, got {p:?}"),
        }
    }

    /// `⌊N₁→N₂⌋← = N₂ - N₁` — backward direction reverses.
    #[test]
    fn from_variable_directional_backward() {
        let l = DescriptorType::new(LabelType::Label("L".into()), PropertyType::open_empty());
        let r = DescriptorType::new(LabelType::Label("R".into()), PropertyType::open_empty());
        let edge = VariableType::EdgeDirectional {
            desc: DescriptorType::star(),
            left: Box::new(VariableType::Node(l.clone())),
            right: Box::new(VariableType::Node(r.clone())),
        };
        let p = PathType::from_variable(&edge, EdgeDir::Left);
        match p {
            PathType::Edge(e) => {
                assert!(
                    matches!(*e.p1, PathType::Node(ref n) if n.desc == r),
                    "backward p1 should be right descriptor"
                );
                assert_eq!(e.n2.desc, l, "backward n2 should be left descriptor");
            }
            _ => panic!("backward direction should yield Edge, got {p:?}"),
        }
    }

    /// `⌊N₁∼N₂⌋∼ = (N₁ - N₂) + (N₂ - N₁)` — undirected joins both
    /// orientations. Distinct endpoint descriptors so `forward !=
    /// reversed` and `PathType::union` doesn't normalize them away.
    #[test]
    fn from_variable_undirected_yields_union_of_orientations() {
        let l = DescriptorType::new(LabelType::Label("L".into()), PropertyType::open_empty());
        let r = DescriptorType::new(LabelType::Label("R".into()), PropertyType::open_empty());
        let edge = VariableType::EdgeNonDirectional {
            desc: DescriptorType::star(),
            left: Box::new(VariableType::Node(l)),
            right: Box::new(VariableType::Node(r)),
        };
        let p = PathType::from_variable(&edge, EdgeDir::Any);
        assert!(
            matches!(p, PathType::Union(_, _)),
            "rules.md §8.1: undirected with distinct endpoints should be a Union, got {p:?}"
        );
    }
}
