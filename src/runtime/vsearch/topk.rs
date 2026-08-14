//! The shared top-k sink.
//!
//! Every arm funnels its accepted results through this, for
//! two reasons. It is what makes the benchmark a comparison of
//! strategies rather than of three different ways to rank; and its
//! `threshold` is the pruning signal the in-LTJ strategy cuts on.

use std::collections::HashMap;

use crate::model::value::Id;
use crate::runtime::result::ResultRow;
use crate::syntax::query::KMode;
use crate::vector::metric::cmp_dist;

/// One accepted entry, ordered by distance. A max-heap of these keeps
/// the worst at the top, which is what an eviction needs.
#[derive(Debug)]
struct Entry<T> {
    dist: f32,
    /// Tie-break, so eviction order is deterministic rather than
    /// dependent on heap internals.
    seq: u64,
    payload: T,
}

impl<T> PartialEq for Entry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.dist == other.dist && self.seq == other.seq
    }
}
impl<T> Eq for Entry<T> {}
impl<T> Ord for Entry<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        cmp_dist(self.dist, other.dist).then_with(|| self.seq.cmp(&other.seq))
    }
}
impl<T> PartialOrd for Entry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A bounded top-k collector over `(distance, result)` pairs.
///
/// `DistinctVar` keeps at most `k` distinct node ids and buffers the rows
/// produced for each. That buffering is the honest cost of the mode:
/// memory is bounded by `k × rows-per-id`, and rows for an id that later
/// loses its slot are dropped when it is evicted.
pub struct TopK {
    k: usize,
    mode: KMode,
    seq: u64,
    /// `KMode::Rows`
    rows: std::collections::BinaryHeap<Entry<ResultRow>>,
    /// `KMode::DistinctVar`
    vars: std::collections::BinaryHeap<Entry<Id>>,
    in_heap: HashMap<Id, f32>,
    buffer: HashMap<Id, Vec<ResultRow>>,
    pub evicted: u64,
    pub buffered: u64,
}

impl TopK {
    pub fn new(k: usize, mode: KMode) -> TopK {
        TopK {
            k,
            mode,
            seq: 0,
            rows: std::collections::BinaryHeap::new(),
            vars: std::collections::BinaryHeap::new(),
            in_heap: HashMap::new(),
            buffer: HashMap::new(),
            evicted: 0,
            buffered: 0,
        }
    }

    pub fn mode(&self) -> KMode {
        self.mode
    }

    pub fn k(&self) -> usize {
        self.k
    }

    /// The worst distance currently held, or `+∞` while there is still
    /// room. Monotonically non-increasing, which is what makes it a
    /// sound cut: once `dist > threshold()`, `dist` can never enter the
    /// answer, because `k` entries already beat it and no later
    /// insertion can loosen the bound.
    pub fn threshold(&self) -> f32 {
        if self.k == 0 {
            return f32::NEG_INFINITY;
        }
        match self.mode {
            KMode::Rows => {
                if self.rows.len() < self.k {
                    f32::INFINITY
                } else {
                    self.rows.peek().map(|e| e.dist).unwrap_or(f32::INFINITY)
                }
            }
            KMode::DistinctVar => {
                if self.vars.len() < self.k {
                    f32::INFINITY
                } else {
                    self.vars.peek().map(|e| e.dist).unwrap_or(f32::INFINITY)
                }
            }
        }
    }

    /// True once no further offer at distance `>= threshold()` can be
    /// accepted. Callers use it only as a hint; `threshold` is the rule.
    pub fn is_full(&self) -> bool {
        match self.mode {
            KMode::Rows => self.rows.len() >= self.k,
            KMode::DistinctVar => self.vars.len() >= self.k,
        }
    }

    /// Offer one result row at `dist`. `KMode::Rows` only.
    pub fn offer_row(&mut self, dist: f32, row: ResultRow) {
        debug_assert_eq!(self.mode, KMode::Rows);
        if self.k == 0 {
            return;
        }
        self.seq += 1;
        let entry = Entry {
            dist,
            seq: self.seq,
            payload: row,
        };
        if self.rows.len() < self.k {
            self.rows.push(entry);
            return;
        }
        // Strictly better than the current worst, or it is not an
        // improvement and ties keep the incumbent.
        let worse_than_incumbent = match self.rows.peek() {
            Some(w) => cmp_dist(entry.dist, w.dist) != std::cmp::Ordering::Less,
            None => false,
        };
        if worse_than_incumbent {
            return;
        }
        self.rows.pop();
        self.evicted += 1;
        self.rows.push(entry);
    }

