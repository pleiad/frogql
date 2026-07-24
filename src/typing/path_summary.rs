//! Boundary-pair summary of a path type — the checker's evaluation
//! representation.
//!
//! The inductive `PathType` (`path_type.rs`, the paper's definition) stores
//! every interior node of every union arm, and its `meet` clones the whole
//! prefix per refined junction arm. On patterns whose positions refine into
//! wide unions (unlabeled chains against a real schema) the tree grows
//! multiplicatively per hop — `()-[]->()` ×16 on LDBC SF0.1 measured ~23 s
//! per check (2026-07 baseline), with memory growth severe enough to push
//! an 8 GiB machine toward swap (peak RSS was not instrumented).
//!
//! The observation that fixes it: once a segment is met and pruned, its
//! interior nodes are never consulted again. Every judgment the checker
//! makes — `is_unsatisfiable` (the `guaranteed_empty` source),
//! `is_empty`/`len` (the repeat-length checks), and further concatenation
//! `meet`s (which only join the left's *last* node with the right's
//! *first*) — factors through the path's **boundary summary**:
//!
//! * `nodes` — the zero-length arms. A zero-length arm is a single node,
//!   so first ≡ last and a descriptor set suffices. Kept separate from
//!   `pairs` because zero-length arms concatenate differently: their node
//!   IS the junction, so the refined junction becomes the exposed
//!   boundary.
//! * `pairs` — the edge-bearing arms as `(first, last, min edge count)`
//!   (count ≥ 1). Arms sharing a boundary are interchangeable under every
//!   further operation except the length judgment, for which the minimum
//!   is exactly what `len()` observes.
//!
//! Width is bounded by |schema.nodes|² + |schema.nodes| regardless of
//! pattern length, so concatenation cost is independent of chain position
//! and the exponential disappears.
//!
//! Representation note: arms live in flat `Vec`s deduplicated by linear
//! equality scan, NOT hash sets. Queries users actually type summarize to
//! 1–3 arms, where a hash structure loses twice: allocation/setup per
//! operation, and hashing must walk the *entire* rich descriptor
//! (label + property `BTreeMap`) on every insert and lookup, while an
//! equality probe fails fast on the label. The linear scan is O(w²) in
//! arm width, bounded by the schema as above; the synthetic unlabeled
//! families are cliff guards, not tuning targets. Set semantics are
//! preserved by a manual order-insensitive `PartialEq`.
//!
//! Correspondence with the spec type: `PathSummary` is the image of the
//! abstraction `summarize : PathType → PathSummary`, and the lattice
//! operations commute with it —
//! `summarize(meet(p, q)) = meet(summarize(p), summarize(q))` (same for
//! `union`) — with `is_unsatisfiable`/`is_empty`/`len` agreeing with the
//! satisfiability-aware ("live") reading of the spec type. Pinned by
//! `tests/path_summary_hom_proptest.rs`. One deliberate divergence: the
//! spec type's own `len`/`is_empty` are satisfiability-blind on dead arms
//! (`Edge{p1: Zero, ..}` counts as length 1), which made the repeat-length
//! warning fire or not depending on *how* an inner pattern died; the
//! summary implements the live semantics uniformly (a dead inner is
//! "empty", matching the old `Zero` branch).
//!
//! Invariant relied on (holds for every checker-reachable path): schema
//! entries carry non-empty descriptors, and `refine`/`refine_to_nodes`
//! outputs are meets of matching entries, so satisfiable arms never carry
//! empty descriptors — unsatisfiability always surfaces as the absence of
//! arms.

use super::descriptor_type::DescriptorType;
use super::path_type::{EdgeDir, PathType};
use super::variable_type::{Schema, VariableType};

/// Boundary summary: zero-length arms as a node-descriptor set, edge-
/// bearing arms as `(first, last, min edge count)`. Both empty ⇔ the
/// path is unsatisfiable (`PathType::Zero`). Set semantics with `Vec`
/// storage — see the module's representation note.
#[derive(Debug, Clone, Default)]
pub struct PathSummary {
    nodes: Vec<DescriptorType>,
    pairs: Vec<(DescriptorType, DescriptorType, usize)>,
}

