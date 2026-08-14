//! Compact CLTJ index: LOUDS-style succinct tries (issue #66).
//!
//! Port of the CompactLTJ reference (`cltj/include/trie/cltj_compact_trie.hpp`,
//! Arroyuelo et al., VLDBJ 2025) — the `cltj_index_spo_basic` tier: six full
//! compact tries, one per SPO ordering. Each trie is
//!   - a topology bitvector `bv` = `0 · (1^{d_0} 0) (1^{d_1} 0) …` where `d_k`
//!     is the child count of the k-th *internal* node in level order (k = 0 is
//!     the root; leaves contribute no run), and
//!   - a bit-packed symbol sequence `seq` holding every non-root node's symbol
//!     in level order.
//!
//! The navigation invariants (verified against the C++ reference):
//!   - a node *handle* `it` is a bv position with `bv[it] == 0`;
//!   - `children(it) = succ0(it + 1) − it`;
//!   - the child symbols of `it` live at `seq[it .. it + children(it)]`
//!     (`first_child(it) = it` — bv zero positions and seq indices align);
//!   - the handle of the node whose symbol sits at seq position `p` is
//!     `select0(p + 2)` (`nodeselect`).
//!
//! The leapfrog seek is a plain binary search inside a node's child block —
//! same asymptotics as the array index, over ~3× less memory (bit-packed
//! symbols + one topology bit per node instead of four u32 per triple per
//! ordering).
//!
//! Divergence from the RDF reference: property-graph triples carry edge ids
//! and bag multiplicity (parallel edges collapse into one trie leaf). The
//! index keeps a side table grouped by distinct (s, p, o) in SPO order —
//! `leaf_offsets` / `leaf_eids` — that the iterator's base case consults via
//! an SPO re-descent (`eids_for`).

/// Plain bitvector with select-0 (position of the k-th zero, 1-indexed) and
/// succ-0 (position of the first zero at or after `i`) support. Select is
/// sample-accelerated: every `SAMPLE`-th zero position is stored, the rest
/// is a popcount word scan. Bits past `len` read as ones so they never count
/// as zeros.
pub struct SelectBitVec {
    words: Vec<u64>,
    len: usize,
    /// Positions of the (k·SAMPLE + 1)-th zeros.
    samples: Vec<u32>,
    num_zeros: usize,
}

const SAMPLE: usize = 512;

