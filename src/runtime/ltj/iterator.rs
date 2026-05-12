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

/// Iterator for navigating a trie ordering associated with one triple pattern.
///
/// Design: maintains a stack of (SpoPos, bound_value) representing the descent path.
/// On each leap/down/up, recomputes the trie ordering and range from scratch.
/// This is simple and correct; optimization can come later.
pub struct LtjIterator<'a> {
    index: &'a TripleIndex,
    /// Constants from the triple pattern: always at the bottom of the effective stack
    constants: Vec<(SpoPos, u32)>,
    /// Stack of variable bindings (pushed by down(), popped by up())
    stack: Vec<(SpoPos, u32)>,
}

impl<'a> LtjIterator<'a> {
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
        LtjIterator {
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
        let nfixed = eff.len();
        match nfixed {
            0 => match querying {
                SpoPos::S => TrieOrder::SPO,
                SpoPos::P => TrieOrder::POS,
                SpoPos::O => TrieOrder::OSP,
            },
            1 => {
                let f0 = eff[0].0;
                match (f0, querying) {
                    (SpoPos::S, SpoPos::P) => TrieOrder::SPO,
                    (SpoPos::S, SpoPos::O) => TrieOrder::SOP,
                    (SpoPos::P, SpoPos::O) => TrieOrder::POS,
                    (SpoPos::P, SpoPos::S) => TrieOrder::PSO,
                    (SpoPos::O, SpoPos::S) => TrieOrder::OSP,
                    (SpoPos::O, SpoPos::P) => TrieOrder::OPS,
                    _ => unreachable!("fixed={:?} querying={:?}", f0, querying),
                }
            }
            2 => {
                let (f0, f1) = (eff[0].0, eff[1].0);
                match (f0, f1) {
                    (SpoPos::S, SpoPos::P) => TrieOrder::SPO,
                    (SpoPos::S, SpoPos::O) => TrieOrder::SOP,
                    (SpoPos::P, SpoPos::O) => TrieOrder::POS,
                    (SpoPos::P, SpoPos::S) => TrieOrder::PSO,
                    (SpoPos::O, SpoPos::S) => TrieOrder::OSP,
                    (SpoPos::O, SpoPos::P) => TrieOrder::OPS,
                    _ => unreachable!("f0={:?} f1={:?}", f0, f1),
                }
            }
            _ => unreachable!("nfixed={}", nfixed),
        }
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
