//! Incremental nearest-neighbour cursors.
//!
//! Every evaluation strategy consumes neighbours the same way: "give me
//! the next nearest one" with no `k` fixed up front. That is the whole
//! reason for a cursor rather than a `top_k(q, k)` call — the in-LTJ and
//! pre-filter strategies do not know in advance how deep they must walk
//! before enough candidates also satisfy the graph pattern.

use crate::model::value::Id;
use crate::vector::metric::cmp_dist;

/// A resumable stream of neighbours in **approximately** non-decreasing
/// distance.
///
/// `BruteForceCursor` is exact: the sequence is genuinely sorted.
/// `HnswCursor` is best-first over a proximity graph, and expanding a
/// node can discover a strictly closer one afterwards, so its sequence
/// has inversions. Consumers that cut on a threshold must therefore
/// allow slack (see `NnStream` users and `FROGQL_VEC_TAU_EPS`); a cut
/// with no slack is exact against the *cursor's* emission order, which
/// is the honest statement of what an approximate index gives you.
pub trait NnCursor {
    /// Next neighbour, or `None` when the stream is exhausted.
    fn next(&mut self) -> Option<(Id, f32)>;

    /// Instrumentation: how many candidates the cursor has examined so
    /// far. For HNSW this is the number of graph nodes expanded, which
    /// is the headline cost metric of the whole experiment. Brute force
    /// reports the number of rows it scanned.
    fn expanded(&self) -> u64 {
        0
    }
}

/// Exact cursor: all distances computed, then sorted. This is the "no
/// metric index" baseline, and also the per-visit local sort used by
/// the in-LTJ strategy when it is told to run without an index.
pub struct BruteForceCursor {
    sorted: Vec<(Id, f32)>,
    pos: usize,
    scanned: u64,
}

impl BruteForceCursor {
    /// Take an unsorted `(id, distance)` list and turn it into a cursor.
    /// Sorting happens here, eagerly — that is what "brute force" costs.
    pub fn from_unsorted(mut pairs: Vec<(Id, f32)>) -> Self {
        let scanned = pairs.len() as u64;
        pairs.sort_by(|a, b| cmp_dist(a.1, b.1).then_with(|| a.0.cmp(&b.0)));
        BruteForceCursor {
            sorted: pairs,
            pos: 0,
            scanned,
        }
    }

    /// Wrap an already-sorted list. Used where the caller ranked the
    /// entries itself and would otherwise pay for a second sort.
    pub fn from_sorted(sorted: Vec<(Id, f32)>) -> Self {
        let scanned = sorted.len() as u64;
        BruteForceCursor {
            sorted,
            pos: 0,
            scanned,
        }
    }

    pub fn len(&self) -> usize {
        self.sorted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }
}

impl NnCursor for BruteForceCursor {
    fn next(&mut self) -> Option<(Id, f32)> {
        let out = self.sorted.get(self.pos).copied();
        if out.is_some() {
            self.pos += 1;
        }
        out
    }

    fn expanded(&self) -> u64 {
        self.scanned
    }
}

/// An empty stream. Used when the requested vector attribute has no
/// sidecar loaded, so callers can degrade without a special case.
pub struct EmptyCursor;

impl NnCursor for EmptyCursor {
    fn next(&mut self) -> Option<(Id, f32)> {
        None
    }
}

/// A monotonically growing prefix over a live cursor.
///
/// The in-LTJ strategy reaches its designated VEO level once per binding
/// of the earlier variables, and each visit re-walks the neighbour
/// stream from the nearest. Rebuilding a cursor per visit would dominate
/// every other cost in the experiment. The prefix is never truncated, so
/// `at(rank)` is a pure function of `rank`: every visit sees the same
/// mapping, and only a visit that runs past the current high-water mark
/// pays cursor work.
///
/// `replays` / `extends` are the counters that prove the cache is doing
/// its job; as the top-k threshold tightens, the high-water mark
/// converges and later visits become pure replays.
pub struct NnStream<'v> {
    cursor: Box<dyn NnCursor + 'v>,
    prefix: Vec<(Id, f32)>,
    exhausted: bool,
    /// Debug counter: emissions that were closer than their predecessor.
    /// Zero for brute force, non-zero for HNSW; quantifies how far the
    /// "approximately non-decreasing" contract bends on a given dataset.
    inversions: u64,
    pub replays: u64,
    pub extends: u64,
}

impl<'v> NnStream<'v> {
    pub fn new(cursor: Box<dyn NnCursor + 'v>) -> Self {
        NnStream {
            cursor,
            prefix: Vec::new(),
            exhausted: false,
            inversions: 0,
            replays: 0,
            extends: 0,
        }
    }