impl SelectBitVec {
    /// Build from the set of zero positions over a domain of `len` bits
    /// (all other bits are ones). `zero_positions` must be strictly
    /// ascending.
    ///
    /// Positions are `u32`, not `usize`: `len` is asserted below
    /// `u32::MAX`, so a position never needs eight bytes, and this vector
    /// has one entry per trie node — at SF0.3 that is the difference
    /// between 18 MiB and 37 MiB of build scratch, per trie, six times.
    pub fn from_zero_positions(len: usize, zero_positions: &[u32]) -> Self {
        assert!(len < u32::MAX as usize, "bitvector too large");
        let n_words = (len + 63) / 64;
        let mut words = vec![!0u64; n_words];
        for &p in zero_positions {
            let p = p as usize;
            debug_assert!(p < len);
            words[p / 64] &= !(1u64 << (p % 64));
        }
        let samples = zero_positions.iter().step_by(SAMPLE).copied().collect();
        SelectBitVec {
            words,
            len,
            samples,
            num_zeros: zero_positions.len(),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Position of the k-th zero (k is 1-indexed). Panics if k exceeds the
    /// number of zeros — callers navigate within trie bounds.
    pub fn select0(&self, k: usize) -> usize {
        debug_assert!(k >= 1 && k <= self.num_zeros, "select0 out of range");
        let s = (k - 1) / SAMPLE;
        let mut pos = self.samples[s] as usize;
        let mut remaining = k - (s * SAMPLE + 1);
        if remaining == 0 {
            return pos;
        }
        // Scan zeros strictly after `pos`.
        let mut wi = (pos + 1) / 64;
        let mut w = !self.words[wi] & (!0u64 << ((pos + 1) % 64));
        loop {
            let cnt = w.count_ones() as usize;
            if remaining <= cnt {
                // remaining-th set bit of w (1-indexed)
                let mut ww = w;
                for _ in 1..remaining {
                    ww &= ww - 1;
                }
                pos = wi * 64 + ww.trailing_zeros() as usize;
                return pos;
            }
            remaining -= cnt;
            wi += 1;
            w = !self.words[wi];
        }
    }

    /// Position of the first zero at or after `i`; `len` if none.
    pub fn succ0(&self, i: usize) -> usize {
        if i >= self.len {
            return self.len;
        }
        let mut wi = i / 64;
        let mut w = !self.words[wi] & (!0u64 << (i % 64));
        while w == 0 {
            wi += 1;
            if wi >= self.words.len() {
                return self.len;
            }
            w = !self.words[wi];
        }
        let pos = wi * 64 + w.trailing_zeros() as usize;
        pos.min(self.len)
    }

    pub fn heap_bytes(&self) -> usize {
        self.words.len() * 8 + self.samples.len() * 4
    }
}

/// Fixed-width bit-packed sequence of u32 symbols.
pub struct IntSeq {
    data: Vec<u64>,
    width: u32,
    len: usize,
}

impl IntSeq {
    pub fn from_slice(values: &[u32]) -> Self {
        let max = values.iter().copied().max().unwrap_or(0);
        let width = (32 - max.leading_zeros()).max(1);
        let total_bits = values.len() * width as usize;
        let mut data = vec![0u64; (total_bits + 63) / 64];
        for (i, &v) in values.iter().enumerate() {
            let bit = i * width as usize;
            let (wi, off) = (bit / 64, (bit % 64) as u32);
            data[wi] |= (v as u64) << off;
            if off + width > 64 {
                data[wi + 1] |= (v as u64) >> (64 - off);
            }
        }
        IntSeq {
            data,
            width,
            len: values.len(),
        }
    }

    #[inline]
    pub fn get(&self, i: usize) -> u32 {
        debug_assert!(i < self.len);
        let width = self.width;
        let bit = i * width as usize;
        let (wi, off) = (bit / 64, (bit % 64) as u32);
        let mut v = self.data[wi] >> off;
        if off + width > 64 {
            v |= self.data[wi + 1] << (64 - off);
        }
        (v & ((1u64 << width) - 1)) as u32
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn heap_bytes(&self) -> usize {
        self.data.len() * 8
    }
}

/// One LOUDS compact trie over one SPO ordering (three levels).
pub struct CompactTrie {
    bv: SelectBitVec,
    seq: IntSeq,
    /// seq position of the first leaf-level (depth 3) symbol
    /// (= number of level-1 + level-2 nodes).
    leaf_base: usize,
}

impl CompactTrie {
    /// Build from triples sorted by the trie's component order. Duplicate
    /// triples are tolerated (they collapse into one leaf), mirroring the
    /// reference `sym_level` walk.
    /// Build scratch is the dominant term in the compact index's peak
    /// memory, so it is kept to two vectors, both `u32`, both reserved
    /// exactly by a counting pre-pass. The earlier version grew three
    /// vectors by doubling — `syms`, a `Vec<usize>` of child counts, and a
    /// `Vec<usize>` of their running sum — which cost more scratch than
    /// the whole finished trie and made the compact index resident-larger
    /// than the arrays it replaces.
    ///
    /// The child-count vector is redundant: its running sum is exactly the
    /// zero positions, so the sum is accumulated in place and the counts
    /// never stored.
    pub fn from_sorted(sorted: &[[u32; 3]]) -> Self {
        // Pre-pass: the node count per level, so both vectors are
        // allocated once at their final size. Counting is a pointer walk
        // with no allocation, and it removes every reallocation spike.
        let mut nodes = 0usize;
        if !sorted.is_empty() {
            for l in 0..3usize {
                nodes += 1;
                for i in 1..sorted.len() {
                    if sorted[i - 1][..=l] != sorted[i][..=l] {
                        nodes += 1;
                    }
                }
            }
        }

        let mut syms: Vec<u32> = Vec::with_capacity(nodes);
        // One leading zero, then a zero at each cumulative child count.
        let mut zeros: Vec<u32> = Vec::with_capacity(nodes + 1);
        zeros.push(0);
        let mut cum = 0u32;
        let mut leaf_base = 0usize;

        if !sorted.is_empty() {
            for l in 0..3usize {
                if l == 2 {
                    leaf_base = syms.len();
                }
                let mut children = 0u32;
                for i in 1..sorted.len() {
                    let (prev, curr) = (&sorted[i - 1], &sorted[i]);
                    if prev[..=l] != curr[..=l] {
                        syms.push(prev[l]);
                        children += 1;
                        if prev[..l] != curr[..l] {
                            cum += children;
                            zeros.push(cum);
                            children = 0;
                        }
                    }
                }
                syms.push(sorted[sorted.len() - 1][l]);
                children += 1;
                cum += children;
                zeros.push(cum);
            }
        }
        debug_assert_eq!(cum as usize, syms.len());
        debug_assert_eq!(syms.len(), nodes, "the pre-pass must count exactly");

        let bv = SelectBitVec::from_zero_positions(syms.len() + 1, &zeros);
        drop(zeros);
        let seq = IntSeq::from_slice(&syms);
        drop(syms);
        CompactTrie { bv, seq, leaf_base }
    }

    /// Root handle.
    #[inline]
    pub fn root(&self) -> usize {
        0
    }

    /// Number of children of the node at handle `it`.
    #[inline]
    pub fn children(&self, it: usize) -> usize {
        self.bv.succ0(it + 1) - it
    }

    /// Handle of the node whose symbol sits at seq position `p`.
    #[inline]
    pub fn node_handle(&self, p: usize) -> usize {
        self.bv.select0(p + 2)
    }

    /// Symbol at seq position `p`.
    #[inline]
    pub fn sym(&self, p: usize) -> u32 {
        self.seq.get(p)
    }

    /// seq position of the first leaf-level symbol.
    #[inline]
    pub fn leaf_base(&self) -> usize {
        self.leaf_base
    }

    /// First value ≥ `val` in `seq[beg..=end]` (a node's child block, sorted
    /// ascending). Returns `(value, seq_pos)`; `None` when every symbol in
    /// the block is smaller.
    pub fn seek(&self, val: u32, beg: usize, end: usize) -> Option<(u32, usize)> {
        if self.seq.get(end) < val {
            return None;
        }
        let (mut lo, mut hi) = (beg, end);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.seq.get(mid) < val {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Some((self.seq.get(lo), lo))
    }

    pub fn is_empty(&self) -> bool {
        self.seq.is_empty()
    }

    pub fn heap_bytes(&self) -> usize {
        self.bv.heap_bytes() + self.seq.heap_bytes()
    }
}

/// The compact analog of the six sorted arrays: six LOUDS tries plus the
/// SPO-ordered edge-id table for bag-multiplicity reconstruction.
pub struct CompactTripleIndex {
    tries: [CompactTrie; 6],
    /// Group boundaries into `leaf_eids`, one group per distinct (s, p, o)
    /// leaf in SPO order; `len = n_leaves + 1`.
    leaf_offsets: Vec<u32>,
    /// Edge ids sorted by (s, p, o, eid), duplicates preserved.
    leaf_eids: Vec<u32>,
    /// Total raw triples including duplicates (parity with the array
    /// index's `len()`).
    raw_len: usize,
}

/// Component permutations, indexed by `TrieOrder as usize`
/// (SPO, SOP, POS, PSO, OSP, OPS). Must stay in sync with
/// `triple_index::TrieOrder` and the iterator's `choose_trie`.
const ORDERS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 2, 0],
    [1, 0, 2],
    [2, 0, 1],
    [2, 1, 0],
];

impl CompactTripleIndex {
    /// Build from raw `(s, p, o, eid)` triples (unsorted, duplicates kept).
    ///
    /// Takes the vector **by value** and works one buffer at a time. The
    /// point of the compact representation is to hold less memory than
    /// the six arrays, and it cannot deliver that if building it costs
    /// more than the arrays it replaces. Three things keep the peak down:
    ///
    /// 1. The caller's triples are consumed, not copied, so the raw
    ///    vector and a sorted duplicate never coexist.
    /// 2. The eid column is dropped as soon as the leaf table is built —
    ///    the tries only need `(s, p, o)` — which is a quarter of the
    ///    remaining working set.
    /// 3. **One** working buffer is permuted in place across all six
    ///    orderings instead of deriving a fresh one per trie. Going from
    ///    ordering `a` to ordering `b` is a column permutation, so the
    ///    buffer is re-sorted rather than rebuilt and never reallocated.
    pub fn from_raw(mut raw: Vec<(u32, u32, u32, u32)>) -> Self {
        let raw_len = raw.len();

        // SPO with eids: sorted once, feeds both the SPO trie and the
        // leaf-eid table.
        raw.sort_unstable();

        let mut leaf_offsets: Vec<u32> = Vec::new();
        let mut leaf_eids: Vec<u32> = Vec::with_capacity(raw.len());
        let mut prev: Option<(u32, u32, u32)> = None;
        for &(s, p, o, e) in &raw {
            if prev != Some((s, p, o)) {
                leaf_offsets.push(leaf_eids.len() as u32);
                prev = Some((s, p, o));
            }
            leaf_eids.push(e);
        }
        leaf_offsets.push(leaf_eids.len() as u32);

        // Shed the eid column, then the source vector.
        let mut work: Vec<[u32; 3]> = Vec::with_capacity(raw.len());
        work.extend(raw.iter().map(|&(s, p, o, _)| [s, p, o]));
        drop(raw);

        // `work` is in ordering `cur`; re-permute its columns into the
        // next ordering and re-sort. `inv` inverts the current order so a
        // component can be found by its SPO identity.
        let mut cur = ORDERS[0];
        let mut tries: Vec<CompactTrie> = Vec::with_capacity(6);
        for (order, next) in ORDERS.iter().enumerate() {
            if order != 0 {
                let mut inv = [0usize; 3];
                for (i, &c) in cur.iter().enumerate() {
                    inv[c] = i;
                }
                let map = [inv[next[0]], inv[next[1]], inv[next[2]]];
                for row in work.iter_mut() {
                    *row = [row[map[0]], row[map[1]], row[map[2]]];
                }
                work.sort_unstable();
                cur = *next;
            }
            tries.push(CompactTrie::from_sorted(&work));
        }
        drop(work);

        let tries: [CompactTrie; 6] = tries
            .try_into()
            .unwrap_or_else(|_| unreachable!("ORDERS has six entries"));

        CompactTripleIndex {
            tries,
            leaf_offsets,
            leaf_eids,
            raw_len,
        }
    }

    #[inline]
    pub fn trie(&self, i: usize) -> &CompactTrie {
        &self.tries[i]
    }

    pub fn len(&self) -> usize {
        self.raw_len
    }

    pub fn is_empty(&self) -> bool {
        self.raw_len == 0
    }

    /// Edge ids of the (s, p, o) leaf, via an SPO trie descent. Empty when
    /// the triple is absent.
    pub fn eids_for(&self, s: u32, p: u32, o: u32) -> &[u32] {
        let trie = &self.tries[0];
        if trie.is_empty() {
            return &[];
        }
        let mut handle = trie.root();
        let mut pos = 0usize;
        for (depth, val) in [s, p, o].into_iter().enumerate() {
            let cnt = trie.children(handle);
            let beg = handle;
            let end = beg + cnt - 1;
            match trie.seek(val, beg, end) {
                Some((v, at)) if v == val => pos = at,
                _ => return &[],
            }
            if depth < 2 {
                handle = trie.node_handle(pos);
            }
        }
        let leaf = pos - trie.leaf_base();
        let lo = self.leaf_offsets[leaf] as usize;
        let hi = self.leaf_offsets[leaf + 1] as usize;
        &self.leaf_eids[lo..hi]
    }

    pub fn heap_bytes(&self) -> usize {
        self.tries.iter().map(|t| t.heap_bytes()).sum::<usize>()
            + self.leaf_offsets.len() * 4
            + self.leaf_eids.len() * 4
    }

    /// Per-trie heap footprint in `TrieOrder` index order. Sums to
    /// `heap_bytes()` minus `side_table_bytes()`.
    pub fn trie_heap_bytes(&self) -> [usize; 6] {
        std::array::from_fn(|i| self.tries[i].heap_bytes())
    }

    /// Heap of the eid side table (`leaf_offsets` + `leaf_eids`). It belongs
    /// to no single trie: the tries collapse parallel edges into one leaf, so
    /// this table is what restores ISO bag multiplicity for all six.
    pub fn side_table_bytes(&self) -> usize {
        self.leaf_offsets.len() * 4 + self.leaf_eids.len() * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_bitvec() {
        // bv = [0,1,0,1,0,0,1,0,0,0] (the worked LOUDS example)
        let zeros = [0u32, 2, 4, 5, 7, 8, 9];
        let bv = SelectBitVec::from_zero_positions(10, &zeros);
        for (k, &p) in zeros.iter().enumerate() {
            assert_eq!(bv.select0(k + 1), p as usize, "select0({})", k + 1);
        }
        assert_eq!(bv.succ0(0), 0);
        assert_eq!(bv.succ0(1), 2);
        assert_eq!(bv.succ0(3), 4);
        assert_eq!(bv.succ0(5), 5);
        assert_eq!(bv.succ0(6), 7);
        assert_eq!(bv.succ0(9), 9);
        assert_eq!(bv.succ0(10), 10); // past the end
    }

    #[test]
    fn test_select_bitvec_large() {
        // Cross-check select0/succ0 against a naive model on a pseudo-random
        // pattern large enough to exercise the sampling.
        let len = 100_000;
        let mut zeros = Vec::new();
        let mut x: u64 = 0x9e3779b97f4a7c15;
        for i in 0..len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            if x % 3 == 0 {
                zeros.push(i as u32);
            }
        }
        let bv = SelectBitVec::from_zero_positions(len, &zeros);
        for (k, &p) in zeros.iter().enumerate() {
            assert_eq!(bv.select0(k + 1), p as usize);
        }
        // succ0 spot checks
        let mut zi = 0;
        for i in (0..len).step_by(97) {
            while zi < zeros.len() && (zeros[zi] as usize) < i {
                zi += 1;
            }
            let expect = zeros.get(zi).map(|&z| z as usize).unwrap_or(len);
            assert_eq!(bv.succ0(i), expect, "succ0({i})");
        }
    }

    #[test]
    fn test_int_seq() {
        let vals = [0u32, 1, 7, 1000, 123456, u32::MAX, 42];
        let seq = IntSeq::from_slice(&vals);
        for (i, &v) in vals.iter().enumerate() {
            assert_eq!(seq.get(i), v);
        }
        // Narrow widths spanning word boundaries
        let vals: Vec<u32> = (0..1000).map(|i| i % 5).collect();
        let seq = IntSeq::from_slice(&vals);
        for (i, &v) in vals.iter().enumerate() {
            assert_eq!(seq.get(i), v);
        }
    }

    #[test]
    fn test_compact_trie_worked_example() {
        // Triples (SPO-sorted): (1,1,1), (1,1,2), (1,2,1), (2,1,1)
        // syms = [1,2, 1,2,1, 1,2,1,1], lengths = [2, 2,1, 2,1,1]
        // bv   = [0,1,0,1,0,0,1,0,0,0]
        let trie = CompactTrie::from_sorted(&[[1, 1, 1], [1, 1, 2], [1, 2, 1], [2, 1, 1]]);

        // Root: two children with symbols 1, 2 at seq[0..2)
        assert_eq!(trie.children(trie.root()), 2);
        assert_eq!(trie.sym(0), 1);
        assert_eq!(trie.sym(1), 2);

        // Node s=1 (symbol at seq pos 0): handle 2, children [1, 2]
        let h1 = trie.node_handle(0);
        assert_eq!(h1, 2);
        assert_eq!(trie.children(h1), 2);
        assert_eq!(trie.sym(2), 1);
        assert_eq!(trie.sym(3), 2);

        // Node s=2 (seq pos 1): handle 4, one child p=1
        let h2 = trie.node_handle(1);
        assert_eq!(h2, 4);
        assert_eq!(trie.children(h2), 1);
        assert_eq!(trie.sym(4), 1);

        // Node (1,1) (seq pos 2): handle 5, children o = [1, 2]
        let h11 = trie.node_handle(2);
        assert_eq!(h11, 5);
        assert_eq!(trie.children(h11), 2);
        assert_eq!(trie.sym(5), 1);
        assert_eq!(trie.sym(6), 2);

        // Node (1,2) (seq pos 3): handle 7, child o = 1
        let h12 = trie.node_handle(3);
        assert_eq!(h12, 7);
        assert_eq!(trie.children(h12), 1);
        assert_eq!(trie.sym(7), 1);

        // Node (2,1) (seq pos 4): handle 8, child o = 1
        let h21 = trie.node_handle(4);
        assert_eq!(h21, 8);
        assert_eq!(trie.children(h21), 1);
        assert_eq!(trie.sym(8), 1);

        // Leaf base: 2 level-1 syms + 3 level-2 syms = 5
        assert_eq!(trie.leaf_base(), 5);

        // seek within root block
        assert_eq!(trie.seek(1, 0, 1), Some((1, 0)));
        assert_eq!(trie.seek(2, 0, 1), Some((2, 1)));
        assert_eq!(trie.seek(3, 0, 1), None);
    }

    #[test]
    fn test_compact_trie_duplicates_collapse() {
        // Duplicate triples collapse into one leaf.
        let trie = CompactTrie::from_sorted(&[[1, 1, 1], [1, 1, 1], [1, 1, 2]]);
        assert_eq!(trie.children(trie.root()), 1);
        let h = trie.node_handle(0); // s=1
        assert_eq!(trie.children(h), 1);
        let h = trie.node_handle(1); // (1,1)
        assert_eq!(trie.children(h), 2); // objects 1, 2
    }

    #[test]
    fn test_compact_index_eids() {
        // Two parallel edges on (1,0,2), one on (2,0,3), plus a duplicate
        // (self-loop mirror style) eid on (3,1,3).
        let raw = vec![
            (1, 0, 2, 10),
            (1, 0, 2, 11),
            (2, 0, 3, 12),
            (3, 1, 3, 13),
            (3, 1, 3, 13),
        ];
        let idx = CompactTripleIndex::from_raw(raw);
        assert_eq!(idx.len(), 5);
        assert_eq!(idx.eids_for(1, 0, 2), &[10, 11]);
        assert_eq!(idx.eids_for(2, 0, 3), &[12]);
        assert_eq!(idx.eids_for(3, 1, 3), &[13, 13]);
        assert_eq!(idx.eids_for(9, 9, 9), &[] as &[u32]);
    }

    #[test]
    fn test_compact_index_empty() {
        let idx = CompactTripleIndex::from_raw(Vec::new());
        assert!(idx.is_empty());
        assert_eq!(idx.eids_for(0, 0, 0), &[] as &[u32]);
    }
}
