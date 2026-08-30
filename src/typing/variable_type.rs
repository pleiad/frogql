use std::fmt;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use super::descriptor_type::DescriptorType;
use super::simple_type::SimpleType;

/// Types for pattern variables (nodes, edges, unions, lists, bottom).
/// `Hash` keys the per-schema refine memo cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VariableType {
    Node(DescriptorType),
    EdgeDirectional {
        desc: DescriptorType,
        left: Box<VariableType>,  // NodeVariableType
        right: Box<VariableType>, // NodeVariableType
    },
    EdgeNonDirectional {
        desc: DescriptorType,
        left: Box<VariableType>,
        right: Box<VariableType>,
    },
    Union(Box<VariableType>, Box<VariableType>),
    Group(Box<VariableType>),
    /// The singleton type for the null value. Introduced when a variable
    /// appears in only one branch of a `TypeEnvironment` join, per the rule
    /// `Γ₁ ⊔ Γ₂` for keys present on a single side.
    Null,
    /// A path variable (`MATCH p = ...`). Terminal: it does not carry a
    /// descriptor, never refines against the schema, and is never shared
    /// across operands, so it stays inert under every lattice operation.
    /// Lifts to `SimpleType::Path`.
    Path,
    Zero,
}

impl VariableType {
    pub fn node_star() -> Self {
        VariableType::Node(DescriptorType::star())
    }

    pub fn edge_directional(desc: DescriptorType) -> Self {
        VariableType::EdgeDirectional {
            desc,
            left: Box::new(VariableType::node_star()),
            right: Box::new(VariableType::node_star()),
        }
    }

    pub fn edge_non_directional(desc: DescriptorType) -> Self {
        VariableType::EdgeNonDirectional {
            desc,
            left: Box::new(VariableType::node_star()),
            right: Box::new(VariableType::node_star()),
        }
    }

    /// Get the descriptor (for Node or Edge variants).
    pub fn descriptor(&self) -> Option<&DescriptorType> {
        match self {
            VariableType::Node(d) => Some(d),
            VariableType::EdgeDirectional { desc, .. } => Some(desc),
            VariableType::EdgeNonDirectional { desc, .. } => Some(desc),
            _ => None,
        }
    }

    /// Get attribute type from the property type.
    pub fn get_attribute(&self, attr: &str) -> SimpleType {
        match self {
            VariableType::Node(d) => d.props.get(attr),
            VariableType::EdgeDirectional { desc, .. } => desc.props.get(attr),
            VariableType::EdgeNonDirectional { desc, .. } => desc.props.get(attr),
            VariableType::Union(t1, t2) => {
                SimpleType::union(&t1.get_attribute(attr), &t2.get_attribute(attr))
            }
            VariableType::Group(t) => SimpleType::Group(Box::new(t.get_attribute(attr))),
            VariableType::Null => SimpleType::Zero,
            // A path has no attributes — `path.attr` is undefined.
            VariableType::Path => SimpleType::Zero,
            VariableType::Zero => SimpleType::Zero,
        }
    }

    // --- Meet ---

    fn meet_node(a: &DescriptorType, b: &DescriptorType) -> VariableType {
        VariableType::Node(DescriptorType::meet(a, b))
    }

    fn meet_edge_directional(
        d1: &DescriptorType,
        l1: &VariableType,
        r1: &VariableType,
        d2: &DescriptorType,
        l2: &VariableType,
        r2: &VariableType,
    ) -> VariableType {
        // l1, l2, r1, r2 should be Node variants
        let ld = match (l1, l2) {
            (VariableType::Node(a), VariableType::Node(b)) => DescriptorType::meet(a, b),
            _ => return VariableType::Zero,
        };
        let rd = match (r1, r2) {
            (VariableType::Node(a), VariableType::Node(b)) => DescriptorType::meet(a, b),
            _ => return VariableType::Zero,
        };
        VariableType::EdgeDirectional {
            desc: DescriptorType::meet(d1, d2),
            left: Box::new(VariableType::Node(ld)),
            right: Box::new(VariableType::Node(rd)),
        }
    }

