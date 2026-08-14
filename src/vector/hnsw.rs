//! HNSW proximity graph: layout and serialization.
//!
//! The build algorithm and the incremental cursor live alongside this
//! structure; what is defined here is the persisted shape, so the
//! sidecar reader/writer does not depend on how the graph was produced.
//!
//! Neighbour lists hold **row indices** into the sidecar's `ids` / `data`
//! arrays, not graph node ids. That keeps the graph self-contained: it
//! can be validated and traversed without resolving anything against the
//! database, and a row index is always in `0..count`.

use std::io;

/// Per-node adjacency across layers. `levels[i][l]` is the neighbour
/// list of row `i` at layer `l`, with layer 0 the dense bottom layer
/// that every row belongs to. A row's top layer is `levels[i].len() - 1`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hnsw {
    /// Max neighbours per node on layers above 0.
    pub m: u32,
    /// Max neighbours per node on layer 0 (conventionally `2 * m`).
    pub m0: u32,
    /// Candidate-list width used during construction.
    pub ef_construction: u32,
    /// Row index of the entry point, on the topmost layer.
    pub entry: u32,
    /// Adjacency, indexed by row.
    pub levels: Vec<Vec<Vec<u32>>>,
}

impl Hnsw {
    /// Resident bytes of the proximity graph. Three levels of `Vec`
    /// nesting, so the per-`Vec` header is counted, not just the ids —
    /// on layer 0 with `m0` neighbours it is a real fraction of the total.
    pub fn heap_bytes(&self) -> usize {
        let hdr = std::mem::size_of::<Vec<u32>>();
        self.levels
            .iter()
            .map(|layer| {
                hdr + layer.capacity() * hdr
                    + layer.iter().map(|nbrs| nbrs.capacity() * 4).sum::<usize>()
            })
            .sum()
    }

    pub fn len(&self) -> usize {
        self.levels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// Topmost layer index present in the graph.
    pub fn max_level(&self) -> usize {
        self.levels
            .iter()
            .map(|per_node| per_node.len().saturating_sub(1))
            .max()
            .unwrap_or(0)
    }

    /// Neighbours of row `i` at layer `l`, empty when the row does not
    /// reach that layer.
    pub fn neighbours(&self, i: usize, l: usize) -> &[u32] {
        match self.levels.get(i) {
            Some(per_node) => match per_node.get(l) {
                Some(list) => list,
                None => &[],
            },
            None => &[],
        }
    }

    /// Structural validation: every neighbour must be an in-range row
    /// index, and the entry point must exist. Called after decoding so a
    /// corrupt or mismatched sidecar fails loudly instead of panicking
    /// deep inside a traversal.
    pub fn validate(&self, count: usize) -> io::Result<()> {
        if self.levels.len() != count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "hnsw adjacency covers {} rows but the sidecar holds {count}",
                    self.levels.len()
                ),
            ));
        }
        if count > 0 && self.entry as usize >= count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "hnsw entry point {} out of range (count {count})",
                    self.entry
                ),
            ));
        }
        for (i, per_node) in self.levels.iter().enumerate() {
            for (l, list) in per_node.iter().enumerate() {
                for &n in list {
                    if n as usize >= count {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "hnsw neighbour {n} of row {i} at layer {l} is out of range \
                                 (count {count})"
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Encode into `out`.
    ///
    /// ```text
    /// m u32 | m0 u32 | ef_construction u32 | entry u32 | count u32
    /// per row: nlevels u32, then per layer: deg u32, deg × u32 neighbours
    /// ```
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.m.to_le_bytes());
        out.extend_from_slice(&self.m0.to_le_bytes());
        out.extend_from_slice(&self.ef_construction.to_le_bytes());
        out.extend_from_slice(&self.entry.to_le_bytes());
        out.extend_from_slice(&(self.levels.len() as u32).to_le_bytes());
        for per_node in &self.levels {
            out.extend_from_slice(&(per_node.len() as u32).to_le_bytes());
            for list in per_node {
                out.extend_from_slice(&(list.len() as u32).to_le_bytes());
                for &n in list {
                    out.extend_from_slice(&n.to_le_bytes());
                }
            }
        }
    }

    /// Decode from `rd`, the inverse of `encode`.
    pub fn decode(rd: &mut super::sidecar::ByteReader<'_>) -> io::Result<Hnsw> {
        let m = rd.u32()?;
        let m0 = rd.u32()?;
        let ef_construction = rd.u32()?;
        let entry = rd.u32()?;
        let count = rd.u32()? as usize;
        let mut levels = Vec::with_capacity(count);
        for _ in 0..count {
            let nlevels = rd.u32()? as usize;
            let mut per_node = Vec::with_capacity(nlevels);
            for _ in 0..nlevels {
                let deg = rd.u32()? as usize;
                per_node.push(rd.u32_vec(deg)?);
            }
            levels.push(per_node);
        }
        Ok(Hnsw {
            m,
            m0,
            ef_construction,
            entry,
            levels,
        })
    }
}

// ---------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------

/// Build parameters, following Malkov & Yashunin's naming.
#[derive(Debug, Clone, Copy)]
pub struct HnswParams {
    /// Neighbours kept per node on layers above 0. Layer 0 keeps `2·m`.
    pub m: usize,
    /// Candidate-list width during construction. Larger means a better
    /// graph and a slower build.
    pub ef_construction: usize,
    /// PRNG seed for the layer assignment. Fixed by default so a build
    /// is reproducible: the benchmark compares strategies, and a graph
    /// that changed between runs would smear that comparison.
    pub seed: u64,
}

impl Default for HnswParams {
    fn default() -> Self {
        HnswParams {
            m: 16,
            ef_construction: 200,
            seed: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

/// xorshift64* — a few lines, no dependency, and deterministic across
/// platforms. Only used for HNSW layer assignment, where the statistical
/// demands are mild (an exponential-ish level distribution).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `(0, 1]`, so `ln` never sees zero.
    fn next_unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 1.0) / 9_007_199_254_740_992.0
    }
}

/// A `(distance, row)` pair ordered by distance, ties broken by row so
/// the order is total and the build is deterministic. `f32` is not
/// `Ord`, hence the wrapper; see `metric::cmp_dist`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cand {
    d: f32,
    i: u32,
}

