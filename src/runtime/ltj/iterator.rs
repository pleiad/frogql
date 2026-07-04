use super::compact::CompactTripleIndex;
use super::triple_index::{IndexEntry, TrieOrder, TripleIndex};

/// A term in a triple pattern: either a variable or a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Term {
    Variable(u8),
    Constant(u32),
}

/// A triple pattern (s, p, o) where each position is a variable or constant.
#[derive(Debug, Clone)]
pub struct TriplePattern {
    pub terms: [Term; 3], // [S, P, O]
}

/// Represents which position in the triple (S=0, P=1, O=2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpoPos {
    S = 0,
    P = 1,
    O = 2,
}

/// Trie ordering whose first level enumerates `querying` given the fixed
/// positions so far. Shared by the array iterator's `choose_ordering` and
/// the compact iterator's `choose_trie` — both navigate the same six
/// orderings, so the routing table is identical (and mirrors the C++
/// reference's `choose_trie`).
fn choose_order(fixed: &[SpoPos], querying: SpoPos) -> TrieOrder {
    match fixed.len() {
        0 => match querying {
            SpoPos::S => TrieOrder::SPO,
            SpoPos::P => TrieOrder::POS,
            SpoPos::O => TrieOrder::OSP,
        },
        1 => match (fixed[0], querying) {
            (SpoPos::S, SpoPos::P) => TrieOrder::SPO,
            (SpoPos::S, SpoPos::O) => TrieOrder::SOP,
            (SpoPos::P, SpoPos::O) => TrieOrder::POS,
            (SpoPos::P, SpoPos::S) => TrieOrder::PSO,
            (SpoPos::O, SpoPos::S) => TrieOrder::OSP,
            (SpoPos::O, SpoPos::P) => TrieOrder::OPS,
            (f0, q) => unreachable!("fixed={:?} querying={:?}", f0, q),
        },
        _ => match (fixed[0], fixed[1]) {
            (SpoPos::S, SpoPos::P) => TrieOrder::SPO,
            (SpoPos::S, SpoPos::O) => TrieOrder::SOP,
            (SpoPos::P, SpoPos::O) => TrieOrder::POS,
            (SpoPos::P, SpoPos::S) => TrieOrder::PSO,
            (SpoPos::O, SpoPos::S) => TrieOrder::OSP,
            (SpoPos::O, SpoPos::P) => TrieOrder::OPS,
            (f0, f1) => unreachable!("f0={:?} f1={:?}", f0, f1),
        },
    }
}