    pub fn meet(a: &VariableType, b: &VariableType) -> VariableType {
        super::stats::record_vt_meet();
        match (a, b) {
            (VariableType::Group(ta), VariableType::Group(tb)) => {
                VariableType::Group(Box::new(VariableType::meet(ta, tb)))
            }
            (VariableType::Node(da), VariableType::Node(db)) => Self::meet_node(da, db),
            (
                VariableType::EdgeDirectional {
                    desc: d1,
                    left: l1,
                    right: r1,
                },
                VariableType::EdgeDirectional {
                    desc: d2,
                    left: l2,
                    right: r2,
                },
            ) => Self::meet_edge_directional(d1, l1, r1, d2, l2, r2),
            (
                VariableType::EdgeNonDirectional {
                    desc: d1,
                    left: l1,
                    right: r1,
                },
                VariableType::EdgeNonDirectional {
                    desc: d2,
                    left: l2,
                    right: r2,
                },
            ) => {
                // Try both orientations, join the results
                let n1 = Self::meet_edge_directional(d1, l1, r1, d2, l2, r2);
                let n2 = Self::meet_edge_directional(d1, l1, r1, d2, r2, l2);
                match (&n1, &n2) {
                    (
                        VariableType::EdgeDirectional {
                            desc: da,
                            left: la,
                            right: ra,
                        },
                        VariableType::EdgeDirectional {
                            desc: db,
                            left: lb,
                            right: rb,
                        },
                    ) => VariableType::join(
                        VariableType::EdgeNonDirectional {
                            desc: da.clone(),
                            left: la.clone(),
                            right: ra.clone(),
                        },
                        VariableType::EdgeNonDirectional {
                            desc: db.clone(),
                            left: lb.clone(),
                            right: rb.clone(),
                        },
                    ),
                    _ => VariableType::Zero,
                }
            }
            (VariableType::Union(t1, t2), _) => {
                let r1 = VariableType::meet(t1, b);
                let r2 = VariableType::meet(t2, b);
                VariableType::join(r1, r2)
            }
            (_, VariableType::Union(_, _)) => VariableType::meet(b, a),
            (VariableType::Null, VariableType::Null) => VariableType::Null,
            (VariableType::Null, _) | (_, VariableType::Null) => VariableType::Zero,
            // Path only meets itself. A path variable is unique to its
            // operand and never shared across a comma-join/concat, so this
            // arm is reached only by a degenerate `p = ..., p = ...`.
            (VariableType::Path, VariableType::Path) => VariableType::Path,
            (VariableType::Zero, _) | (_, VariableType::Zero) => VariableType::Zero,
            _ => VariableType::Zero,
        }
    }

    // --- Join ---

    pub fn join(a: VariableType, b: VariableType) -> VariableType {
        super::stats::record_vt_join();
        if a == VariableType::Zero {
            return b;
        }
        if b == VariableType::Zero {
            return a;
        }
        if a == b {
            return a;
        }
        VariableType::Union(Box::new(a), Box::new(b))
    }

    pub fn join_from_list(types: Vec<VariableType>) -> VariableType {
        types.into_iter().fold(VariableType::Zero, Self::join)
    }

    // --- Subtyping ---

    /// Subtype check for the Node endpoints of an Edge variant.
    fn node_endpoint_subtype(a: &VariableType, b: &VariableType) -> bool {
        match (a, b) {
            (VariableType::Node(a), VariableType::Node(b)) => DescriptorType::is_subtype(a, b),
            _ => false,
        }
    }

    /// Subtype rule for a single orientation of an edge: descriptor
    /// subtype plus pointwise Node-endpoint subtype on left and right.
    /// Shared by both `EdgeDirectional` and the orientation-OR check
    /// of `EdgeNonDirectional`.
    fn edge_directional_subtype(
        d1: &DescriptorType,
        l1: &VariableType,
        r1: &VariableType,
        d2: &DescriptorType,
        l2: &VariableType,
        r2: &VariableType,
    ) -> bool {
        DescriptorType::is_subtype(d1, d2)
            && Self::node_endpoint_subtype(l1, l2)
            && Self::node_endpoint_subtype(r1, r2)
    }