impl Eq for Cand {}

impl Ord for Cand {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        crate::vector::metric::cmp_dist(self.d, other.d).then_with(|| self.i.cmp(&other.i))
    }
}

impl PartialOrd for Cand {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Hnsw {
    fn max_degree(&self, layer: usize) -> usize {
        if layer == 0 {
            self.m0 as usize
        } else {
            self.m as usize
        }
    }

    /// Build a proximity graph over every row of `set`.
    ///
    /// Rows are inserted in stored order. The layer of each row is drawn
    /// from `floor(-ln(U) / ln(m))`, the standard exponential decay that
    /// makes the upper layers a sparse navigable skeleton over the dense
    /// layer 0.
    pub fn build(set: &super::store::VectorSet, params: HnswParams) -> Hnsw {
        let count = set.len();
        let m = params.m.max(2);
        let m0 = m * 2;
        let mut graph = Hnsw {
            m: m as u32,
            m0: m0 as u32,
            ef_construction: params.ef_construction.max(m) as u32,
            entry: 0,
            levels: Vec::with_capacity(count),
        };
        if count == 0 {
            return graph;
        }

        let mut rng = Rng::new(params.seed);
        let ml = 1.0 / (m as f64).ln();
        let node_levels: Vec<usize> = (0..count)
            .map(|_| (-rng.next_unit().ln() * ml).floor() as usize)
            .collect();

        for &lvl in node_levels.iter() {
            graph.levels.push(vec![Vec::new(); lvl + 1]);
        }
        // The entry point is the highest-layer row seen so far; ties go
        // to the earliest, which keeps the build order-independent.
        let mut max_level = node_levels[0];
        graph.entry = 0;

        let ef = graph.ef_construction as usize;
        for (i, &lvl) in node_levels.iter().enumerate().skip(1) {
            let mut ep = graph.entry as usize;

            // Greedy descent through the layers this row does not join.
            let mut lc = max_level;
            while lc > lvl {
                ep = graph.greedy_descend(set, i, ep, lc);
                lc -= 1;
            }

            // Connect on every layer the row does join.
            let top = lvl.min(max_level);
            for layer in (0..=top).rev() {
                let w = graph.search_layer(set, i, ep, ef, layer);
                let picked = graph.select_neighbours(set, i, &w, graph.max_degree(layer));
                for &n in &picked {
                    graph.levels[i][layer].push(n);
                    graph.levels[n as usize][layer].push(i as u32);
                }
                // Adding a back-link can push a neighbour over its degree
                // cap; re-run the diversity heuristic on its full list.
                for &n in &picked {
                    let cap = graph.max_degree(layer);
                    if graph.levels[n as usize][layer].len() > cap {
                        graph.prune(set, n as usize, layer, cap);
                    }
                }
                ep = match w.first() {
                    Some(c) => c.i as usize,
                    None => ep,
                };
            }

            if lvl > max_level {
                max_level = lvl;
                graph.entry = i as u32;
            }
        }

        graph
    }