/// Iterator over one triple pattern, dispatching on the physical
/// representation the `TripleIndex` was built with. The array variant is
/// the original simplified implementation; the compact variant navigates
/// the LOUDS succinct tries (issue #66). Both expose identical semantics —
/// `tests/compact_ltj_test.rs` pins the equivalence.
pub enum LtjIterator<'a> {
    Array(ArrayLtjIterator<'a>),
    Compact(CompactLtjIterator<'a>),
}

impl<'a> LtjIterator<'a> {
    pub fn new(pattern: TriplePattern, index: &'a TripleIndex) -> Self {
        match index.compact() {
            Some(c) => LtjIterator::Compact(CompactLtjIterator::new(pattern, c)),
            None => LtjIterator::Array(ArrayLtjIterator::new(pattern, index)),
        }
    }

    /// Leap: find the smallest value >= `c` for the given variable position.
    pub fn leap(&mut self, var_pos: SpoPos, c: u32) -> Option<u32> {
        match self {
            LtjIterator::Array(it) => it.leap(var_pos, c),
            LtjIterator::Compact(it) => it.leap(var_pos, c),
        }
    }

    /// Descend one level: bind `var_pos` to `val`.
    pub fn down(&mut self, var_pos: SpoPos, val: u32) {
        match self {
            LtjIterator::Array(it) => it.down(var_pos, val),
            LtjIterator::Compact(it) => it.down(var_pos, val),
        }
    }

    /// Ascend one level.
    pub fn up(&mut self, var_pos: SpoPos) {
        match self {
            LtjIterator::Array(it) => it.up(var_pos),
            LtjIterator::Compact(it) => it.up(var_pos),
        }
    }

    /// Get all distinct values at the current level for the given variable position.
    pub fn seek_all(&mut self, var_pos: SpoPos) -> Vec<u32> {
        match self {
            LtjIterator::Array(it) => it.seek_all(var_pos),
            LtjIterator::Compact(it) => it.seek_all(var_pos),
        }
    }

    /// Number of distinct children at the current level.
    pub fn children_count(&self, var_pos: SpoPos) -> usize {
        match self {
            LtjIterator::Array(it) => it.children_count(var_pos),
            LtjIterator::Compact(it) => it.children_count(var_pos),
        }
    }

    /// Whether all 3 positions are fixed (constants + variables).
    pub fn in_last_level(&self) -> bool {
        match self {
            LtjIterator::Array(it) => it.in_last_level(),
            LtjIterator::Compact(it) => it.in_last_level(),
        }
    }

    /// Number of fixed variable positions (not counting constants).
    pub fn nfixed(&self) -> usize {
        match self {
            LtjIterator::Array(it) => it.nfixed(),
            LtjIterator::Compact(it) => it.nfixed(),
        }
    }

    /// With all three positions fixed, the edge id of the first matching
    /// index entry (see `ArrayLtjIterator::current_eid`).
    pub fn current_eid(&self) -> Option<u32> {
        match self {
            LtjIterator::Array(it) => it.current_eid(),
            LtjIterator::Compact(it) => it.current_eid(),
        }
    }

    /// All edge ids whose entries share the bound (src, label, tgt) prefix
    /// (see `ArrayLtjIterator::current_eids_all`).
    pub fn current_eids_all(&self) -> Vec<u32> {
        match self {
            LtjIterator::Array(it) => it.current_eids_all(),
            LtjIterator::Compact(it) => it.current_eids_all(),
        }
    }
}

/// Iterator for navigating a trie ordering associated with one triple pattern.
///
/// Design: maintains a stack of (SpoPos, bound_value) representing the descent path.
/// On each leap/down/up, recomputes the trie ordering and range from scratch.
/// This is simple and correct; optimization can come later.
pub struct ArrayLtjIterator<'a> {
    index: &'a TripleIndex,
    /// Constants from the triple pattern: always at the bottom of the effective stack
    constants: Vec<(SpoPos, u32)>,
    /// Stack of variable bindings (pushed by down(), popped by up())
    stack: Vec<(SpoPos, u32)>,
}

impl<'a> ArrayLtjIterator<'a> {
    pub fn new(pattern: TriplePattern, index: &'a TripleIndex) -> Self {
        let mut constants = Vec::new();
        for (i, term) in pattern.terms.iter().enumerate() {
            if let Term::Constant(val) = term {
                let pos = match i {
                    0 => SpoPos::S,
                    1 => SpoPos::P,
                    2 => SpoPos::O,
                    _ => unreachable!(),
                };
                constants.push((pos, *val));
            }
        }
        ArrayLtjIterator {
            index,
            constants,
            stack: Vec::with_capacity(3),
        }
    }

    /// The effective stack: constants first, then variable bindings.
    fn effective_stack(&self) -> Vec<(SpoPos, u32)> {
        let mut s = self.constants.clone();
        s.extend_from_slice(&self.stack);
        s
    }

    /// Choose the trie ordering so that:
    /// - Previously fixed positions (constants + stack) occupy depths 0..nfixed-1
    /// - The queried position occupies depth nfixed
    fn choose_ordering(&self, querying: SpoPos) -> TrieOrder {
        let eff = self.effective_stack();
        let fixed: Vec<SpoPos> = eff.iter().map(|&(p, _)| p).collect();
        choose_order(&fixed, querying)
    }