    pub fn is_subtype(t1: &VariableType, t2: &VariableType) -> bool {
        match (t1, t2) {
            (VariableType::Zero, _) => true,
            (VariableType::Null, VariableType::Null) => true,
            (VariableType::Path, VariableType::Path) => true,
            (VariableType::Node(a), VariableType::Node(b)) => DescriptorType::is_subtype(a, b),
            (
                VariableType::EdgeDirectional {
                    desc: d1,
                    left: l1,
                    right: r1,
                },
                VariableType::EdgeDirectional {
                    desc: d2,
                    left: l2,
                    right: r2,
                },
            ) => Self::edge_directional_subtype(d1, l1, r1, d2, l2, r2),
            (
                VariableType::EdgeNonDirectional {
                    desc: d1,
                    left: l1,
                    right: r1,
                },
                VariableType::EdgeNonDirectional {
                    desc: d2,
                    left: l2,
                    right: r2,
                },
            ) => {
                Self::edge_directional_subtype(d1, l1, r1, d2, l2, r2)
                    || Self::edge_directional_subtype(d1, l1, r1, d2, r2, l2)
            }
            (VariableType::Group(a), VariableType::Group(b)) => VariableType::is_subtype(a, b),
            (VariableType::Union(a, b), _) => {
                VariableType::is_subtype(a, t2) || VariableType::is_subtype(b, t2)
            }
            (_, VariableType::Union(a, b)) => {
                VariableType::is_subtype(t1, a) || VariableType::is_subtype(t1, b)
            }
            _ => false,
        }
    }

    /// Refine a variable type against the schema and flatten the result into
    /// the concrete `Node` variants reachable. Mirrors fppc's
    /// `VariableType::refine_to_nodes` and is consumed by `PathType::meet`.
    pub fn refine_to_nodes(schema: &Schema, t: &VariableType) -> Vec<VariableType> {
        super::stats::record_refine_to_nodes();
        // Borrow-walk the (possibly cache-shared) refined tree and clone
        // only the matching Node leaves — a cache hit no longer deep-
        // clones the whole refined union up front.
        let refined = VariableType::refine_rc(schema, t);
        let mut out = Vec::new();
        let mut stack: Vec<&VariableType> = vec![&refined];
        while let Some(curr) = stack.pop() {
            match curr {
                VariableType::Node(_) => out.push(curr.clone()),
                VariableType::Union(t1, t2) => {
                    stack.push(t2);
                    stack.push(t1);
                }
                _ => {}
            }
        }
        out
    }

    // --- Refine ---

    /// By-value refine, kept for callers that need an owned result (and
    /// for the reference models in `lattice_proptest`). The scan arms
    /// delegate to [`VariableType::refine_rc`], so they share the memo.
    pub fn refine(schema: &Schema, node: &VariableType) -> VariableType {
        match node {
            VariableType::Node(_)
            | VariableType::EdgeDirectional { .. }
            | VariableType::EdgeNonDirectional { .. } => {
                (*VariableType::refine_rc(schema, node)).clone()
            }
            VariableType::Union(t1, t2) => VariableType::join(
                VariableType::refine(schema, t1),
                VariableType::refine(schema, t2),
            ),
            VariableType::Group(t) => {
                VariableType::Group(Box::new(VariableType::refine(schema, t)))
            }
            VariableType::Null => VariableType::Null,
            VariableType::Path => VariableType::Path,
            VariableType::Zero => VariableType::Zero,
        }
    }