    /// Walk downhill at `layer` until no neighbour is closer to `target`.
    fn greedy_descend(
        &self,
        set: &super::store::VectorSet,
        target: usize,
        mut cur: usize,
        layer: usize,
    ) -> usize {
        let mut best = row_dist(set, target, cur);
        loop {
            let mut improved = false;
            for &n in self.neighbours(cur, layer) {
                let d = row_dist(set, target, n as usize);
                if d < best {
                    best = d;
                    cur = n as usize;
                    improved = true;
                }
            }
            if !improved {
                return cur;
            }
        }
    }

    /// Best-first search at `layer`, keeping the `ef` closest rows to
    /// `target`. Returns them sorted nearest-first.
    fn search_layer(
        &self,
        set: &super::store::VectorSet,
        target: usize,
        ep: usize,
        ef: usize,
        layer: usize,
    ) -> Vec<Cand> {
        use std::cmp::Reverse;
        use std::collections::{BinaryHeap, HashSet};

        let start = Cand {
            d: row_dist(set, target, ep),
            i: ep as u32,
        };
        let mut visited: HashSet<u32> = HashSet::new();
        visited.insert(ep as u32);
        let mut frontier: BinaryHeap<Reverse<Cand>> = BinaryHeap::new();
        frontier.push(Reverse(start));
        // Max-heap of the current best `ef`, so the worst is at the top
        // and can be evicted in O(log ef).
        let mut best: BinaryHeap<Cand> = BinaryHeap::new();
        best.push(start);

        while let Some(Reverse(c)) = frontier.pop() {
            let worst = match best.peek() {
                Some(w) => w.d,
                None => f32::INFINITY,
            };
            if c.d > worst && best.len() >= ef {
                break;
            }
            for &n in self.neighbours(c.i as usize, layer) {
                if !visited.insert(n) {
                    continue;
                }
                let d = row_dist(set, target, n as usize);
                let worst = match best.peek() {
                    Some(w) => w.d,
                    None => f32::INFINITY,
                };
                if best.len() < ef || d < worst {
                    let cand = Cand { d, i: n };
                    frontier.push(Reverse(cand));
                    best.push(cand);
                    if best.len() > ef {
                        best.pop();
                    }
                }
            }
        }

        let mut out = best.into_vec();
        out.sort();
        out
    }

    /// Algorithm 4's diversity heuristic: keep a candidate only when it
    /// is closer to `target` than to every neighbour already kept. This
    /// is what stops all of a node's links from pointing into the same
    /// dense cluster, which is what makes the graph navigable.
    ///
    /// If diversity leaves fewer than `cap` links, top up with the
    /// nearest rejects; a sparse node is worse than a slightly redundant
    /// one, since a node with no links is unreachable.
    fn select_neighbours(
        &self,
        set: &super::store::VectorSet,
        target: usize,
        candidates: &[Cand],
        cap: usize,
    ) -> Vec<u32> {
        let mut kept: Vec<u32> = Vec::with_capacity(cap);
        let mut rejected: Vec<u32> = Vec::new();
        for c in candidates {
            if c.i as usize == target {
                continue;
            }
            if kept.len() >= cap {
                break;
            }
            let diverse = kept
                .iter()
                .all(|&s| c.d < row_dist(set, c.i as usize, s as usize));
            if diverse {
                kept.push(c.i);
            } else {
                rejected.push(c.i);
            }
        }
        for r in rejected {
            if kept.len() >= cap {
                break;
            }
            kept.push(r);
        }
        kept
    }

