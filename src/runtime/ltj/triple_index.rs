use std::collections::HashMap;

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

/// Index of all directed edges as (src, label_id, tgt) triples, stored in 6 sorted orderings.
/// Each entry also carries the original edge_id for result reconstruction.
pub struct TripleIndex {
    orderings: [Vec<IndexEntry>; 6],
    pub label_to_id: HashMap<String, u32>,
    pub id_to_label: Vec<String>,
}

impl TripleIndex {
    /// Build the index from a graph.
    /// Iterates all directed edges, extracts labels, assigns numeric label IDs,
    /// and builds 6 sorted orderings.
    pub fn from_graph<G: GraphAccess>(graph: &G) -> Self {
        let mut label_to_id: HashMap<String, u32> = HashMap::new();
        let mut id_to_label: Vec<String> = Vec::new();

        // Collect all triples: (src, label_id, tgt, edge_id)
        let mut raw_triples: Vec<(u32, u32, u32, u32)> = Vec::new();

        for eid in graph.edges_directed() {
            let src = graph.src(eid);
            let tgt = graph.tgt(eid);
            let labels = graph.edge_labels(eid);
            let label_strings = labels.required_labels();

            if label_strings.is_empty() {
                // Edge with no label — use a special "no label" ID
                let lid = Self::get_or_insert_label(&mut label_to_id, &mut id_to_label, "");
                raw_triples.push((src, lid, tgt, eid));
            } else {
                // One triple per label
                for ls in label_strings {
                    let lid = Self::get_or_insert_label(&mut label_to_id, &mut id_to_label, ls);
                    raw_triples.push((src, lid, tgt, eid));
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

        // Build 6 orderings
        let spo = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (s, p, o, e));
        let sop = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (s, o, p, e));
        let pos = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (p, o, s, e));
        let pso = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (p, s, o, e));
        let osp = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (o, s, p, e));
        let ops = Self::build_ordering(&raw_triples, |&(s, p, o, e)| (o, p, s, e));

        TripleIndex {
            orderings: [spo, sop, pos, pso, osp, ops],
            label_to_id,
            id_to_label,
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

    /// Get the sorted array for a given ordering.
    pub fn get_ordering(&self, order: TrieOrder) -> &[IndexEntry] {
        &self.orderings[order as usize]
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

    /// Total number of triples in the index.
    pub fn len(&self) -> usize {
        self.orderings[0].len()
    }

    pub fn is_empty(&self) -> bool {
        self.orderings[0].is_empty()
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
        for i in 0..6 {
            assert_eq!(idx.orderings[i].len(), n);
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