    /// Offer one **distinct** binding of the search variable, with every
    /// row it produced. `KMode::DistinctVar` only.
    ///
    /// Re-offering an id already held is a no-op: a node has exactly one
    /// distance, so letting it in twice would burn two of the `k` slots
    /// on the same neighbour.
    pub fn offer_var(&mut self, id: Id, dist: f32, rows: Vec<ResultRow>) {
        debug_assert_eq!(self.mode, KMode::DistinctVar);
        if self.k == 0 || rows.is_empty() || self.in_heap.contains_key(&id) {
            return;
        }
        self.seq += 1;
        let entry = Entry {
            dist,
            seq: self.seq,
            payload: id,
        };
        if self.vars.len() < self.k {
            self.buffered += rows.len() as u64;
            self.buffer.insert(id, rows);
            self.in_heap.insert(id, dist);
            self.vars.push(entry);
            return;
        }
        let worse_than_incumbent = match self.vars.peek() {
            Some(w) => cmp_dist(entry.dist, w.dist) != std::cmp::Ordering::Less,
            None => false,
        };
        if worse_than_incumbent {
            return;
        }
        // Evict the current worst, dropping its buffered rows with it —
        // keeping them would leak every id that ever held a slot.
        if let Some(worst) = self.vars.pop() {
            self.in_heap.remove(&worst.payload);
            self.buffer.remove(&worst.payload);
            self.evicted += 1;
        }
        self.buffered += rows.len() as u64;
        self.buffer.insert(id, rows);
        self.in_heap.insert(id, dist);
        self.vars.push(entry);
    }

    /// Has this id already claimed a slot? Lets a caller skip work it
    /// would only discard.
    pub fn holds_var(&self, id: Id) -> bool {
        self.in_heap.contains_key(&id)
    }

    /// Every accepted row, nearest first. Rows of one `DistinctVar`
    /// binding keep their production order relative to each other.
    pub fn drain_sorted(self) -> Vec<(f32, ResultRow)> {
        match self.mode {
            KMode::Rows => {
                let mut v: Vec<Entry<ResultRow>> = self.rows.into_vec();
                v.sort();
                v.into_iter().map(|e| (e.dist, e.payload)).collect()
            }
            KMode::DistinctVar => {
                let mut v: Vec<Entry<Id>> = self.vars.into_vec();
                v.sort();
                let mut buffer = self.buffer;
                let mut out = Vec::new();
                for e in v {
                    if let Some(rows) = buffer.remove(&e.payload) {
                        for row in rows {
                            out.push((e.dist, row));
                        }
                    }
                }
                out
            }
        }
    }
}

/// The pruning half of `TopK`, carrying distances only.
///
/// The in-LTJ strategy needs the threshold *while* the join is still
/// running, but at that point a match is a `ResultTuple` of raw variable
/// bindings — the rows do not exist until the search returns and the
/// tuples are converted. So the search tracks distances alone, and the
/// assembled `TopK` does the final selection afterwards under the same
/// rule.
///
/// The two agree by construction: both keep the `k` smallest keys and
/// both refuse a tie once full, so the threshold reported here is the one
/// `TopK` will settle on.
pub struct DistThreshold {
    k: usize,
    mode: KMode,
    /// Max-heap of the `k` smallest distances seen.
    heap: std::collections::BinaryHeap<Entry<()>>,
    /// `DistinctVar`: bindings already counted, so one cannot claim two
    /// slots.
    seen: HashMap<Id, f32>,
    seq: u64,
}

impl DistThreshold {
    pub fn new(k: usize, mode: KMode) -> DistThreshold {
        DistThreshold {
            k,
            mode,
            heap: std::collections::BinaryHeap::new(),
            seen: HashMap::new(),
            seq: 0,
        }
    }

    /// Same contract as `TopK::threshold`: `+∞` until `k` are held, then
    /// the worst held, never rising afterwards.
    pub fn get(&self) -> f32 {
        if self.k == 0 {
            return f32::NEG_INFINITY;
        }
        if self.heap.len() < self.k {
            f32::INFINITY
        } else {
            self.heap.peek().map(|e| e.dist).unwrap_or(f32::INFINITY)
        }
    }

    /// Record an accepted result. `id` is the search variable's binding,
    /// consulted only in `DistinctVar` mode.
    pub fn accept(&mut self, id: Id, dist: f32) {
        if self.k == 0 {
            return;
        }
        if self.mode == KMode::DistinctVar {
            if self.seen.contains_key(&id) {
                return;
            }
            self.seen.insert(id, dist);
        }
        self.seq += 1;
        let entry = Entry {
            dist,
            seq: self.seq,
            payload: (),
        };
        if self.heap.len() < self.k {
            self.heap.push(entry);
            return;
        }
        let worse_than_incumbent = match self.heap.peek() {
            Some(w) => cmp_dist(entry.dist, w.dist) != std::cmp::Ordering::Less,
            None => false,
        };
        if worse_than_incumbent {
            return;
        }
        self.heap.pop();
        self.heap.push(entry);
    }