    /// Re-select `node`'s links at `layer` down to `cap`.
    fn prune(&mut self, set: &super::store::VectorSet, node: usize, layer: usize, cap: usize) {
        let mut cands: Vec<Cand> = self.levels[node][layer]
            .iter()
            .map(|&n| Cand {
                d: row_dist(set, node, n as usize),
                i: n,
            })
            .collect();
        cands.sort();
        cands.dedup_by_key(|c| c.i);
        let kept = self.select_neighbours(set, node, &cands, cap);
        self.levels[node][layer] = kept;
    }
}

fn row_dist(set: &super::store::VectorSet, a: usize, b: usize) -> f32 {
    set.dist_between(a, b)
}

// ---------------------------------------------------------------------
// Incremental cursor
// ---------------------------------------------------------------------

/// Unbounded best-first traversal of layer 0.
///
/// Ordinary HNSW search fixes `ef` and stops; this cursor never stops,
/// so it behaves as an incremental nearest-neighbour enumerator.
///
/// Emission runs a fixed **lookahead** behind the exploration: before
/// handing back the `i`-th neighbour the cursor has expanded at least
/// `i + ef` rows, so the row it emits is the minimum over a frontier
/// that a plain `ef`-bounded HNSW search would also have seen. Emitting
/// straight off the frontier instead is measurably wrong at top-1 — it
/// hands back the greedy-descent seed after expanding a handful of rows,
/// and on a 400×8 uniform set that inverts the first two neighbours.
///
/// The order is still only *approximately* non-decreasing: a row closer
/// than the one just emitted can sit behind an unexplored part of the
/// graph. That is the approximation an ANN index trades for speed, and
/// consumers that cut on a threshold must allow slack for it.
///
/// Two further consequences worth stating plainly. Rows in a layer-0
/// component not reachable from the entry point are never emitted, so a
/// cursor can end before covering the attribute. And driving the cursor
/// to exhaustion costs more than a brute-force scan; it pays off only
/// because every strategy stops early.
pub struct HnswCursor<'a> {
    set: &'a super::store::VectorSet,
    hnsw: &'a Hnsw,
    q: &'a [f32],
    q_norm: f32,
    /// Discovered rows not yet emitted, nearest at the top.
    frontier: std::collections::BinaryHeap<std::cmp::Reverse<Cand>>,
    /// Discovered rows not yet expanded, nearest at the top. Kept apart
    /// from `frontier` because a row leaves the two at different times:
    /// it is expanded during lookahead and emitted later.
    pending: std::collections::BinaryHeap<std::cmp::Reverse<Cand>>,
    /// Ever pushed; stops re-discovery.
    discovered: Vec<bool>,
    /// Neighbours already pushed; stops re-expansion.
    expanded_rows: Vec<bool>,
    expansions: u64,
    emitted: u64,
    ef: usize,
}

/// How far exploration stays ahead of emission. Mirrors the `ef` of an
/// ordinary HNSW search: the same knob, applied continuously instead of
/// once. 64 is the usual default and recovers exact top-1 on the
/// datasets the unit tests cover.
pub const DEFAULT_EF_SEARCH: usize = 64;