    /// Map an SpoPos to its depth in a given trie ordering.
    fn spo_to_depth(order: TrieOrder, pos: SpoPos) -> usize {
        let perm: [SpoPos; 3] = match order {
            TrieOrder::SPO => [SpoPos::S, SpoPos::P, SpoPos::O],
            TrieOrder::SOP => [SpoPos::S, SpoPos::O, SpoPos::P],
            TrieOrder::POS => [SpoPos::P, SpoPos::O, SpoPos::S],
            TrieOrder::PSO => [SpoPos::P, SpoPos::S, SpoPos::O],
            TrieOrder::OSP => [SpoPos::O, SpoPos::S, SpoPos::P],
            TrieOrder::OPS => [SpoPos::O, SpoPos::P, SpoPos::S],
        };
        perm.iter().position(|&p| p == pos).unwrap()
    }

    /// Compute the valid range at the query depth, given constants + stack.
    /// Returns (ordering_slice, begin, end, query_depth).
    fn compute_range(&self, querying: SpoPos) -> (&'a [IndexEntry], usize, usize, usize) {
        let order = self.choose_ordering(querying);
        let slice = self.index.get_ordering(order);
        let mut begin = 0;
        let mut end = slice.len();
        let eff = self.effective_stack();

        // Descend for each item in the effective stack (constants + variables)
        for (i, &(pos, val)) in eff.iter().enumerate() {
            let depth = Self::spo_to_depth(order, pos);
            debug_assert_eq!(
                depth, i,
                "stack position {:?} should be at depth {} but is at {}",
                pos, i, depth
            );
            let (lo, hi) = TripleIndex::range_for_key(slice, begin, end, depth, val);
            begin = lo;
            end = hi;
        }

        let query_depth = Self::spo_to_depth(order, querying);
        debug_assert_eq!(query_depth, eff.len());
        (slice, begin, end, query_depth)
    }

    /// Leap: find the smallest value >= `c` for the given variable position.
    pub fn leap(&mut self, var_pos: SpoPos, c: u32) -> Option<u32> {
        let (slice, begin, end, depth) = self.compute_range(var_pos);
        TripleIndex::leap(slice, begin, end, depth, c).map(|(val, _)| val)
    }

    /// Descend one level: bind `var_pos` to `val`.
    pub fn down(&mut self, var_pos: SpoPos, val: u32) {
        self.stack.push((var_pos, val));
    }

    /// Ascend one level.
    pub fn up(&mut self, _var_pos: SpoPos) {
        self.stack.pop();
    }

    /// Get all distinct values at the current level for the given variable position.
    pub fn seek_all(&mut self, var_pos: SpoPos) -> Vec<u32> {
        let (slice, begin, end, depth) = self.compute_range(var_pos);
        TripleIndex::all_values(slice, begin, end, depth)
    }

    /// Number of distinct children at the current level.
    pub fn children_count(&self, var_pos: SpoPos) -> usize {
        let (slice, begin, end, depth) = self.compute_range(var_pos);
        TripleIndex::distinct_count(slice, begin, end, depth)
    }

    /// Whether all 3 positions are fixed (constants + variables).
    pub fn in_last_level(&self) -> bool {
        self.constants.len() + self.stack.len() >= 2
    }

    /// Number of fixed variable positions (not counting constants).
    pub fn nfixed(&self) -> usize {
        self.stack.len()
    }

    /// With all three positions (S, P, O) fixed via constants + stack,
    /// return the edge id of the first index entry that matches. Used by
    /// LTJ result reconstruction when the triple does not bind an edge
    /// variable — a single eid is enough to recover the canonical edge
    /// for display in `_paths`.
    pub fn current_eid(&self) -> Option<u32> {
        let (slice, begin, end) = self.current_range()?;
        if begin < end {
            Some(slice[begin].3)
        } else {
            None
        }
    }