    /// `Rc`-valued refine: on the Node/Edge scan arms a memo hit is a
    /// refcount bump instead of a deep clone of the refined tree. This is
    /// the form the checker and the environment operators consume — they
    /// store bindings as `Rc<VariableType>` anyway.
    pub fn refine_rc(schema: &Schema, node: &VariableType) -> Rc<VariableType> {
        match node {
            VariableType::Node(_) => {
                if !refine_cache_disabled() {
                    if let Some(hit) = schema.refine_cache_get(node) {
                        super::stats::record_refine_cache_hit();
                        return hit;
                    }
                }
                let matches = schema.scan_matches(true, &schema.nodes, node);
                let refined = Rc::new(VariableType::join_from_list(matches));
                if !refine_cache_disabled() {
                    schema.refine_cache_put(node.clone(), Rc::clone(&refined));
                }
                refined
            }
            VariableType::EdgeDirectional { .. } | VariableType::EdgeNonDirectional { .. } => {
                if !refine_cache_disabled() {
                    if let Some(hit) = schema.refine_cache_get(node) {
                        super::stats::record_refine_cache_hit();
                        return hit;
                    }
                }
                let matches = schema.scan_matches(false, &schema.edges, node);
                let refined = Rc::new(VariableType::join_from_list(matches));
                if !refine_cache_disabled() {
                    schema.refine_cache_put(node.clone(), Rc::clone(&refined));
                }
                refined
            }
            other => Rc::new(VariableType::refine(schema, other)),
        }
    }

    // --- Is empty ---

    pub fn is_empty(&self) -> bool {
        match self {
            VariableType::Zero => true,
            VariableType::Node(d) => d.is_empty(),
            VariableType::EdgeDirectional { desc, left, right }
            | VariableType::EdgeNonDirectional { desc, left, right } => {
                desc.is_empty() || left.is_empty() || right.is_empty()
            }
            VariableType::Union(t1, t2) => t1.is_empty() && t2.is_empty(),
            VariableType::Group(t) => t.is_empty(),
            VariableType::Null => false,
            // A path binding is always inhabited; it never empties an env.
            VariableType::Path => false,
        }
    }
}

impl fmt::Display for VariableType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VariableType::Node(d) => write!(f, "⸨{d}⸩"),
            VariableType::EdgeDirectional { desc, left, right } => {
                write!(f, "{left}-[{desc}]->{right}")
            }
            VariableType::EdgeNonDirectional { desc, left, right } => {
                write!(f, "{left}-[{desc}]-{right}")
            }
            VariableType::Union(t1, t2) => write!(f, "{t1} + {t2}"),
            VariableType::Group(t) => write!(f, "group<{t}>"),
            VariableType::Null => write!(f, "Null"),
            VariableType::Path => write!(f, "path"),
            VariableType::Zero => write!(f, "⊥"),
        }
    }
}