impl<'a> HnswCursor<'a> {
    pub fn new(set: &'a super::store::VectorSet, hnsw: &'a Hnsw, q: &'a [f32]) -> HnswCursor<'a> {
        HnswCursor::with_ef(set, hnsw, q, DEFAULT_EF_SEARCH)
    }

    pub fn with_ef(
        set: &'a super::store::VectorSet,
        hnsw: &'a Hnsw,
        q: &'a [f32],
        ef: usize,
    ) -> HnswCursor<'a> {
        let count = set.len();
        let q_norm = set.query_norm(q);
        let mut cursor = HnswCursor {
            set,
            hnsw,
            q,
            q_norm,
            frontier: std::collections::BinaryHeap::new(),
            pending: std::collections::BinaryHeap::new(),
            discovered: vec![false; count],
            expanded_rows: vec![false; count],
            expansions: 0,
            emitted: 0,
            ef: ef.max(1),
        };
        if count == 0 {
            return cursor;
        }

        // Greedy descent through the upper layers seeds the frontier at
        // a good entry into layer 0 — the same navigation ordinary HNSW
        // search performs before its `ef` phase.
        let mut cur = hnsw.entry as usize;
        if cur >= count {
            cur = 0;
        }
        let mut best = cursor.dist(cur);
        let mut layer = hnsw.max_level();
        while layer > 0 {
            loop {
                let mut improved = false;
                for &n in hnsw.neighbours(cur, layer) {
                    let d = cursor.dist(n as usize);
                    if d < best {
                        best = d;
                        cur = n as usize;
                        improved = true;
                    }
                }
                if !improved {
                    break;
                }
            }
            layer -= 1;
        }
        cursor.discover(cur, best);
        cursor
    }

    fn dist(&self, row: usize) -> f32 {
        self.set.dist_at(self.q, self.q_norm, row)
    }

    fn discover(&mut self, row: usize, d: f32) {
        self.discovered[row] = true;
        let c = std::cmp::Reverse(Cand { d, i: row as u32 });
        self.frontier.push(c);
        self.pending.push(c);
    }

    /// Expand the closest not-yet-expanded row. Returns false when the
    /// reachable part of layer 0 is exhausted.
    fn expand_one(&mut self) -> bool {
        while let Some(std::cmp::Reverse(c)) = self.pending.pop() {
            let row = c.i as usize;
            if self.expanded_rows[row] {
                continue;
            }
            self.expanded_rows[row] = true;
            self.expansions += 1;
            for k in 0..self.hnsw.neighbours(row, 0).len() {
                let n = self.hnsw.neighbours(row, 0)[k] as usize;
                if n >= self.discovered.len() || self.discovered[n] {
                    continue;
                }
                let d = self.dist(n);
                self.discover(n, d);
            }
            return true;
        }
        false
    }
}

impl<'a> super::cursor::NnCursor for HnswCursor<'a> {
    fn next(&mut self) -> Option<(crate::model::value::Id, f32)> {
        while self.expansions < self.emitted + self.ef as u64 {
            if !self.expand_one() {
                break;
            }
        }
        let std::cmp::Reverse(c) = self.frontier.pop()?;
        self.emitted += 1;
        Some((self.set.ids()[c.i as usize], c.d))
    }