    /// All edge ids whose entries share the bound (src, label, tgt) prefix.
    /// Used when the triple binds an edge variable: the LTJ trie collapses
    /// parallel edges (same src, label, tgt, different eid) into a single
    /// search path, so the base case has to fan out one tuple per physical
    /// entry. Without this, `(a)-[e:KNOWS]->(b)` would return one row per
    /// distinct (a, KNOWS, b) triple and silently drop the rest.
    pub fn current_eids_all(&self) -> Vec<u32> {
        let Some((slice, begin, end)) = self.current_range() else {
            return Vec::new();
        };
        slice[begin..end].iter().map(|e| e.3).collect()
    }

    /// Walk SPO with the constants + stack to find the entry range whose
    /// (S, P, O) prefix matches the current bindings. Returns the slice
    /// reference and the matched range. Bails if any position is still
    /// unbound — callers should only invoke this at the base case.
    fn current_range(&self) -> Option<(&'a [IndexEntry], usize, usize)> {
        let slice = self.index.get_ordering(TrieOrder::SPO);
        let mut by_pos: [Option<u32>; 3] = [None; 3];
        for &(pos, val) in self.constants.iter().chain(self.stack.iter()) {
            by_pos[pos as usize] = Some(val);
        }
        let mut begin = 0usize;
        let mut end = slice.len();
        for (depth, slot) in by_pos.iter().enumerate() {
            let val = (*slot)?;
            let (lo, hi) = TripleIndex::range_for_key(slice, begin, end, depth, val);
            begin = lo;
            end = hi;
        }
        Some((slice, begin, end))
    }
}

/// Compact-trie iterator: a port of the reference `ltj_iterator_basic`
/// (`cltj/include/query/ltj_iterator_basic.hpp`) navigating the LOUDS
/// tries. Unlike the array iterator, descent state is incremental — a node
/// handle per fixed level — so a leap is one `children` (succ0) plus one
/// binary search inside the child block, never a full re-descent.
///
/// The six tries pair by first component ({SPO, SOP}, {POS, PSO},
/// {OSP, OPS}). The first two levels of a pair coincide, so the first
/// descent computes handles in *both* tries of the pair from the same seq
/// position, and `choose_order` later picks whichever continues the walk
/// (even trie ↔ pair slot 0, odd trie ↔ slot 1 — `trie_i & 1`).
pub struct CompactLtjIterator<'a> {
    index: &'a CompactTripleIndex,
    /// Fixed history: constants first (descended at construction), then
    /// variable bindings pushed by `down` / popped by `up`.
    fixed: Vec<(SpoPos, u32)>,
    n_constants: usize,
    /// Handles in the two tries of the active pair after the first fix.
    level1: [usize; 2],
    /// (trie index, handle) after the second fix.
    level2: (usize, usize),
    /// A constant was absent from the index: every leap misses.
    empty: bool,
    /// `Some(d)`: the descent at depth `d - 1` bound a value with no
    /// matching child (possible with repeated variables in one triple —
    /// the leapfrog agrees on a value that this triple's deeper level
    /// lacks). Levels ≥ `d` have no valid handle; leaps there return
    /// `None`, mirroring the array iterator's empty range. Cleared when
    /// `up` pops back above `d`.
    dead_from: Option<usize>,
}

impl<'a> CompactLtjIterator<'a> {
    pub fn new(pattern: TriplePattern, index: &'a CompactTripleIndex) -> Self {
        let mut it = CompactLtjIterator {
            index,
            fixed: Vec::with_capacity(3),
            n_constants: 0,
            level1: [0, 0],
            level2: (0, 0),
            empty: index.is_empty(),
            dead_from: None,
        };
        // Process constants in S, P, O order (the reference's
        // `process_constants`): descend each; a miss empties the iterator.
        for (i, term) in pattern.terms.iter().enumerate() {
            if let Term::Constant(val) = term {
                let pos = match i {
                    0 => SpoPos::S,
                    1 => SpoPos::P,
                    2 => SpoPos::O,
                    _ => unreachable!(),
                };
                if it.empty || !it.descend(pos, *val) {
                    it.empty = true;
                }
                it.n_constants += 1;
            }
        }
        it
    }