/// Schema: a set of allowed node and edge types.
///
/// `nodes` and `edges` are wrapped in `Rc` so `Clone` is cheap — every
/// `Typechecker::new` call clones the active Schema, and without the
/// `Rc` wrapping that clone deep-copies the entire descriptor tree.
///
/// The fields stay `pub` for read-side compatibility (callers iterate
/// or index via `&Rc<Vec<T>>` → `&Vec<T>` deref). Schemas are immutable
/// after construction — DDL replaces the whole Schema rather than
/// mutating in place, so there is no `Rc::make_mut` call site today.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub nodes: Rc<Vec<VariableType>>,
    pub edges: Rc<Vec<VariableType>>,
    /// Memo for `VariableType::refine`'s Node/Edge scan arms, keyed by the
    /// pattern type being refined. Shared across `Schema::clone` (so every
    /// `Typechecker::new(schema.clone())` reuses it — transparently
    /// cross-query for REPL/Connection lifetimes) and safely invalidated by
    /// construction: DDL and inference replace the whole `Schema`, never
    /// mutate one in place, so a cache can never outlive its entries.
    /// Values are `Rc` so a hit is a refcount bump, not a deep clone of the
    /// refined descriptor tree. Skipped by serde — a deserialized schema
    /// starts cold. `GQLITE_DISABLE_TC_REFINE_CACHE=1` bypasses it (A/B
    /// kill switch).
    #[serde(skip, default)]
    refine_cache: Rc<std::cell::RefCell<std::collections::HashMap<VariableType, Rc<VariableType>>>>,
    /// Descriptor interner: hash-conses `DescriptorType`s into dense `u32`
    /// ids for the lifetime of this Schema. `PathSummary` and the junction
    /// cache operate on ids, so their dedup/equality/hash work is integer
    /// arithmetic instead of walks over label trees and property maps —
    /// each distinct descriptor pays one hash at intern time. Ids are
    /// never recycled (no cap: growth is bounded by distinct descriptors
    /// seen against this schema, and DDL/inference replace the Schema).
    #[serde(skip, default)]
    interner: Rc<std::cell::RefCell<DescInterner>>,
    /// Memo for `PathSummary::meet`'s junction refinement: for a boundary
    /// id pair `(last, first)` the satisfiable refined junction node
    /// descriptors as interned ids. Same lifetime/invalidation story as
    /// `refine_cache`; cross-query AND cross-hop (chains reuse one
    /// junction at every position). `GQLITE_DISABLE_TC_JUNCTION_CACHE=1`
    /// bypasses it.
    #[serde(skip, default)]
    junction_cache: Rc<std::cell::RefCell<JunctionCache>>,
    /// Interner for whole `VariableType`s (refined types, met results,
    /// join results — the values environment bindings hold). Bindings
    /// carry a lazily-computed id so the lattice memos below can key by
    /// integer pair; each distinct type pays one hash at intern time.
    /// Ids are never recycled (bounded by distinct types seen against
    /// this schema; DDL/inference replace the whole Schema).
    #[serde(skip, default)]
    vt_interner: Rc<std::cell::RefCell<VtInterner>>,
    /// Memo for the environment-meet step `refine(meet(a, b))` keyed by
    /// interned type ids. `None` marks the collapse-error outcome (met
    /// to Zero with both sides non-empty) so the error path is memoized
    /// too — the message is regenerated from the operand types, which is
    /// what the uncached path formats as well.
    /// `GQLITE_DISABLE_TC_MEET_CACHE=1` bypasses both this and
    /// `join_cache`.
    #[serde(skip, default)]
    meet_refine_cache: Rc<std::cell::RefCell<MeetRefineCache>>,
    /// Memo for environment joins (`Γ₁ ⊔ Γ₂` arms and TLEFTJOIN's
    /// `T ⊔ T'`), keyed by interned id pair (order-sensitive, matching
    /// `join`'s structural asymmetry).
    #[serde(skip, default)]
    join_cache: Rc<std::cell::RefCell<JoinCache>>,
    /// Label buckets over the schema entries (`schema_index.rs`) — makes
    /// refine's *miss path* cheap: a star/neg-free query scans only the
    /// entries sharing a leaf label (plus the conservative fallback)
    /// instead of everything. Built lazily on first refine; the memos
    /// above make warm behavior identical with or without it.
    /// `GQLITE_DISABLE_TC_SCHEMA_INDEX=1` forces the full scan (A/B).
    #[serde(skip, default)]
    schema_index: Rc<std::cell::OnceCell<super::schema_index::SchemaIndex>>,
}

/// Junction memo: boundary id pair → refined junction descriptor ids.
type JunctionCache = std::collections::HashMap<(u32, u32), Rc<Vec<u32>>>;

/// Backing store for [`Schema`]'s variable-type interner.
#[derive(Debug, Default)]
struct VtInterner {
    ids: std::collections::HashMap<VariableType, u32>,
    vts: Vec<Rc<VariableType>>,
}

/// Env-meet memo: `(a, b)` ids → `refine(meet(a,b))` or the collapse
/// marker.
type MeetRefineCache = std::collections::HashMap<(u32, u32), Option<(u32, Rc<VariableType>)>>;

/// Env-join memo: `(a, b)` ids → `join(a, b)`.
type JoinCache = std::collections::HashMap<(u32, u32), (u32, Rc<VariableType>)>;

/// Backing store for [`Schema`]'s descriptor interner.
#[derive(Debug, Default)]
struct DescInterner {
    ids: std::collections::HashMap<DescriptorType, u32>,
    descs: Vec<Rc<DescriptorType>>,
}

/// Safety valve for adversarial/degenerate sessions: the cache resets when
/// it grows past this many distinct pattern descriptors (real queries stay
/// in the dozens).
const REFINE_CACHE_CAP: usize = 4096;