    fn expanded(&self) -> u64 {
        self.expansions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::sidecar::ByteReader;

    fn sample() -> Hnsw {
        Hnsw {
            m: 8,
            m0: 16,
            ef_construction: 64,
            entry: 1,
            levels: vec![
                vec![vec![1, 2]],
                vec![vec![0, 2], vec![2]],
                vec![vec![0, 1]],
            ],
        }
    }

    #[test]
    fn encode_decode_round_trips() {
        let h = sample();
        let mut bytes = Vec::new();
        h.encode(&mut bytes);
        let mut rd = ByteReader::new(&bytes);
        let back = Hnsw::decode(&mut rd).expect("decode");
        assert_eq!(back, h);
        assert_eq!(rd.remaining(), 0, "decode must consume exactly its bytes");
    }

    #[test]
    fn empty_graph_round_trips() {
        let h = Hnsw::default();
        let mut bytes = Vec::new();
        h.encode(&mut bytes);
        let mut rd = ByteReader::new(&bytes);
        assert_eq!(Hnsw::decode(&mut rd).expect("decode"), h);
    }

    #[test]
    fn max_level_reads_the_deepest_stack() {
        assert_eq!(sample().max_level(), 1);
        assert_eq!(Hnsw::default().max_level(), 0);
    }

    #[test]
    fn neighbours_are_empty_off_the_end() {
        let h = sample();
        assert_eq!(h.neighbours(0, 0), &[1, 2]);
        assert_eq!(h.neighbours(0, 1), &[] as &[u32]);
        assert_eq!(h.neighbours(99, 0), &[] as &[u32]);
    }

    #[test]
    fn validate_accepts_a_consistent_graph() {
        assert!(sample().validate(3).is_ok());
    }

    #[test]
    fn validate_rejects_a_row_count_mismatch() {
        assert!(sample().validate(4).is_err());
    }

    #[test]
    fn validate_rejects_an_out_of_range_neighbour() {
        let mut h = sample();
        h.levels[0][0][1] = 99;
        assert!(h.validate(3).is_err());
    }

    #[test]
    fn validate_rejects_an_out_of_range_entry() {
        let mut h = sample();
        h.entry = 7;
        assert!(h.validate(3).is_err());
    }

    // -- build + cursor ------------------------------------------------

    use crate::vector::cursor::NnCursor;
    use crate::vector::metric::Metric;
    use crate::vector::store::VectorSet;

    /// `n` points spread over `dim` dimensions from a fixed seed, owned
    /// by node ids `1..=n` so a row index never accidentally equals its
    /// node id.
    fn random_set(n: usize, dim: usize, seed: u64) -> VectorSet {
        let mut rng = Rng::new(seed);
        let data: Vec<f32> = (0..n * dim)
            .map(|_| (rng.next_unit() as f32) * 2.0 - 1.0)
            .collect();
        let ids: Vec<u32> = (1..=n as u32).collect();
        VectorSet::new("emb".to_string(), dim, Metric::L2Sq, 0, ids, data)
    }

    fn drain(mut c: Box<dyn NnCursor + '_>) -> Vec<(u32, f32)> {
        let mut out = Vec::new();
        while let Some(e) = c.next() {
            out.push(e);
        }
        out
    }

    #[test]
    fn build_on_an_empty_set_is_an_empty_graph() {
        let set = VectorSet::new("e".to_string(), 2, Metric::L2Sq, 0, vec![], vec![]);
        let h = Hnsw::build(&set, HnswParams::default());
        assert!(h.is_empty());
        assert!(h.validate(0).is_ok());
    }

    #[test]
    fn build_on_a_single_row_is_valid_and_navigable() {
        let set = VectorSet::new("e".to_string(), 2, Metric::L2Sq, 0, vec![5], vec![1.0, 2.0]);
        let h = Hnsw::build(&set, HnswParams::default());
        assert!(h.validate(1).is_ok());
        let set = set.with_hnsw(h);
        let got = drain(set.cursor(&[0.0, 0.0], true));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 5);
    }

    #[test]
    fn build_produces_a_structurally_valid_graph() {
        let set = random_set(200, 8, 1);
        let h = Hnsw::build(&set, HnswParams::default());
        assert!(h.validate(200).is_ok());
        // No node may link to itself, and every node must have at least
        // one layer-0 link or it is unreachable.
        for i in 0..200 {
            assert!(
                !h.neighbours(i, 0).contains(&(i as u32)),
                "row {i} links to itself"
            );
            assert!(!h.neighbours(i, 0).is_empty(), "row {i} is isolated");
        }
    }

    #[test]
    fn build_respects_the_degree_cap() {
        let params = HnswParams {
            m: 4,
            ef_construction: 32,
            seed: 3,
        };
        let set = random_set(300, 6, 2);
        let h = Hnsw::build(&set, params);
        for i in 0..300 {
            for l in 0..h.levels[i].len() {
                assert!(
                    h.levels[i][l].len() <= h.max_degree(l),
                    "row {i} layer {l} has {} links, cap {}",
                    h.levels[i][l].len(),
                    h.max_degree(l)
                );
            }
        }
    }

    #[test]
    fn build_is_deterministic_for_a_fixed_seed() {
        let set = random_set(150, 4, 9);
        let a = Hnsw::build(&set, HnswParams::default());
        let b = Hnsw::build(&set, HnswParams::default());
        assert_eq!(a, b, "a benchmark needs a reproducible graph");
    }