    /// Trie index for querying `pos` given the current fixed prefix.
    fn choose_trie(&self, querying: SpoPos) -> usize {
        let fixed: Vec<SpoPos> = self.fixed.iter().map(|&(p, _)| p).collect();
        choose_order(&fixed, querying) as usize
    }

    /// Handle of the current node in trie `trie_i` (whose next level
    /// enumerates the queried position).
    fn parent_handle(&self, trie_i: usize) -> usize {
        match self.fixed.len() {
            0 => 0, // root
            1 => self.level1[trie_i & 1],
            _ => {
                debug_assert_eq!(trie_i, self.level2.0);
                self.level2.1
            }
        }
    }

    /// Whether navigation state is valid at the current depth.
    fn alive(&self) -> bool {
        !self.empty && self.dead_from.map_or(true, |d| self.fixed.len() < d)
    }

    /// Child block `(trie_i, beg, end)` for querying `pos`; `None` when the
    /// iterator is dead at this depth.
    fn child_block(&self, pos: SpoPos) -> Option<(usize, usize, usize)> {
        if !self.alive() {
            return None;
        }
        let trie_i = self.choose_trie(pos);
        let parent = self.parent_handle(trie_i);
        let cnt = self.index.trie(trie_i).children(parent);
        Some((trie_i, parent, parent + cnt - 1))
    }

    /// Navigate down to child `val` of the current node, updating handles.
    /// Returns false (state pushed, but dead) when `val` is not a child.
    fn descend(&mut self, pos: SpoPos, val: u32) -> bool {
        let nfixed = self.fixed.len();
        if nfixed >= 2 || !self.alive() {
            // Third fix needs no navigation; dead levels push bare state.
            self.fixed.push((pos, val));
            return self.alive();
        }
        let Some((trie_i, beg, end)) = self.child_block(pos) else {
            self.fixed.push((pos, val));
            return false;
        };
        let trie = self.index.trie(trie_i);
        let found = match trie.seek(val, beg, end) {
            Some((v, p)) if v == val => Some(p),
            _ => None,
        };
        match found {
            Some(p) => {
                if nfixed == 0 {
                    // Handles in both tries of the pair (same seq position:
                    // the pair shares its first level).
                    let pair = trie_i & !1;
                    self.level1 = [
                        self.index.trie(pair).node_handle(p),
                        self.index.trie(pair + 1).node_handle(p),
                    ];
                } else {
                    self.level2 = (trie_i, trie.node_handle(p));
                }
                self.fixed.push((pos, val));
                true
            }
            None => {
                self.fixed.push((pos, val));
                self.dead_from = Some(self.fixed.len());
                false
            }
        }
    }

    /// Leap: find the smallest value >= `c` for the given variable position.
    pub fn leap(&mut self, var_pos: SpoPos, c: u32) -> Option<u32> {
        let (trie_i, beg, end) = self.child_block(var_pos)?;
        self.index
            .trie(trie_i)
            .seek(c, beg, end)
            .map(|(val, _)| val)
    }

    /// Descend one level: bind `var_pos` to `val`.
    pub fn down(&mut self, var_pos: SpoPos, val: u32) {
        self.descend(var_pos, val);
    }

    /// Ascend one level.
    pub fn up(&mut self, _var_pos: SpoPos) {
        self.fixed.pop();
        if self.dead_from.is_some_and(|d| self.fixed.len() < d) {
            self.dead_from = None;
        }
    }

    /// All values at the current level for the given variable position
    /// (trie children are distinct by construction).
    pub fn seek_all(&mut self, var_pos: SpoPos) -> Vec<u32> {
        let Some((trie_i, beg, end)) = self.child_block(var_pos) else {
            return Vec::new();
        };
        let trie = self.index.trie(trie_i);
        (beg..=end).map(|p| trie.sym(p)).collect()
    }