impl Schema {
    /// Permissive schema that allows anything.
    pub fn star() -> Self {
        Schema {
            nodes: Rc::new(vec![VariableType::node_star()]),
            edges: Rc::new(vec![
                VariableType::edge_directional(DescriptorType::star()),
                VariableType::edge_non_directional(DescriptorType::star()),
            ]),
            refine_cache: Rc::default(),
            interner: Rc::default(),
            junction_cache: Rc::default(),
            vt_interner: Rc::default(),
            meet_refine_cache: Rc::default(),
            join_cache: Rc::default(),
            schema_index: Rc::default(),
        }
    }

    /// Construct from explicit nodes/edges. Used by inference and tests.
    pub fn from_parts(nodes: Vec<VariableType>, edges: Vec<VariableType>) -> Self {
        Schema {
            nodes: Rc::new(nodes),
            edges: Rc::new(edges),
            refine_cache: Rc::default(),
            interner: Rc::default(),
            junction_cache: Rc::default(),
            vt_interner: Rc::default(),
            meet_refine_cache: Rc::default(),
            join_cache: Rc::default(),
            schema_index: Rc::default(),
        }
    }

    /// Intern a shared variable type, returning its dense id. The first
    /// `Rc` seen for a value becomes the canonical allocation handed back
    /// by `vt_of`, which keeps downstream `Rc::ptr_eq` fast paths hitting.
    pub(crate) fn intern_vt_rc(&self, t: &Rc<VariableType>) -> u32 {
        let mut i = self.vt_interner.borrow_mut();
        if let Some(&id) = i.ids.get(&**t) {
            return id;
        }
        let id = i.vts.len() as u32;
        i.vts.push(Rc::clone(t));
        i.ids.insert((**t).clone(), id);
        id
    }

    /// Resolve an interned variable-type id to its canonical `Rc`.
    pub(crate) fn vt_of(&self, id: u32) -> Rc<VariableType> {
        Rc::clone(&self.vt_interner.borrow().vts[id as usize])
    }

    /// A schema over the same entries with EMPTY memo caches but shared
    /// interners and label index. This is the honest "cold" A/B tool for
    /// benches and tests: memos are per-session *warmth*, while the
    /// interners (stable ids) and the label index (build-once structure,
    /// like the runtime's TripleIndex) are part of the schema itself —
    /// a session's first sighting of a shape pays a memo miss, not an
    /// index rebuild.
    pub fn fresh_caches(&self) -> Schema {
        Schema {
            nodes: Rc::clone(&self.nodes),
            edges: Rc::clone(&self.edges),
            refine_cache: Rc::default(),
            interner: Rc::clone(&self.interner),
            junction_cache: Rc::default(),
            vt_interner: Rc::clone(&self.vt_interner),
            meet_refine_cache: Rc::default(),
            join_cache: Rc::default(),
            schema_index: Rc::clone(&self.schema_index),
        }
    }

    /// The candidate entries of `entries` matching `query`, met against
    /// it — refine's scan body. Label-index-pruned when the query's label
    /// tree is bucket-servable (`schema_index.rs`); the candidate list is
    /// ascending, so survivors fold in the same order as the full scan
    /// and the refined result is bit-identical.
    fn scan_matches(
        &self,
        nodes: bool,
        entries: &[VariableType],
        query: &VariableType,
    ) -> Vec<VariableType> {
        let cands = if schema_index_disabled() {
            None
        } else {
            let idx = self
                .schema_index
                .get_or_init(|| super::schema_index::SchemaIndex::build(&self.nodes, &self.edges));
            query
                .descriptor()
                .and_then(|d| idx.candidates(nodes, &d.label))
        };
        match cands {
            Some(ids) => {
                if nodes {
                    super::stats::record_refine_node_scan(ids.len());
                } else {
                    super::stats::record_refine_edge_scan(ids.len());
                }
                ids.iter()
                    .map(|&i| &entries[i as usize])
                    .filter(|e| VariableType::is_subtype(e, query))
                    .map(|e| VariableType::meet(e, query))
                    .collect()
            }
            None => {
                if nodes {
                    super::stats::record_refine_node_scan(entries.len());
                } else {
                    super::stats::record_refine_edge_scan(entries.len());
                }
                entries
                    .iter()
                    .filter(|e| VariableType::is_subtype(e, query))
                    .map(|e| VariableType::meet(e, query))
                    .collect()
            }
        }
    }

