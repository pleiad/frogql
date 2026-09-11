//! The triples a session has changed since its LTJ index was built.
//!
//! # Why this exists
//!
//! Every successful DML statement used to drop the cached `TripleIndex`,
//! so the next query rebuilt all six orderings from the whole graph.
//! Building is `O(E log E)` — 670 ms at LDBC SF0.1 and **252 seconds,
//! measured, on a 617 M-edge RDF dump** — while an `INSERT` changes a
//! handful of edges. Paying the second cost for the first change is the
//! thing this module removes.
//!
//! # Why an overlay and not an edit
//!
//! The default representation is six LOUDS succinct tries with sampled
//! select. They are static: there is no insert. So "incremental" cannot
//! mean "edit the index", and the only shape left is a static base plus a
//! small second index consulted alongside it — which is what the store
//! already does for the graph itself (`MutationOverlay`). The cost moves
//! to the query path, where every `leap` asks two sources instead of one;
//! `DeltaLtjIterator` is where that merge happens.
//!
//! # What a delta is computed against
//!
//! Not the on-disk file: `TripleIndex::from_graph` reads the *merged*
//! base + overlay view, so an index built mid-session already contains
//! whatever the overlay held at that moment. The index therefore records
//! that moment — `OverlayStamp`, the next edge id the overlay would hand
//! out and the set of ids already tombstoned — and a refresh reports only
//! what happened since. Refreshes recompute from the stamp rather than
//! from each other, so applying one twice is the same as applying it once.

use std::collections::HashSet;

use crate::model::graph_access::GraphAccess;
use crate::model::value::Id;

use super::triple_index::{IndexEntry, TripleIndex};

/// Where the overlay stood when an index was built.
///
/// Overlay edge ids are handed out in increasing order and never reused,
/// so a single watermark identifies every edge added since. Deletions
/// have no such order, so the set is kept whole — it is session-sized,
/// and empty in the case that matters (an index built at open).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayStamp {
    pub next_edge_id: Id,
    pub deleted: HashSet<Id>,
}

/// What a store's overlay currently holds, as the delta builder needs it.
///
/// `None` from `GraphAccess::edge_mutations` means the backend has no
/// overlay at all, which is the honest answer for a read-only disk store
/// and the reason a refresh falls back to a full rebuild there.
#[derive(Debug, Clone, Default)]
pub struct EdgeMutations {
    /// Live edges the overlay allocated, in id order, each with whether
    /// it is directed. The flag rides along because `GraphAccess` has no
    /// way to ask — `edges_directed()` / `edges_undirected()` return the
    /// whole graph's lists, which is exactly the `O(E)` walk this module
    /// exists to avoid.
    pub live_overlay: Vec<(Id, bool)>,
    /// Every tombstoned edge id, base-image or overlay-allocated.
    pub deleted: HashSet<Id>,
    /// The id the next overlay edge would be given.
    pub next_edge_id: Id,
}

/// Triples added since the base was built, plus the edge ids removed.
///
/// The additions are six sorted arrays whatever the base representation
/// is. They are small by construction and rebuilt per version, so the
/// succinct representation would cost build time to save space that is
/// not being spent.
#[derive(Debug, Clone, Default)]
pub struct TripleDelta {
    added: [Vec<IndexEntry>; 6],
    removed: HashSet<u32>,
}

impl TripleDelta {
    pub fn orderings(&self) -> &[Vec<IndexEntry>; 6] {
        &self.added
    }

    pub fn removed(&self) -> &HashSet<u32> {
        &self.removed
    }

    /// Nothing to merge: the index answers exactly as the base does, and
    /// `LtjIterator::new` skips the merging iterator entirely.
    pub fn is_empty(&self) -> bool {
        self.added[0].is_empty() && self.removed.is_empty()
    }

    /// How many triples the additions carry. The refresh path uses this
    /// to decide when a delta has grown past the point where merging is
    /// still cheaper than rebuilding.
    pub fn added_len(&self) -> usize {
        self.added[0].len()
    }

