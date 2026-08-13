use std::collections::HashMap;

use super::compact::CompactTripleIndex;
use crate::model::graph_access::GraphAccess;

/// Which of the 6 SPO orderings to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrieOrder {
    SPO = 0,
    SOP = 1,
    POS = 2,
    PSO = 3,
    OSP = 4,
    OPS = 5,
}

/// A single entry in the index: (component0, component1, component2, edge_id).
/// The meaning of each component depends on the TrieOrder.
pub type IndexEntry = (u32, u32, u32, u32);

/// Physical representation of the six orderings. `Array` is the original
/// simplified implementation (six fully-materialized sorted tuple arrays);
/// `Compact` is the CLTJ port (six LOUDS succinct tries + an eid side table,
/// issue #66). Selected at build time via `FROGQL_LTJ_COMPACT=1`.
enum IndexRepr {
    Array([Vec<IndexEntry>; 6]),
    Compact(Box<CompactTripleIndex>),
}

/// Index of all directed edges as (src, label_id, tgt) triples, stored in 6 sorted orderings.
/// Each entry also carries the original edge_id for result reconstruction.
pub struct TripleIndex {
    repr: IndexRepr,
    pub label_to_id: HashMap<String, u32>,
    pub id_to_label: Vec<String>,
}

impl TripleIndex {
    /// Build the standard index from a graph: directed edges in their
    /// physical sense only, undirected edges in both senses. This is the
    /// index for directed / `~`-undirected / leftward pattern edges.
    pub fn from_graph<G: GraphAccess>(graph: &G) -> Self {
        Self::from_graph_impl(graph, false)
    }

    /// Build the **any-direction** index: *every* physical edge — directed
    /// and undirected alike — is stored in both senses, sharing one eid.
    /// A pattern edge `-[e]-` decomposes to a single forward triple
    /// `(x, L, y)` run against this index, which then matches an edge
    /// between `x` and `y` regardless of physical orientation, in one LTJ
    /// pass — no per-edge `2^k` branch enumeration, no reverse-view
    /// intricacy in the iterator. Combined with the base-case per-eid
    /// fan-out (issue #71), this reproduces ISO bag multiplicity: a
    /// directed edge `a→b` yields two matches under `-[e]-` (`x=a,y=b` and
    /// `x=b,y=a`), a reciprocal pair yields two per endpoint binding, etc.
    /// Roughly doubles the directed-triple count vs `from_graph`; built
    /// lazily, only when a query actually contains an any-direction edge.
    pub fn from_graph_anydir<G: GraphAccess>(graph: &G) -> Self {
        Self::from_graph_impl(graph, true)
    }