    /// The environment-meet step `refine(meet(a, b))`, memoized by id
    /// pair. `None` is the collapse-error outcome (`met == Zero` with
    /// both operands non-empty); callers format the same message the
    /// uncached path did, from the operand types.
    pub(crate) fn meet_refined(
        &self,
        ia: u32,
        a: &Rc<VariableType>,
        ib: u32,
        b: &Rc<VariableType>,
    ) -> Option<(u32, Rc<VariableType>)> {
        let cache_on = !meet_cache_disabled();
        if cache_on {
            if let Some(hit) = self.meet_refine_cache.borrow().get(&(ia, ib)) {
                return hit.clone();
            }
        }
        let met = VariableType::meet(a, b);
        let out = if met == VariableType::Zero && !a.is_empty() && !b.is_empty() {
            None
        } else {
            let refined = VariableType::refine_rc(self, &met);
            let rid = self.intern_vt_rc(&refined);
            Some((rid, self.vt_of(rid)))
        };
        if cache_on {
            let mut m = self.meet_refine_cache.borrow_mut();
            if m.len() >= REFINE_CACHE_CAP {
                m.clear();
            }
            m.insert((ia, ib), out.clone());
        }
        out
    }

    /// `join(a, b)` memoized by id pair (order-sensitive: `join` is
    /// structurally asymmetric). Returns the canonical interned `Rc`.
    pub(crate) fn join_interned(
        &self,
        ia: u32,
        a: &Rc<VariableType>,
        ib: u32,
        b: &Rc<VariableType>,
    ) -> (u32, Rc<VariableType>) {
        let cache_on = !meet_cache_disabled();
        if cache_on {
            if let Some(hit) = self.join_cache.borrow().get(&(ia, ib)) {
                return hit.clone();
            }
        }
        let joined = Rc::new(VariableType::join((**a).clone(), (**b).clone()));
        let jid = self.intern_vt_rc(&joined);
        let out = (jid, self.vt_of(jid));
        if cache_on {
            let mut m = self.join_cache.borrow_mut();
            if m.len() >= REFINE_CACHE_CAP {
                m.clear();
            }
            m.insert((ia, ib), out.clone());
        }
        out
    }

    /// Intern a descriptor, returning its dense id for this Schema.
    pub(crate) fn intern_desc(&self, d: &DescriptorType) -> u32 {
        let mut i = self.interner.borrow_mut();
        if let Some(&id) = i.ids.get(d) {
            return id;
        }
        let id = i.descs.len() as u32;
        i.descs.push(Rc::new(d.clone()));
        i.ids.insert(d.clone(), id);
        id
    }

    /// Resolve an interned descriptor id back to the descriptor.
    pub(crate) fn desc_of(&self, id: u32) -> Rc<DescriptorType> {
        Rc::clone(&self.interner.borrow().descs[id as usize])
    }

    fn refine_cache_get(&self, key: &VariableType) -> Option<Rc<VariableType>> {
        self.refine_cache.borrow().get(key).map(Rc::clone)
    }

    fn refine_cache_put(&self, key: VariableType, value: Rc<VariableType>) {
        let mut m = self.refine_cache.borrow_mut();
        if m.len() >= REFINE_CACHE_CAP {
            m.clear();
        }
        m.insert(key, value);
    }