/// Order-insensitive set equality (arm order is an artifact of
/// construction order, never meaningful).
impl PartialEq for PathSummary {
    fn eq(&self, other: &Self) -> bool {
        self.nodes.len() == other.nodes.len()
            && self.pairs.len() == other.pairs.len()
            && self.nodes.iter().all(|d| other.nodes.contains(d))
            && self.pairs.iter().all(|(f, l, n)| {
                other
                    .pairs
                    .iter()
                    .any(|(f2, l2, n2)| f == f2 && l == l2 && n == n2)
            })
    }
}
impl Eq for PathSummary {}

impl PathSummary {
    /// Bottom — no satisfiable arm. Mirrors `PathType::Zero`.
    pub fn zero() -> Self {
        PathSummary::default()
    }

    /// A single-node path. Mirrors `PathType::Node`.
    pub fn node(desc: DescriptorType) -> Self {
        PathSummary {
            nodes: vec![desc],
            pairs: Vec::new(),
        }
    }

    /// Mirrors `PathType::default()`: the anonymous star node.
    pub fn star_node() -> Self {
        PathSummary::node(DescriptorType::star())
    }

    /// Mirrors `PathType::from_variable`: build from a (refined) variable
    /// type and the direction the edge is observed at.
    pub fn from_variable(t: &VariableType, dir: EdgeDir) -> Self {
        match t {
            VariableType::Node(d) => PathSummary::node(d.clone()),
            VariableType::EdgeDirectional { left, right, .. } => {
                directed_edge_pairs(left, right, dir)
            }
            // Undirected: both orientations regardless of dir (as the
            // spec's `from_variable` does).
            VariableType::EdgeNonDirectional { left, right, .. } => {
                directed_edge_pairs(left, right, EdgeDir::Any)
            }
            VariableType::Union(t1, t2) => PathSummary::union(
                PathSummary::from_variable(t1, dir),
                PathSummary::from_variable(t2, dir),
            ),
            VariableType::Group(_)
            | VariableType::Null
            | VariableType::Path
            | VariableType::Zero => PathSummary::zero(),
        }
    }

    /// Minimum edge count over live arms (0 when unsatisfiable, matching
    /// `PathType::Zero::len()`).
    pub fn len(&self) -> usize {
        if !self.nodes.is_empty() {
            return 0;
        }
        self.pairs.iter().map(|&(_, _, n)| n).min().unwrap_or(0)
    }

    /// True when some live arm has no edges — or when there is no live
    /// arm at all (mirrors the spec: both `Node` and `Zero` are empty).
    pub fn is_empty(&self) -> bool {
        !self.nodes.is_empty() || self.pairs.is_empty()
    }

    /// Bottom check — no live arm remains.
    pub fn is_unsatisfiable(&self) -> bool {
        self.nodes.is_empty() && self.pairs.is_empty()
    }

    fn insert_node(&mut self, d: DescriptorType) {
        if !self.nodes.contains(&d) {
            self.nodes.push(d);
        }
    }

    fn insert_pair(&mut self, f: DescriptorType, l: DescriptorType, len: usize) {
        debug_assert!(len >= 1, "edge-bearing pair with zero length");
        for (f2, l2, n) in self.pairs.iter_mut() {
            if *f2 == f && *l2 == l {
                *n = (*n).min(len);
                return;
            }
        }
        self.pairs.push((f, l, len));
    }

    /// Least upper bound: arm-set union (min length per edge boundary).
    /// Mirrors `PathType::union` (Zero is identity).
    pub fn union(a: PathSummary, b: PathSummary) -> PathSummary {
        let (mut big, small) = if a.nodes.len() + a.pairs.len() >= b.nodes.len() + b.pairs.len() {
            (a, b)
        } else {
            (b, a)
        };
        for d in small.nodes {
            big.insert_node(d);
        }
        for (f, l, n) in small.pairs {
            big.insert_pair(f, l, n);
        }
        big
    }