    fn from_graph_impl<G: GraphAccess>(graph: &G, mirror_directed: bool) -> Self {
        let mut label_to_id: HashMap<String, u32> = HashMap::new();
        let mut id_to_label: Vec<String> = Vec::new();

        // Collect all triples: (src, label_id, tgt, edge_id)
        let mut raw_triples: Vec<(u32, u32, u32, u32)> = Vec::new();

        // In the any-direction index, mirror directed edges too, so a
        // forward triple lookup finds them from either endpoint. Self-loops
        // are pushed twice (matching the fallback's `right` + `left`
        // solutions); parallel edges keep their multiplicity (build_ordering
        // does not dedup, and the base case fans out per eid).
        let push_directed = |raw: &mut Vec<(u32, u32, u32, u32)>, s, lid, o, e| {
            raw.push((s, lid, o, e));
            if mirror_directed {
                raw.push((o, lid, s, e));
            }
        };

        for eid in graph.edges_directed() {
            let src = graph.src(eid);
            let tgt = graph.tgt(eid);
            let labels = graph.edge_labels(eid);
            let label_strings = labels.required_labels();

            if label_strings.is_empty() {
                // Edge with no label — use a special "no label" ID
                let lid = Self::get_or_insert_label(&mut label_to_id, &mut id_to_label, "");
                push_directed(&mut raw_triples, src, lid, tgt, eid);
            } else {
                // One triple per label
                for ls in label_strings {
                    let lid = Self::get_or_insert_label(&mut label_to_id, &mut id_to_label, ls);
                    push_directed(&mut raw_triples, src, lid, tgt, eid);
                }
            }
        }

        // Undirected edges are emitted in both senses so that a forward
        // triple lookup (the only kind LTJ understands today) finds them
        // regardless of which endpoint the query binds first. The eid is
        // shared across both senses; the runner uses `graph.edge_path_value`
        // to recover the `PathValue::EdgeUndirectional` variant.
        for eid in graph.edges_undirected() {
            let src = graph.src(eid);
            let tgt = graph.tgt(eid);
            let labels = graph.edge_labels(eid);
            let label_strings = labels.required_labels();

            let push_both = |raw: &mut Vec<(u32, u32, u32, u32)>, lid: u32| {
                raw.push((src, lid, tgt, eid));
                if src != tgt {
                    raw.push((tgt, lid, src, eid));
                }
            };

            if label_strings.is_empty() {
                let lid = Self::get_or_insert_label(&mut label_to_id, &mut id_to_label, "");
                push_both(&mut raw_triples, lid);
            } else {
                for ls in label_strings {
                    let lid = Self::get_or_insert_label(&mut label_to_id, &mut id_to_label, ls);
                    push_both(&mut raw_triples, lid);
                }
            }
        }

        let repr = if Self::compact_selected() {
            IndexRepr::Compact(Box::new(CompactTripleIndex::from_raw(&raw_triples)))
        } else {
            // Build 6 orderings
            let spo = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (s, p, o, e));
            let sop = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (s, o, p, e));
            let pos = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (p, o, s, e));
            let pso = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (p, s, o, e));
            let osp = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (o, s, p, e));
            let ops = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (o, p, s, e));
            IndexRepr::Array([spo, sop, pos, pso, osp, ops])
        };

        TripleIndex {
            repr,
            label_to_id,
            id_to_label,
        }
    }

    /// `FROGQL_LTJ_COMPACT=1` selects the compact CLTJ representation at
    /// build time; default stays on the array implementation.
    fn compact_selected() -> bool {
        std::env::var("FROGQL_LTJ_COMPACT").is_ok_and(|v| v == "1")
    }

    /// The array orderings, when this index was built in array mode.
    pub(super) fn array(&self) -> Option<&[Vec<IndexEntry>; 6]> {
        match &self.repr {
            IndexRepr::Array(o) => Some(o),
            IndexRepr::Compact(_) => None,
        }
    }

    /// The compact index, when this index was built in compact mode.
    pub(super) fn compact(&self) -> Option<&CompactTripleIndex> {
        match &self.repr {
            IndexRepr::Array(_) => None,
            IndexRepr::Compact(c) => Some(c),
        }
    }

    /// Approximate heap footprint of the index payload (excludes label maps).
    pub fn heap_bytes(&self) -> usize {
        match &self.repr {
            IndexRepr::Array(orderings) => orderings
                .iter()
                .map(|o| o.len() * std::mem::size_of::<IndexEntry>())
                .sum(),
            IndexRepr::Compact(c) => c.heap_bytes(),
        }
    }

    fn get_or_insert_label(
        map: &mut HashMap<String, u32>,
        vec: &mut Vec<String>,
        label: &str,
    ) -> u32 {
        if let Some(&id) = map.get(label) {
            id
        } else {
            let id = vec.len() as u32;
            vec.push(label.to_string());
            map.insert(label.to_string(), id);
            id
        }
    }

    fn build_ordering<F>(raw: &[(u32, u32, u32, u32)], reorder: F) -> Vec<IndexEntry>
    where
        F: Fn(&(u32, u32, u32, u32)) -> IndexEntry,
    {
        let mut v: Vec<IndexEntry> = raw.iter().map(&reorder).collect();
        v.sort_unstable();
        v
    }

    /// Get the sorted array for a given ordering. Panics on a compact-mode
    /// index — the array navigation path never runs against one (the
    /// iterator dispatches on the representation first).
    pub fn get_ordering(&self, order: TrieOrder) -> &[IndexEntry] {
        &self
            .array()
            .expect("get_ordering called on a compact TripleIndex")[order as usize]
    }

    /// Binary search within a range [begin, end) at a given depth (0, 1, or 2).
    /// Returns the sub-range of entries whose component at `depth` equals `key`.
    pub fn range_for_key(
        slice: &[IndexEntry],
        begin: usize,
        end: usize,
        depth: usize,
        key: u32,
    ) -> (usize, usize) {
        let sub = &slice[begin..end];

        let lo = sub.partition_point(|entry| Self::component(entry, depth) < key);
        let hi = lo + sub[lo..].partition_point(|entry| Self::component(entry, depth) <= key);

        (begin + lo, begin + hi)
    }

    /// Find the first value >= `key` at `depth` within [begin, end).
    /// Returns the value and the start of its range, or None if no such value exists.
    pub fn leap(
        slice: &[IndexEntry],
        begin: usize,
        end: usize,
        depth: usize,
        key: u32,
    ) -> Option<(u32, usize)> {
        let sub = &slice[begin..end];
        let pos = sub.partition_point(|entry| Self::component(entry, depth) < key);
        if pos >= sub.len() {
            None
        } else {
            let val = Self::component(&sub[pos], depth);
            Some((val, begin + pos))
        }
    }

    /// Get the number of distinct values at `depth` within [begin, end).
    pub fn distinct_count(slice: &[IndexEntry], begin: usize, end: usize, depth: usize) -> usize {
        if begin >= end {
            return 0;
        }
        let sub = &slice[begin..end];
        let mut count = 1;
        let mut prev = Self::component(&sub[0], depth);
        for entry in &sub[1..] {
            let v = Self::component(entry, depth);
            if v != prev {
                count += 1;
                prev = v;
            }
        }
        count
    }

    /// Collect all distinct values at `depth` within [begin, end).
    pub fn all_values(slice: &[IndexEntry], begin: usize, end: usize, depth: usize) -> Vec<u32> {
        if begin >= end {
            return vec![];
        }
        let sub = &slice[begin..end];
        let mut result = vec![Self::component(&sub[0], depth)];
        for entry in &sub[1..] {
            let v = Self::component(entry, depth);
            if v != *result.last().unwrap() {
                result.push(v);
            }
        }
        result
    }

    /// Extract a component from an entry by depth (0, 1, or 2).
    #[inline]
    pub fn component(entry: &IndexEntry, depth: usize) -> u32 {
        match depth {
            0 => entry.0,
            1 => entry.1,
            2 => entry.2,
            _ => entry.3, // edge_id, depth 3
        }
    }

    /// Get edge_id from an entry.
    #[inline]
    pub fn edge_id(entry: &IndexEntry) -> u32 {
        entry.3
    }

    /// Total number of triples in the index (duplicates included).
    pub fn len(&self) -> usize {
        match &self.repr {
            IndexRepr::Array(orderings) => orderings[0].len(),
            IndexRepr::Compact(c) => c.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::graph::MemoryGraphStore;
    use std::path::Path;

    #[test]
    fn test_build_from_fraud_graph() {
        let graph = MemoryGraphStore::from_file(Path::new("test_data/fraud.json")).unwrap();
        let idx = TripleIndex::from_graph(&graph);
        assert!(!idx.is_empty());

        // All 6 orderings have the same length
        let n = idx.len();
        let orderings = idx.array().expect("default build is array mode");
        for ordering in orderings {
            assert_eq!(ordering.len(), n);
        }

        // SPO ordering is sorted
        let spo = idx.get_ordering(TrieOrder::SPO);
        for w in spo.windows(2) {
            assert!(w[0] <= w[1], "SPO not sorted: {:?} > {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn test_range_and_leap() {
        // Manual triple set: (0,0,1), (0,0,2), (0,1,3), (1,0,2)
        let entries: Vec<IndexEntry> = vec![
            (0, 0, 1, 100),
            (0, 0, 2, 101),
            (0, 1, 3, 102),
            (1, 0, 2, 103),
        ];

        // Range for depth=0, key=0 → first 3 entries
        let (lo, hi) = TripleIndex::range_for_key(&entries, 0, 4, 0, 0);
        assert_eq!((lo, hi), (0, 3));

        // Range for depth=0, key=1 → last entry
        let (lo, hi) = TripleIndex::range_for_key(&entries, 0, 4, 0, 1);
        assert_eq!((lo, hi), (3, 4));

        // Leap at depth=0, key=0 → finds 0 at pos 0
        let r = TripleIndex::leap(&entries, 0, 4, 0, 0);
        assert_eq!(r, Some((0, 0)));

        // Leap at depth=0, key=1 → finds 1 at pos 3
        let r = TripleIndex::leap(&entries, 0, 4, 0, 1);
        assert_eq!(r, Some((1, 3)));

        // Leap at depth=0, key=2 → None
        let r = TripleIndex::leap(&entries, 0, 4, 0, 2);
        assert_eq!(r, None);

        // Within range [0,3) at depth=1: values are 0,0,1
        let r = TripleIndex::leap(&entries, 0, 3, 1, 0);
        assert_eq!(r, Some((0, 0)));
        let r = TripleIndex::leap(&entries, 0, 3, 1, 1);
        assert_eq!(r, Some((1, 2)));

        // distinct_count at depth=1 within [0,3) → 2 (values 0 and 1)
        assert_eq!(TripleIndex::distinct_count(&entries, 0, 3, 1), 2);

        // all_values at depth=2 within [0,2) → [1, 2]
        assert_eq!(TripleIndex::all_values(&entries, 0, 2, 2), vec![1, 2]);
    }
}