    /// The satisfiable refined junction descriptors for a boundary id
    /// pair — `refine_to_nodes(meet(last, first))` flattened to interned
    /// ids, memoized per schema (see `junction_cache`).
    pub(crate) fn junction_ids(&self, last: u32, first: u32) -> Rc<Vec<u32>> {
        let cache_on = !junction_cache_disabled();
        if cache_on {
            if let Some(hit) = self.junction_cache.borrow().get(&(last, first)) {
                return Rc::clone(hit);
            }
        }
        let last_d = self.desc_of(last);
        let first_d = self.desc_of(first);
        let met = VariableType::Node(DescriptorType::meet(&last_d, &first_d));
        let rs: Rc<Vec<u32>> = Rc::new(
            VariableType::refine_to_nodes(self, &met)
                .into_iter()
                .filter_map(|v| match v {
                    VariableType::Node(d) => Some(self.intern_desc(&d)),
                    _ => None,
                })
                .collect(),
        );
        if cache_on {
            let mut m = self.junction_cache.borrow_mut();
            if m.len() >= REFINE_CACHE_CAP {
                m.clear();
            }
            m.insert((last, first), Rc::clone(&rs));
        }
        rs
    }
}

fn refine_cache_disabled() -> bool {
    std::env::var("GQLITE_DISABLE_TC_REFINE_CACHE").is_ok()
}

fn junction_cache_disabled() -> bool {
    std::env::var("GQLITE_DISABLE_TC_JUNCTION_CACHE").is_ok()
}

fn meet_cache_disabled() -> bool {
    std::env::var("GQLITE_DISABLE_TC_MEET_CACHE").is_ok()
}

fn schema_index_disabled() -> bool {
    std::env::var("GQLITE_DISABLE_TC_SCHEMA_INDEX").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typing::label_type::LabelType;
    use crate::typing::property_type::PropertyType;

    fn node_with_label(name: &str) -> VariableType {
        VariableType::Node(DescriptorType::new(
            LabelType::Label(name.into()),
            PropertyType::open_empty(),
        ))
    }

    // is_empty — note: Edge variants use OR over (desc, left, right),
    // different from Union which uses AND.
    #[test]
    fn test_zero_is_empty() {
        assert!(VariableType::Zero.is_empty());
    }
    #[test]
    fn test_node_empty_iff_descriptor_empty() {
        let empty_d = DescriptorType::new(LabelType::Star, PropertyType::Zero);
        assert!(VariableType::Node(empty_d).is_empty());
        assert!(!VariableType::node_star().is_empty());
    }
    #[test]
    fn test_edge_directional_empty_iff_any_component_empty() {
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
    fn test_union_empty_iff_both_empty() {
        // Sanity: complement to Edge's OR semantics.
        let zero = || Box::new(VariableType::Zero);
        let n = || Box::new(VariableType::node_star());
        assert!(VariableType::Union(zero(), zero()).is_empty());
        assert!(!VariableType::Union(zero(), n()).is_empty());
        assert!(!VariableType::Union(n(), zero()).is_empty());
    }

    // join — Zero is identity, equal-collapse.
    #[test]
    fn test_join_drops_left_zero() {
        let n = VariableType::node_star();
        assert_eq!(VariableType::join(VariableType::Zero, n.clone()), n);
    }
    #[test]
    fn test_join_drops_right_zero() {
        let n = VariableType::node_star();
        assert_eq!(VariableType::join(n.clone(), VariableType::Zero), n);
    }
    #[test]
    fn test_join_collapses_equal_operands() {
        let n = VariableType::node_star();
        assert_eq!(VariableType::join(n.clone(), n.clone()), n);
    }

    // meet — Node preservation + descriptor combination.
    #[test]
    fn test_meet_same_node_returns_same() {
        let n = node_with_label("Person");
        assert_eq!(VariableType::meet(&n, &n), n);
    }
    #[test]
    fn test_meet_distinct_node_atoms_collapses_descriptor() {
        // meet of two label-distinct Nodes should produce a Node whose
        // descriptor's label is the And.
        let na = node_with_label("A");
        let nb = node_with_label("B");
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

    // refine — schema admission.
    #[test]
    fn test_refine_with_no_matching_label_returns_zero() {
        // Schema with only `Person`; query for `Animal` → ⊥.
        let schema = Schema::from_parts(vec![node_with_label("Person")], vec![]);
        let q = node_with_label("Animal");
        assert_eq!(VariableType::refine(&schema, &q), VariableType::Zero);
    }
}