    /// Greatest lower bound — path concatenation joining `a`'s last node
    /// with `b`'s first node, exactly as `PathType::meet` does: the
    /// junction descriptors are met and re-refined against the schema
    /// (`refine_to_nodes`). A zero-length side's node IS the junction, so
    /// it adopts the refined junction as its exposed boundary; a
    /// positive-length side keeps its own outer boundary (the junction
    /// becomes interior and is dropped).
    pub fn meet(schema: &Schema, a: &PathSummary, b: &PathSummary) -> PathSummary {
        super::stats::record_pathtype_meet();
        let mut out = PathSummary::zero();
        // The junction only depends on (a.last, b.first). Memoized by
        // pointer identity of the operand descriptors — w×w arm combos
        // cost w distinct refinements; a value-equal miss just recomputes.
        let mut junctions: Vec<(
            *const DescriptorType,
            *const DescriptorType,
            Vec<DescriptorType>,
        )> = Vec::new();
        let mut junction = |l1: &DescriptorType, f2: &DescriptorType| -> Vec<DescriptorType> {
            let key = (l1 as *const _, f2 as *const _);
            if let Some((_, _, rs)) = junctions.iter().find(|(p1, p2, _)| (*p1, *p2) == key) {
                return rs.clone();
            }
            let met = VariableType::Node(DescriptorType::meet(l1, f2));
            let rs: Vec<DescriptorType> = VariableType::refine_to_nodes(schema, &met)
                .into_iter()
                .filter_map(|v| match v {
                    VariableType::Node(d) => Some(d),
                    _ => None,
                })
                .collect();
            junctions.push((key.0, key.1, rs.clone()));
            rs
        };

        // pairs × pairs: junction interior, outer boundaries survive.
        for (f1, l1, len1) in &a.pairs {
            for (f2, l2, len2) in &b.pairs {
                if !junction(l1, f2).is_empty() {
                    out.insert_pair(f1.clone(), l2.clone(), len1 + len2);
                }
            }
        }
        // pairs × nodes: the zero-length right is the junction; the
        // refined junction becomes the result's last boundary.
        for (f1, l1, len1) in &a.pairs {
            for d in &b.nodes {
                for r in junction(l1, d) {
                    out.insert_pair(f1.clone(), r, *len1);
                }
            }
        }
        // nodes × pairs: symmetric — refined junction becomes first.
        for d in &a.nodes {
            for (f2, l2, len2) in &b.pairs {
                for r in junction(d, f2) {
                    out.insert_pair(r, l2.clone(), *len2);
                }
            }
        }
        // nodes × nodes: both are the junction; result is the refined
        // node itself.
        for d1 in &a.nodes {
            for d2 in &b.nodes {
                for r in junction(d1, d2) {
                    out.insert_node(r);
                }
            }
        }
        out
    }

    /// `p^0` = the identity node path, `p^n = meet(p, p^{n-1})`. Mirrors
    /// the checker's `pow_path_type`.
    pub fn pow(schema: &Schema, p: &PathSummary, n: u64) -> PathSummary {
        match n {
            0 => PathSummary::star_node(),
            1 => p.clone(),
            _ => PathSummary::meet(schema, p, &PathSummary::pow(schema, p, n - 1)),
        }
    }

    /// Abstraction function from the spec type. `Edge` exposes its `n2`
    /// as the last boundary and discards the prefix's own last boundary
    /// (it became interior when the edge was appended); a dead arm
    /// (empty descriptor or `Zero` prefix) contributes nothing, giving
    /// the live semantics documented above.
    pub fn summarize(p: &PathType) -> PathSummary {
        match p {
            PathType::Zero => PathSummary::zero(),
            PathType::Node(n) => {
                if n.desc.is_empty() {
                    PathSummary::zero()
                } else {
                    PathSummary::node(n.desc.clone())
                }
            }
            PathType::Edge(e) => {
                if e.n2.desc.is_empty() {
                    return PathSummary::zero();
                }
                let prefix = PathSummary::summarize(&e.p1);
                let mut out = PathSummary::zero();
                for d in &prefix.nodes {
                    out.insert_pair(d.clone(), e.n2.desc.clone(), 1);
                }
                for (f, _, len) in &prefix.pairs {
                    out.insert_pair(f.clone(), e.n2.desc.clone(), len + 1);
                }
                out
            }
            PathType::Union(p1, p2) => {
                PathSummary::union(PathSummary::summarize(p1), PathSummary::summarize(p2))
            }
        }
    }
}

/// Mirrors `path_type.rs::directed_edge_to_path` in pair form.
fn directed_edge_pairs(left: &VariableType, right: &VariableType, dir: EdgeDir) -> PathSummary {
    let (l, r) = match (left, right) {
        (VariableType::Node(l), VariableType::Node(r)) => (l, r),
        _ => return PathSummary::zero(),
    };
    let mut out = PathSummary::zero();
    if matches!(dir, EdgeDir::Right | EdgeDir::Any | EdgeDir::None) {
        out.insert_pair(l.clone(), r.clone(), 1);
    }
    if matches!(dir, EdgeDir::Left | EdgeDir::Any | EdgeDir::None) {
        out.insert_pair(r.clone(), l.clone(), 1);
    }
    out
}