    /// Number of distinct children at the current level.
    pub fn children_count(&self, var_pos: SpoPos) -> usize {
        match self.child_block(var_pos) {
            Some((_, beg, end)) => end - beg + 1,
            None => 0,
        }
    }

    /// Whether all 3 positions are fixed (constants + variables).
    pub fn in_last_level(&self) -> bool {
        self.fixed.len() >= 2
    }

    /// Number of fixed variable positions (not counting constants).
    pub fn nfixed(&self) -> usize {
        self.fixed.len() - self.n_constants
    }

    /// First edge id at the bound (s, p, o); see the array counterpart.
    pub fn current_eid(&self) -> Option<u32> {
        self.current_eids().first().copied()
    }

    /// All edge ids at the bound (s, p, o); see the array counterpart.
    pub fn current_eids_all(&self) -> Vec<u32> {
        self.current_eids().to_vec()
    }

    fn current_eids(&self) -> &[u32] {
        let mut by_pos: [Option<u32>; 3] = [None; 3];
        for &(pos, val) in &self.fixed {
            by_pos[pos as usize] = Some(val);
        }
        match by_pos {
            [Some(s), Some(p), Some(o)] => self.index.eids_for(s, p, o),
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ltj::compact::CompactTripleIndex;

    fn pattern(s: Term, p: Term, o: Term) -> TriplePattern {
        TriplePattern { terms: [s, p, o] }
    }

    /// Drain one iterator's full (s, p, o, eid) match set by recursive
    /// leapfrog over its own variables — a single-triple LTJ.
    fn drain(it: &mut CompactLtjIterator, vars: &[SpoPos]) -> Vec<(Vec<u32>, Vec<u32>)> {
        fn rec(
            it: &mut CompactLtjIterator,
            vars: &[SpoPos],
            depth: usize,
            bound: &mut Vec<u32>,
            out: &mut Vec<(Vec<u32>, Vec<u32>)>,
        ) {
            if depth == vars.len() {
                out.push((bound.clone(), it.current_eids_all()));
                return;
            }
            let pos = vars[depth];
            let mut c = it.leap(pos, 0);
            while let Some(v) = c {
                bound.push(v);
                it.down(pos, v);
                rec(it, vars, depth + 1, bound, out);
                it.up(pos);
                bound.pop();
                c = if v == u32::MAX {
                    None
                } else {
                    it.leap(pos, v + 1)
                };
            }
        }
        let mut out = Vec::new();
        rec(it, vars, 0, &mut Vec::new(), &mut out);
        out
    }

    #[test]
    fn test_compact_iterator_single_triple() {
        // Graph: 1-[0]->2 (eid 100), 1-[0]->3 (eid 101), 2-[1]->3 (eid 102),
        // parallel 1-[0]->2 (eid 103).
        let raw = vec![
            (1, 0, 2, 100),
            (1, 0, 3, 101),
            (2, 1, 3, 102),
            (1, 0, 2, 103),
        ];
        let idx = CompactTripleIndex::from_raw(&raw);

        // (?s, ?p, ?o): all matches, S then P then O.
        let mut it = CompactLtjIterator::new(
            pattern(Term::Variable(0), Term::Variable(1), Term::Variable(2)),
            &idx,
        );
        let rows = drain(&mut it, &[SpoPos::S, SpoPos::P, SpoPos::O]);
        assert_eq!(
            rows,
            vec![
                (vec![1, 0, 2], vec![100, 103]),
                (vec![1, 0, 3], vec![101]),
                (vec![2, 1, 3], vec![102]),
            ]
        );

        // Same pattern, bound O-first then S then P (exercises pair-trie
        // handles: OSP/OPS).
        let mut it = CompactLtjIterator::new(
            pattern(Term::Variable(0), Term::Variable(1), Term::Variable(2)),
            &idx,
        );
        let rows = drain(&mut it, &[SpoPos::O, SpoPos::S, SpoPos::P]);
        assert_eq!(
            rows,
            vec![
                (vec![2, 1, 0], vec![100, 103]),
                (vec![3, 1, 0], vec![101]),
                (vec![3, 2, 1], vec![102]),
            ]
        );
    }

    #[test]
    fn test_compact_iterator_constants() {
        let raw = vec![(1, 0, 2, 100), (1, 0, 3, 101), (2, 1, 3, 102)];
        let idx = CompactTripleIndex::from_raw(&raw);

        // (1, 0, ?o)
        let mut it = CompactLtjIterator::new(
            pattern(Term::Constant(1), Term::Constant(0), Term::Variable(0)),
            &idx,
        );
        let rows = drain(&mut it, &[SpoPos::O]);
        assert_eq!(rows, vec![(vec![2], vec![100]), (vec![3], vec![101])]);

        // (?s, 1, ?o) — constant in the middle
        let mut it = CompactLtjIterator::new(
            pattern(Term::Variable(0), Term::Constant(1), Term::Variable(1)),
            &idx,
        );
        let rows = drain(&mut it, &[SpoPos::S, SpoPos::O]);
        assert_eq!(rows, vec![(vec![2, 3], vec![102])]);

        // Absent constant → empty
        let mut it = CompactLtjIterator::new(
            pattern(Term::Constant(9), Term::Variable(0), Term::Variable(1)),
            &idx,
        );
        assert_eq!(it.leap(SpoPos::P, 0), None);
        assert!(drain(&mut it, &[SpoPos::P, SpoPos::O]).is_empty());
    }

    #[test]
    fn test_compact_iterator_repeated_var_dead_level() {
        // 1→2 and 2→1 exist but no self-loop; a repeated variable (x, ?p, x)
        // reaches down(S, 1) then down(O, 1) — O=1 is not a child of S=1's
        // subtree, so the level goes dead: the P leap below it must return
        // None, and the state must revive on up().
        let raw = vec![(1, 0, 2, 100), (2, 0, 1, 101), (3, 0, 3, 102)];
        let idx = CompactTripleIndex::from_raw(&raw);
        let mut it = CompactLtjIterator::new(
            pattern(Term::Variable(0), Term::Variable(1), Term::Variable(0)),
            &idx,
        );

        // Simulate the algorithm binding x: intersect S and O levels.
        // x = 1: down(S,1), down(O,1) → dead; leap below → None.
        it.down(SpoPos::S, 1);
        it.down(SpoPos::O, 1);
        assert_eq!(it.leap(SpoPos::P, 0), None);
        assert_eq!(it.seek_all(SpoPos::P), Vec::<u32>::new());
        it.up(SpoPos::O);
        // Revived: S=1 alone has children again.
        assert_eq!(it.leap(SpoPos::O, 0), Some(2));
        it.up(SpoPos::S);

        // x = 3: genuine self-loop; P enumerates below it.
        it.down(SpoPos::S, 3);
        it.down(SpoPos::O, 3);
        assert_eq!(it.leap(SpoPos::P, 0), Some(0));
        it.down(SpoPos::P, 0);
        assert_eq!(it.current_eids_all(), vec![102]);
        it.up(SpoPos::P);
        it.up(SpoPos::O);
        it.up(SpoPos::S);
    }

    #[test]
    fn test_compact_iterator_seek_all_matches_leap() {
        let raw = vec![
            (1, 0, 2, 100),
            (1, 0, 3, 101),
            (1, 1, 2, 102),
            (2, 1, 3, 103),
        ];
        let idx = CompactTripleIndex::from_raw(&raw);
        let mut it = CompactLtjIterator::new(
            pattern(Term::Constant(1), Term::Variable(0), Term::Variable(1)),
            &idx,
        );
        assert_eq!(it.seek_all(SpoPos::P), vec![0, 1]);
        assert_eq!(it.children_count(SpoPos::P), 2);
        it.down(SpoPos::P, 0);
        assert_eq!(it.seek_all(SpoPos::O), vec![2, 3]);
        assert!(it.in_last_level());
    }
}