    /// Has this binding already been counted? `DistinctVar` only.
    pub fn holds(&self, id: Id) -> bool {
        self.seen.contains_key(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::value::PathValue;
    use crate::runtime::assignment::Assignment;

    fn row(tag: u32) -> ResultRow {
        let mut a = Assignment::new();
        a.extend("x".to_string(), PathValue::Node(tag));
        ResultRow::with_paths(Vec::new(), a)
    }

    fn tags(out: &[(f32, ResultRow)]) -> Vec<u32> {
        out.iter()
            .map(|(_, r)| match r.assignment.get("x") {
                Some(PathValue::Node(n)) => *n,
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn rows_mode_keeps_the_k_nearest_in_order() {
        let mut t = TopK::new(2, KMode::Rows);
        t.offer_row(5.0, row(5));
        t.offer_row(1.0, row(1));
        t.offer_row(3.0, row(3));
        let out = t.drain_sorted();
        assert_eq!(tags(&out), vec![1, 3]);
        assert_eq!(out[0].0, 1.0);
    }

    #[test]
    fn rows_mode_threshold_is_infinite_until_full_then_the_worst() {
        let mut t = TopK::new(2, KMode::Rows);
        assert_eq!(t.threshold(), f32::INFINITY);
        t.offer_row(1.0, row(1));
        assert_eq!(t.threshold(), f32::INFINITY);
        t.offer_row(4.0, row(4));
        assert_eq!(t.threshold(), 4.0);
        t.offer_row(2.0, row(2));
        assert_eq!(t.threshold(), 2.0, "threshold only ever tightens");
    }

    #[test]
    fn rows_mode_rejects_a_tie_once_full_so_the_incumbent_wins() {
        let mut t = TopK::new(1, KMode::Rows);
        t.offer_row(2.0, row(1));
        t.offer_row(2.0, row(2));
        assert_eq!(tags(&t.drain_sorted()), vec![1]);
    }

    #[test]
    fn distinct_var_mode_keeps_every_row_of_an_accepted_binding() {
        let mut t = TopK::new(2, KMode::DistinctVar);
        t.offer_var(10, 1.0, vec![row(1), row(2), row(3)]);
        t.offer_var(20, 2.0, vec![row(4)]);
        let out = t.drain_sorted();
        assert_eq!(tags(&out), vec![1, 2, 3, 4]);
        assert_eq!(out.len(), 4, "k counts bindings, not rows");
    }

    #[test]
    fn distinct_var_mode_ignores_a_repeat_offer() {
        let mut t = TopK::new(2, KMode::DistinctVar);
        t.offer_var(10, 1.0, vec![row(1)]);
        t.offer_var(10, 1.0, vec![row(99)]);
        assert!(t.holds_var(10));
        assert_eq!(tags(&t.drain_sorted()), vec![1]);
    }

    #[test]
    fn distinct_var_eviction_drops_the_loser_rows() {
        let mut t = TopK::new(1, KMode::DistinctVar);
        t.offer_var(10, 5.0, vec![row(1), row(2)]);
        t.offer_var(20, 1.0, vec![row(3)]);
        assert_eq!(tags(&t.drain_sorted()), vec![3]);
    }

    #[test]
    fn an_empty_binding_never_takes_a_slot() {
        let mut t = TopK::new(1, KMode::DistinctVar);
        t.offer_var(10, 0.0, vec![]);
        assert!(!t.holds_var(10));
        assert_eq!(t.threshold(), f32::INFINITY);
        assert!(t.drain_sorted().is_empty());
    }

    #[test]
    fn k_zero_accepts_nothing_in_either_mode() {
        let mut t = TopK::new(0, KMode::Rows);
        t.offer_row(1.0, row(1));
        assert!(t.drain_sorted().is_empty());

        let mut t = TopK::new(0, KMode::DistinctVar);
        t.offer_var(1, 1.0, vec![row(1)]);
        assert!(t.drain_sorted().is_empty());
    }

    #[test]
    fn threshold_never_grows_across_a_long_offer_sequence() {
        let mut t = TopK::new(3, KMode::Rows);
        let mut prev = f32::INFINITY;
        for d in [9.0, 2.0, 7.0, 1.0, 8.0, 0.5, 6.0] {
            t.offer_row(d, row(0));
            let now = t.threshold();
            assert!(now <= prev, "threshold rose from {prev} to {now}");
            prev = now;
        }
    }
}