    /// The neighbour at position `rank`, extending the underlying cursor
    /// only as far as needed. `None` means the stream ended before
    /// reaching `rank`.
    pub fn at(&mut self, rank: usize) -> Option<(Id, f32)> {
        while self.prefix.len() <= rank && !self.exhausted {
            match self.cursor.next() {
                Some(entry) => {
                    if let Some(prev) = self.prefix.last() {
                        if entry.1 < prev.1 {
                            self.inversions += 1;
                        }
                    }
                    self.prefix.push(entry);
                    self.extends += 1;
                }
                None => self.exhausted = true,
            }
        }
        match self.prefix.get(rank) {
            Some(entry) => {
                self.replays += 1;
                Some(*entry)
            }
            None => None,
        }
    }

    /// How far the underlying cursor has been driven.
    pub fn high_water(&self) -> usize {
        self.prefix.len()
    }

    pub fn inversions(&self) -> u64 {
        self.inversions
    }

    pub fn expanded(&self) -> u64 {
        self.cursor.expanded()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor_of(pairs: &[(Id, f32)]) -> BruteForceCursor {
        BruteForceCursor::from_unsorted(pairs.to_vec())
    }

    #[test]
    fn brute_force_emits_in_distance_order() {
        let mut c = cursor_of(&[(7, 3.0), (2, 1.0), (5, 2.0)]);
        assert_eq!(c.next(), Some((2, 1.0)));
        assert_eq!(c.next(), Some((5, 2.0)));
        assert_eq!(c.next(), Some((7, 3.0)));
        assert_eq!(c.next(), None);
        assert_eq!(c.next(), None, "exhausted cursor stays exhausted");
    }

    #[test]
    fn brute_force_breaks_ties_by_id() {
        let mut c = cursor_of(&[(9, 1.0), (3, 1.0), (6, 1.0)]);
        assert_eq!(c.next(), Some((3, 1.0)));
        assert_eq!(c.next(), Some((6, 1.0)));
        assert_eq!(c.next(), Some((9, 1.0)));
    }

    #[test]
    fn brute_force_reports_scan_width() {
        let c = cursor_of(&[(1, 1.0), (2, 2.0)]);
        assert_eq!(c.expanded(), 2);
    }

    #[test]
    fn empty_cursor_yields_nothing() {
        assert_eq!(EmptyCursor.next(), None);
    }

    #[test]
    fn stream_replays_the_prefix_without_re_driving_the_cursor() {
        let mut s = NnStream::new(Box::new(cursor_of(&[(1, 1.0), (2, 2.0), (3, 3.0)])));

        assert_eq!(s.at(0), Some((1, 1.0)));
        assert_eq!(s.at(1), Some((2, 2.0)));
        assert_eq!(s.extends, 2);

        // Second visit re-reads the same ranks: no further cursor work.
        assert_eq!(s.at(0), Some((1, 1.0)));
        assert_eq!(s.at(1), Some((2, 2.0)));
        assert_eq!(s.extends, 2, "prefix replay must not extend the cursor");
        assert_eq!(s.high_water(), 2);
    }

    #[test]
    fn stream_extends_only_past_the_high_water_mark() {
        let mut s = NnStream::new(Box::new(cursor_of(&[(1, 1.0), (2, 2.0), (3, 3.0)])));
        assert_eq!(s.at(2), Some((3, 3.0)));
        assert_eq!(s.extends, 3, "reaching rank 2 pulls three entries");
        assert_eq!(s.at(0), Some((1, 1.0)));
        assert_eq!(s.extends, 3);
    }

    #[test]
    fn stream_reports_end_and_stays_ended() {
        let mut s = NnStream::new(Box::new(cursor_of(&[(1, 1.0)])));
        assert_eq!(s.at(0), Some((1, 1.0)));
        assert_eq!(s.at(1), None);
        assert_eq!(s.at(1), None);
        assert_eq!(s.extends, 1, "exhaustion is remembered, not re-probed");
    }

    #[test]
    fn stream_counts_inversions() {
        // A deliberately unsorted cursor stands in for approximate HNSW
        // emission order.
        struct Fixed(Vec<(Id, f32)>, usize);
        impl NnCursor for Fixed {
            fn next(&mut self) -> Option<(Id, f32)> {
                let out = self.0.get(self.1).copied();
                if out.is_some() {
                    self.1 += 1;
                }
                out
            }
        }
        let mut s = NnStream::new(Box::new(Fixed(vec![(1, 1.0), (2, 5.0), (3, 2.0)], 0)));
        assert_eq!(s.at(2), Some((3, 2.0)));
        assert_eq!(s.inversions(), 1);
    }
}