    #[test]
    fn cursor_emits_distinct_rows_and_terminates() {
        let set = random_set(120, 5, 4);
        let h = Hnsw::build(&set, HnswParams::default());
        let set = set.with_hnsw(h);
        let q = vec![0.0f32; 5];
        let got = drain(set.cursor(&q, true));

        let mut seen = std::collections::HashSet::new();
        for (id, _) in &got {
            assert!(seen.insert(*id), "row {id} emitted twice");
        }
        // Layer 0 is built connected here, so the walk covers everything.
        assert_eq!(got.len(), 120);
    }

    #[test]
    fn cursor_finds_the_true_nearest_neighbour() {
        let set = random_set(400, 8, 11);
        let h = Hnsw::build(&set, HnswParams::default());
        let set = set.with_hnsw(h);
        let q: Vec<f32> = (0..8).map(|i| i as f32 * 0.05).collect();

        let exact = drain(set.cursor(&q, false));
        let approx = drain(set.cursor(&q, true));
        assert_eq!(approx[0].0, exact[0].0, "top-1 must match the oracle");
    }

    #[test]
    fn cursor_recall_at_10_is_high() {
        let set = random_set(1000, 12, 21);
        let h = Hnsw::build(&set, HnswParams::default());
        let set = set.with_hnsw(h);

        let mut rng = Rng::new(777);
        let mut hits = 0usize;
        let mut total = 0usize;
        for _ in 0..20 {
            let q: Vec<f32> = (0..12)
                .map(|_| (rng.next_unit() as f32) * 2.0 - 1.0)
                .collect();
            let exact: Vec<u32> = drain(set.cursor(&q, false))
                .into_iter()
                .take(10)
                .map(|e| e.0)
                .collect();
            let approx: std::collections::HashSet<u32> = drain(set.cursor(&q, true))
                .into_iter()
                .take(10)
                .map(|e| e.0)
                .collect();
            hits += exact.iter().filter(|id| approx.contains(id)).count();
            total += exact.len();
        }
        let recall = hits as f64 / total as f64;
        assert!(recall > 0.9, "recall@10 was {recall:.3}, expected > 0.9");
    }

    #[test]
    fn cursor_expands_far_fewer_nodes_than_a_full_scan_for_the_top_k() {
        let set = random_set(2000, 12, 33);
        let h = Hnsw::build(&set, HnswParams::default());
        let set = set.with_hnsw(h);
        let q = vec![0.25f32; 12];

        let mut c = set.cursor(&q, true);
        for _ in 0..10 {
            assert!(c.next().is_some());
        }
        // This is the property the whole in-LTJ strategy rests on: the
        // cost of the first k neighbours is sublinear in the corpus.
        assert!(
            c.expanded() < 200,
            "expanded {} nodes for the top 10 of 2000",
            c.expanded()
        );
    }

    #[test]
    fn a_narrow_lookahead_trades_accuracy_for_expansions() {
        // The `ef` knob has to actually do something, or the accuracy of
        // the head of the stream is an accident rather than a setting.
        let set = random_set(400, 8, 11);
        let h = Hnsw::build(&set, HnswParams::default());
        let set = set.with_hnsw(h);
        let hnsw = set.hnsw().expect("built");
        let q: Vec<f32> = (0..8).map(|i| i as f32 * 0.05).collect();

        let mut narrow = HnswCursor::with_ef(&set, hnsw, &q, 1);
        let mut wide = HnswCursor::with_ef(&set, hnsw, &q, DEFAULT_EF_SEARCH);
        let (n0, w0) = (narrow.next().unwrap(), wide.next().unwrap());

        let exact = drain(set.cursor(&q, false));
        assert_eq!(w0.0, exact[0].0, "the default lookahead is exact here");
        assert_ne!(n0.0, exact[0].0, "ef=1 is expected to miss");
        assert!(narrow.expanded() < wide.expanded());
    }

    #[test]
    fn cursor_round_trips_through_the_sidecar_encoding() {
        let set = random_set(100, 4, 5);
        let h = Hnsw::build(&set, HnswParams::default());
        let set = set.with_hnsw(h);
        let bytes = set.to_sidecar().encode();
        let back = VectorSet::from_sidecar(crate::vector::Sidecar::decode(&bytes).expect("decode"));

        let q = vec![0.1f32; 4];
        assert_eq!(drain(back.cursor(&q, true)), drain(set.cursor(&q, true)));
    }
}
