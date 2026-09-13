//! Secondary-index delta over the mutation overlay.
//!
//! The `SecondaryIndex` is built from the on-disk records at open and is
//! never updated, so a staged mutation makes its answer wrong. The store's
//! response used to be to decline the index entirely once anything was
//! staged, which is correct and catastrophic: one `INSERT` sent every later
//! lookup of the session to a full label scan with a per-node property
//! decode. Measured on `examples/fraud_detection.gdb` (1 200 `ACCOUNT`
//! nodes), a point lookup went from 0.0 ms to 2–4 ms after one unrelated
//! node insert, permanently; on a 26 k-node graph the same shape cost 64 ms,
//! which makes loading by `INSERT` quadratic.
//!
//! This module holds the other half of the answer, the same shape as
//! `runtime::ltj::delta`: keep the base index, maintain a small delta beside
//! it, and merge at read time. A lookup becomes
//!
//! ```text
//! base hits  −  {deleted}  −  {shadowed}  +  overlay hits
//! ```
//!
//! *Shadowed* is the subtle half. A base node the overlay has touched may no
//! longer hold the value the base index filed it under, so it must be
//! dropped from every base answer — not only for the property that changed,
//! since one `HashSet<Id>` cannot say which. The delta therefore re-indexes
//! **all** of a touched node's indexed pairs, so whatever still matches
//! comes back through the overlay half.
//!
//! Only the `(label, prop)` pairs the base index already covers are indexed
//! here. A lookup on any other pair returns `None` from the base index and
//! scans regardless, so filing it would cost memory and buy nothing.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Bound;

use crate::model::graph::Props;
use crate::model::value::{Id, Value};
use crate::store::secondary_index::IndexKey;

/// `FROGQL_DISABLE_OVERLAY_INDEX=1` declines the delta, restoring the old
/// behaviour: any staged node mutation sends every indexed lookup to a
/// scan. Correct and slow, and the baseline `tests/overlay_index_test.rs`
/// A/Bs the delta against.
pub fn overlay_index_disabled() -> bool {
    std::env::var("FROGQL_DISABLE_OVERLAY_INDEX").is_ok_and(|v| v == "1")
}

/// Where one node id sits in the delta, so a re-index can retract it.
type Placement = ((String, String), IndexKey);

#[derive(Debug, Default)]
pub struct OverlayNodeIndex {
    /// `(label, prop) -> key -> ids`. A `BTreeMap` rather than a `HashMap`
    /// because the same structure has to serve equality, ranges and
    /// `ORDER BY`; the delta is small enough that the log factor on a point
    /// lookup does not matter next to the scan it replaces.
    btrees: HashMap<(String, String), BTreeMap<IndexKey, Vec<Id>>>,
    /// Every placement of a node, so re-indexing it is a retract plus an
    /// insert rather than a walk of the whole delta.
    placements: HashMap<Id, Vec<Placement>>,
    /// Base node ids the overlay has touched. Their base-index entries no
    /// longer describe them and must be dropped from a base answer.
    shadowed: HashSet<Id>,
    /// How much of `MutationOverlay::touched_node_log` has been absorbed.
    pub absorbed: usize,
    /// The overlay's node-state sizes as of the last sync, so a mutation
    /// that changed them *without* logging a touch is detectable. See
    /// `LazyGraphStore::sync_overlay_index`.
    pub witness: (usize, usize, usize, usize),
}

impl OverlayNodeIndex {
    pub fn clear(&mut self) {
        self.btrees.clear();
        self.placements.clear();
        self.shadowed.clear();
        self.absorbed = 0;
        self.witness = (0, 0, 0, 0);
    }

    pub fn is_shadowed(&self, id: Id) -> bool {
        self.shadowed.contains(&id)
    }

    /// Re-file `id` under its current labels and properties.
    ///
    /// `pairs` is the `(label, prop)` set the base index covers. `labels`
    /// and `props` are the *merged* view — what a read of that node returns
    /// right now — so a node whose property was removed simply files under
    /// fewer keys.
    pub fn reindex(
        &mut self,
        pairs: &HashSet<(String, String)>,
        base_node_count: u32,
        id: Id,
        labels: &[String],
        props: &Props,
    ) {
        self.retract(id);
        if id < base_node_count {
            self.shadowed.insert(id);
        }
        let mut placed: Vec<Placement> = Vec::new();
        for label in labels {
            for (prop, value) in props {
                let lp = (label.clone(), prop.clone());
                if !pairs.contains(&lp) {
                    continue;
                }
                let Some(key) = IndexKey::from_value(value) else {
                    continue;
                };
                self.btrees
                    .entry(lp.clone())
                    .or_default()
                    .entry(key.clone())
                    .or_default()
                    .push(id);
                placed.push((lp, key));
            }
        }
        if !placed.is_empty() {
            self.placements.insert(id, placed);
        }
    }

    /// File `id` as deleted: retract its entries, and shadow it if it is a
    /// base id so the base answer stops naming it.
    pub fn forget(&mut self, base_node_count: u32, id: Id) {
        self.retract(id);
        if id < base_node_count {
            self.shadowed.insert(id);
        }
    }

    fn retract(&mut self, id: Id) {
        let Some(places) = self.placements.remove(&id) else {
            return;
        };
        for (lp, key) in places {
            let Some(bucket) = self.btrees.get_mut(&lp) else {
                continue;
            };
            if let Some(ids) = bucket.get_mut(&key) {
                ids.retain(|x| *x != id);
                if ids.is_empty() {
                    bucket.remove(&key);
                }
            }
        }
    }

    pub fn lookup_eq(&self, label: &str, prop: &str, value: &Value) -> Vec<Id> {
        let Some(key) = IndexKey::from_value(value) else {
            return Vec::new();
        };
        self.btrees
            .get(&(label.to_string(), prop.to_string()))
            .and_then(|b| b.get(&key))
            .cloned()
            .unwrap_or_default()
    }

    pub fn lookup_range(
        &self,
        label: &str,
        prop: &str,
        lo: Bound<IndexKey>,
        hi: Bound<IndexKey>,
    ) -> Vec<Id> {
        let Some(bucket) = self.btrees.get(&(label.to_string(), prop.to_string())) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (_, ids) in bucket.range((lo, hi)) {
            out.extend_from_slice(ids);
        }
        out
    }

    /// The delta's `(key, ids)` pairs for `(label, prop)` in key order, for
    /// the `ORDER BY` merge.
    pub fn ordered_entries(&self, label: &str, prop: &str) -> Vec<(IndexKey, Vec<Id>)> {
        self.btrees
            .get(&(label.to_string(), prop.to_string()))
            .map(|b| b.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }
}