    /// Build the delta between `stamp` and what the store holds now.
    ///
    /// `labels` is the index's dictionary, extended in place: an inserted
    /// edge can carry a label the base never saw, and its id has to
    /// continue the base's numbering or the two sides would disagree
    /// about what a predicate constant means.
    pub fn build<G: GraphAccess>(
        graph: &G,
        stamp: &OverlayStamp,
        now: &EdgeMutations,
        labels: &mut LabelDict,
        mirror_directed: bool,
    ) -> Self {
        let mut raw: Vec<IndexEntry> = Vec::new();
        for &(eid, directed) in &now.live_overlay {
            if eid < stamp.next_edge_id {
                // Already in the base: it existed when the index was built
                // and is alive now.
                continue;
            }
            let src = graph.src(eid);
            let tgt = graph.tgt(eid);
            let labels_of = graph.edge_labels(eid);
            let names = labels_of.required_labels();
            let mut push = |lid: u32| {
                raw.push((src, lid, tgt, eid));
                // The same two rules `from_graph_impl` applies: an
                // undirected edge is stored in both senses so a forward
                // lookup finds it from either endpoint, and the mirrored
                // index stores every edge both ways.
                if (mirror_directed || !directed) && (mirror_directed || src != tgt) {
                    raw.push((tgt, lid, src, eid));
                }
            };
            if names.is_empty() {
                push(labels.id_for(""));
            } else {
                for n in names {
                    push(labels.id_for(n));
                }
            }
        }

        let added = [
            sorted_by(&raw, |&(s, p, o, e)| (s, p, o, e)),
            sorted_by(&raw, |&(s, p, o, e)| (s, o, p, e)),
            sorted_by(&raw, |&(s, p, o, e)| (p, o, s, e)),
            sorted_by(&raw, |&(s, p, o, e)| (p, s, o, e)),
            sorted_by(&raw, |&(s, p, o, e)| (o, s, p, e)),
            sorted_by(&raw, |&(s, p, o, e)| (o, p, s, e)),
        ];

        // Only deletions the base does not already know about. One that
        // was already tombstoned when the index was built never reached
        // the index in the first place.
        let removed: HashSet<u32> = now
            .deleted
            .iter()
            .filter(|e| !stamp.deleted.contains(e))
            .copied()
            .collect();

        TripleDelta { added, removed }
    }
}

fn sorted_by(
    raw: &[IndexEntry],
    key: impl Fn(&IndexEntry) -> (u32, u32, u32, u32),
) -> Vec<IndexEntry> {
    let mut v: Vec<IndexEntry> = raw.iter().map(&key).collect();
    v.sort_unstable();
    v
}

/// The label dictionary of an index, borrowed so a delta can extend it.
///
/// Ids are positions in `id_to_label`, so appending is the only safe way
/// to add one: renumbering would invalidate every triple already in the
/// base.
pub struct LabelDict<'a> {
    pub to_id: &'a mut std::collections::HashMap<String, u32>,
    pub to_label: &'a mut Vec<String>,
}

impl LabelDict<'_> {
    fn id_for(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.to_id.get(name) {
            return id;
        }
        let id = self.to_label.len() as u32;
        self.to_label.push(name.to_string());
        self.to_id.insert(name.to_string(), id);
        id
    }
}

/// A delta past this many triples is not worth merging: every `leap`
/// pays for the second source, and a full rebuild amortises better than
/// a session-long overlay that keeps growing. Past it, the refresh drops
/// the index and the next query builds one.
///
/// The number is a guess, not a measurement, and it is deliberately
/// generous — a bulk load through DML is the shape that reaches it, and
/// an ordinary editing session never will. It is also the bound on how
/// much work a refresh does, since the delta is rebuilt from the stamp
/// rather than appended to.
pub const MAX_DELTA_TRIPLES: usize = 200_000;

/// Whether this build maintains deltas at all.
/// `FROGQL_DISABLE_LTJ_DELTA=1` restores the old behaviour — every DML
/// drops the index and the next query rebuilds it — which is what the
/// differential suite A/Bs against.
pub fn delta_disabled() -> bool {
    std::env::var("FROGQL_DISABLE_LTJ_DELTA").is_ok_and(|v| v == "1")
}

/// The index `base` describes, brought up to date with the store.
///
/// `None` means "no usable delta, rebuild instead": the backend has no
/// overlay to read, or the delta has outgrown `MAX_DELTA_TRIPLES`.
pub fn refresh<G: GraphAccess>(base: &TripleIndex, graph: &G) -> Option<TripleIndex> {
    if delta_disabled() {
        return None;
    }
    let now = graph.edge_mutations()?;
    let mut to_id = base.label_to_id.clone();
    let mut to_label = base.id_to_label.clone();
    let delta = TripleDelta::build(
        graph,
        base.overlay_stamp(),
        &now,
        &mut LabelDict {
            to_id: &mut to_id,
            to_label: &mut to_label,
        },
        base.is_mirrored(),
    );
    if delta.added_len() > MAX_DELTA_TRIPLES {
        return None;
    }
    Some(base.rebased(delta, to_id, to_label))
}
