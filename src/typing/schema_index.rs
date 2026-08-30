//! Label index over schema entries — the typechecker's analogue of the
//! runtime's TripleIndex label table.
//!
//! `refine`'s scan arms test every schema entry with `is_subtype` + `meet`.
//! Warm, the refine memo absorbs that; **cold** (first sighting of a shape
//! in a session) the full scan is why a label-visible rejection cost more
//! than the runtime's index lookup. This index makes the *miss path* cheap:
//! entries are bucketed under every positive leaf label of their
//! `LabelType` tree, and a query whose label tree is star/neg-free scans
//! only `buckets(query leaves) ∪ fallback` instead of everything. A
//! nonexistent label yields just the fallback bucket — usually empty.
//!
//! Soundness (over-approximation — the existing `is_subtype` filter still
//! decides membership; the index only prunes provable non-matches):
//! entries whose label tree contains `Star`/`Top`/`Empty`/`Neg` go to the
//! always-scanned fallback. For a bucket-classified entry (pure
//! `Label`/`And`/`Or` tree) and a star/neg-free query tree, every
//! `is_subtype(entry, query)` derivation bottoms out in
//! `(Label a, Label b) ⇒ a == b` through `And`/`Or` decompositions on
//! either side — so a matching entry shares at least one leaf label with
//! the query and sits in one of the consulted buckets. Queries containing
//! `Star`/`Top`/`Neg`/`Empty` fall back to the full scan (`candidates`
//! returns `None`); in particular anonymous patterns are unaffected.
//! Pinned by `tests/tc_schema_index_proptest.rs` (indexed ≡ linear over
//! random label trees) and the `GQLITE_DISABLE_TC_SCHEMA_INDEX` switch.
//!
//! Candidate lists are kept in ascending entry order so the surviving
//! matches fold (`join_from_list`) in exactly the order the linear scan
//! produced — refined results are bit-identical, not merely equivalent.

use std::collections::HashMap;

use super::label_type::LabelType;
use super::variable_type::VariableType;

#[derive(Debug, Default)]
pub(crate) struct SchemaIndex {
    node_by_label: HashMap<String, Vec<u32>>,
    node_fallback: Vec<u32>,
    edge_by_label: HashMap<String, Vec<u32>>,
    edge_fallback: Vec<u32>,
}

/// Collect the positive leaf labels of a pure `Label`/`And`/`Or` tree.
/// Returns `false` (collection invalid) if the tree contains any
/// `Star`/`Top`/`Empty`/`Neg` — those can match without sharing a leaf.
fn leaf_labels<'a>(l: &'a LabelType, out: &mut Vec<&'a str>) -> bool {
    match l {
        LabelType::Label(s) => {
            out.push(s.as_str());
            true
        }
        LabelType::And(a, b) | LabelType::Or(a, b) => leaf_labels(a, out) && leaf_labels(b, out),
        LabelType::Star | LabelType::Top | LabelType::Empty | LabelType::Neg(_) => false,
    }
}

fn classify(entries: &[VariableType], by: &mut HashMap<String, Vec<u32>>, fallback: &mut Vec<u32>) {
    for (i, e) in entries.iter().enumerate() {
        let mut leaves = Vec::new();
        match e.descriptor() {
            Some(d) if leaf_labels(&d.label, &mut leaves) => {
                for s in leaves {
                    by.entry(s.to_string()).or_default().push(i as u32);
                }
            }
            // Star/Top/Empty/Neg-labelled or non-Node/Edge entries can
            // match without a shared leaf: always scanned.
            _ => fallback.push(i as u32),
        }
    }
}

impl SchemaIndex {
    pub(crate) fn build(nodes: &[VariableType], edges: &[VariableType]) -> Self {
        let mut idx = SchemaIndex::default();
        classify(nodes, &mut idx.node_by_label, &mut idx.node_fallback);
        classify(edges, &mut idx.edge_by_label, &mut idx.edge_fallback);
        idx
    }

    /// Candidate entry indices for a query label, ascending and deduped;
    /// `None` means the query's label tree cannot be bucket-served — do
    /// the full scan.
    pub(crate) fn candidates(&self, nodes: bool, query: &LabelType) -> Option<Vec<u32>> {
        let mut leaves = Vec::new();
        if !leaf_labels(query, &mut leaves) {
            return None;
        }
        let (by, fallback) = if nodes {
            (&self.node_by_label, &self.node_fallback)
        } else {
            (&self.edge_by_label, &self.edge_fallback)
        };
        let mut out = fallback.clone();
        for s in leaves {
            if let Some(v) = by.get(s) {
                out.extend_from_slice(v);
            }
        }
        out.sort_unstable();
        out.dedup();
        Some(out)
    }
}
