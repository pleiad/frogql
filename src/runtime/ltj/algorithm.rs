use crate::model::graph_access::GraphAccess;
use crate::model::value::Value;
use crate::runtime::cmp_values;
use crate::syntax::expr::BinOp;

use super::iterator::{LtjIterator, SpoPos};
use super::veo::Veo;

/// Iterate every combination across `lists`. Empty `lists` yields one
/// empty combo; a single empty inner vector kills the product. Used at
/// the LTJ base case to fan out parallel-edge entries per triple.
fn cartesian(lists: &[Vec<u32>]) -> impl Iterator<Item = Vec<u32>> + '_ {
    let lens: Vec<usize> = lists.iter().map(|l| l.len()).collect();
    let total: usize = if lens.is_empty() {
        1
    } else if lens.contains(&0) {
        0
    } else {
        lens.iter().product()
    };
    (0..total).map(move |idx| {
        let mut out = Vec::with_capacity(lists.len());
        let mut k = idx;
        for (list, &len) in lists.iter().zip(lens.iter()) {
            let pick = k % len;
            k /= len;
            out.push(list[pick]);
        }
        out
    })
}

/// A filter placed at a specific level in the VEO.
/// Evaluated after binding the variable at that level, before descending further.
#[derive(Clone)]
pub struct PlacedFilter {
    /// Level in the VEO where this filter should be evaluated
    /// (= index of the last variable it depends on)
    pub eval_at_level: usize,
    /// The filter to evaluate
    pub kind: FilterKind,
}

#[derive(Clone)]
pub enum FilterKind {
    /// Node label: check that the node has a specific label
    NodeLabel { var_id: u8, label: String },
    /// Node property: check that the node has a property of a given type
    NodeProperty { var_id: u8, prop: String },
    /// Node attribute compared against a literal: `var.attr <op> value`.
    /// Pushed down from WHERE conjuncts by the optimizer; evaluated at the
    /// VEO level where `var_id` is bound, before descending further.
    NodeAttrCmp {
        var_id: u8,
        attr: String,
        op: BinOp,
        value: Value,
    },
    /// Node membership in a precomputed set. Built by the optimizer from a
    /// BTree secondary-index range query: the original `var.attr <op> literal`
    /// predicate is replaced by membership in the set of node IDs the btree
    /// returned, which is O(log N) per candidate and avoids reading the
    /// node's property from the page cache. The set is sorted ascending.
    NodeInSet {
        var_id: u8,
        /// Sorted ascending so membership is a binary search.
        set: std::sync::Arc<Vec<u32>>,
    },
}

/// A result tuple: variable bindings as (var_id, value), plus the source
/// edge id per triple. `triple_eids[i]` is the edge id of the index entry
/// that produced the binding for triple `i`. Replaces the older
/// `find_edge(src, tgt)` reconstruction, which ignored the bound label
/// and aliased parallel edges of different labels onto a single eid.
#[derive(Debug, Clone, Default)]
pub struct ResultTuple {
    pub vars: Vec<(u8, u32)>,
    pub triple_eids: Vec<u32>,
}

impl ResultTuple {
    pub fn new(vars: Vec<(u8, u32)>, triple_eids: Vec<u32>) -> Self {
        ResultTuple { vars, triple_eids }
    }
}

/// Main LTJ algorithm.
/// Holds iterators, variable-to-iterator mapping, VEO, and placed filters.
pub struct LtjAlgorithm<'a> {
    iterators: Vec<LtjIterator<'a>>,
    /// var_id → indices into `iterators` that contain this variable
    var_to_iterators: Vec<Vec<usize>>,
    /// var_id → the SpoPos of this variable in each iterator
    var_to_positions: Vec<Vec<SpoPos>>,
    /// Variable ordering. Excludes any variables that were pinned to a
    /// constant by index-resolved equality before search; those don't need
    /// to be bound by leapfrog because their NodeId is already known.
    veo: Box<dyn Veo>,
    /// Filters indexed by VEO level
    filters_at_level: Vec<Vec<PlacedFilter>>,
    /// Number of variables (including pinned ones).
    num_vars: usize,
    /// (var_id, node_id) bindings resolved by secondary-index lookup before
    /// search starts. These slots are pre-populated in the result tuple and
    /// never written to by the search loop.
    pinned: Vec<(u8, u32)>,
    /// Parallel to `iterators`: `true` when the triple binds an edge
    /// variable, so the base case has to fan out one tuple per physical
    /// parallel edge (same src/label/tgt, distinct eid). When `false`,
    /// the canonical first eid is enough — the LTJ trie collapses the
    /// parallel siblings, which is the right behavior for joins on
    /// vertex variables but loses them when the user binds the edge.
    triple_has_edge_var: Vec<bool>,
}

impl<'a> LtjAlgorithm<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        iterators: Vec<LtjIterator<'a>>,
        var_to_iterators: Vec<Vec<usize>>,
        var_to_positions: Vec<Vec<SpoPos>>,
        veo: Box<dyn Veo>,
        filters_at_level: Vec<Vec<PlacedFilter>>,
        num_vars: usize,
        pinned: Vec<(u8, u32)>,
        triple_has_edge_var: Vec<bool>,
    ) -> Self {
        debug_assert_eq!(triple_has_edge_var.len(), iterators.len());
        LtjAlgorithm {
            iterators,
            var_to_iterators,
            var_to_positions,
            veo,
            filters_at_level,
            num_vars,
            pinned,
            triple_has_edge_var,
        }
    }

    /// Run the LTJ algorithm. Returns result tuples.
    pub fn run(&mut self, limit: usize) -> Vec<ResultTuple> {
        let mut results = Vec::new();
        let mut tuple = vec![(0u8, 0u32); self.num_vars];
        // Pre-populate pinned bindings into the slots beyond the VEO so
        // result construction sees them. The search loop only writes to
        // tuple[0..veo.size()].
        for (i, &binding) in self.pinned.iter().enumerate() {
            let slot = self.veo.size() + i;
            if slot < tuple.len() {
                tuple[slot] = binding;
            }
        }
        self.search(0, &mut tuple, &mut results, limit);
        results
    }

    /// Leapfrog seek: intersect across all iterators containing `var_id`.
    /// Finds the smallest value that all iterators agree on (>= `c`).
    fn seek(&mut self, var_id: u8, c: Option<u32>) -> Option<u32> {
        let iter_indices = &self.var_to_iterators[var_id as usize];
        let positions = &self.var_to_positions[var_id as usize];

        if iter_indices.is_empty() {
            return None;
        }

        // Single iterator — just leap directly
        if iter_indices.len() == 1 {
            let idx = iter_indices[0];
            let pos = positions[0];
            return match c {
                Some(cv) => self.iterators[idx].leap(pos, cv),
                None => self.iterators[idx].leap(pos, 0),
            };
        }

        // Multiple iterators — leapfrog intersection
        let n = iter_indices.len();
        let mut current_c = c.unwrap_or(0);
        let mut n_ok = 0usize;
        let mut i = 0;
        let mut prev_val: Option<u32> = None;

        loop {
            let idx = iter_indices[i];
            let pos = positions[i];
            let val = self.iterators[idx].leap(pos, current_c)?;

            if Some(val) == prev_val {
                n_ok += 1;
            } else {
                n_ok = 1;
            }

            if n_ok >= n {
                return Some(val);
            }

            current_c = val;
            prev_val = Some(val);
            i = (i + 1) % n;
        }
    }

    /// Recursive search: bind variable at level `j`, evaluate filters, recurse.
    fn search(
        &mut self,
        j: usize,
        tuple: &mut Vec<(u8, u32)>,
        results: &mut Vec<ResultTuple>,
        limit: usize,
    ) -> bool {
        // Check limit
        if limit > 0 && results.len() >= limit {
            return false;
        }

        // Base case: all variables bound. Triples with an edge variable
        // fan out one tuple per parallel entry sharing the bound (s,p,o);
        // others contribute a single canonical eid. The cartesian product
        // across triples preserves multi-triple joins — typically all but
        // one factor is a singleton, so this stays O(rows) in practice.
        if j >= self.veo.size() {
            let eid_lists: Vec<Vec<u32>> = self
                .iterators
                .iter()
                .enumerate()
                .map(|(i, it)| {
                    if self.triple_has_edge_var[i] {
                        let eids = it.current_eids_all();
                        if eids.is_empty() {
                            vec![u32::MAX]
                        } else {
                            eids
                        }
                    } else {
                        vec![it.current_eid().unwrap_or(u32::MAX)]
                    }
                })
                .collect();
            let var_bindings = tuple[..self.num_vars].to_vec();
            let mut accepted_any = false;
            for combo in cartesian(&eid_lists) {
                accepted_any = true;
                results.push(ResultTuple::new(var_bindings.clone(), combo));
                if limit > 0 && results.len() >= limit {
                    return false;
                }
            }
            // Defensive: when no iterator could produce a triple_eid at
            // all (degenerate empty index), still emit one row so callers
            // see the variable bindings.
            if !accepted_any {
                results.push(ResultTuple::new(var_bindings, Vec::new()));
            }
            return true;
        }

        let var_id = self.veo.var_at(j);
        let iter_indices: Vec<usize> = self.var_to_iterators[var_id as usize].clone();
        let positions: Vec<SpoPos> = self.var_to_positions[var_id as usize].clone();

        // Optimization: lonely variable at last level — use seek_all
        if iter_indices.len() == 1 && self.iterators[iter_indices[0]].in_last_level() {
            let idx = iter_indices[0];
            let pos = positions[0];
            let values = self.iterators[idx].seek_all(pos);

            for val in values {
                tuple[j] = (var_id, val);

                // Evaluate filters at this level
                if !self.check_filters(j, tuple) {
                    continue;
                }

                // Descend
                self.iterators[idx].down(pos, val);
                let ok = self.search(j + 1, tuple, results, limit);
                self.iterators[idx].up(pos);

                if !ok {
                    return false;
                }
            }
            return true;
        }

        // General case: leapfrog intersection
        let mut c = self.seek(var_id, None);

        while let Some(val) = c {
            tuple[j] = (var_id, val);

            // Evaluate filters at this level
            if self.check_filters(j, tuple) {
                // Descend in all iterators containing this variable
                for k in 0..iter_indices.len() {
                    self.iterators[iter_indices[k]].down(positions[k], val);
                }

                let ok = self.search(j + 1, tuple, results, limit);

                // Ascend in all iterators
                for k in 0..iter_indices.len() {
                    self.iterators[iter_indices[k]].up(positions[k]);
                }

                if !ok {
                    return false;
                }
            }

            c = self.seek(var_id, Some(val + 1));
        }

        true
    }

    /// Evaluate all filters placed at level `j`.
    fn check_filters(&self, j: usize, _tuple: &[(u8, u32)]) -> bool {
        if j >= self.filters_at_level.len() {
            return true;
        }
        true
    }
}

/// Extended algorithm that can evaluate filters against the graph.
pub struct LtjRunner<'a, G: GraphAccess> {
    pub algorithm: LtjAlgorithm<'a>,
    pub graph: &'a G,
}

impl<'a, G: GraphAccess> LtjRunner<'a, G> {
    pub fn new(algorithm: LtjAlgorithm<'a>, graph: &'a G) -> Self {
        LtjRunner { algorithm, graph }
    }

    /// Run with filter evaluation against the graph.
    pub fn run(&mut self, limit: usize) -> Vec<ResultTuple> {
        let mut results = Vec::new();
        let mut tuple = vec![(0u8, 0u32); self.algorithm.num_vars];
        // Pre-populate pinned bindings into the slots beyond the VEO so
        // result construction sees them. The search loop only writes to
        // tuple[0..veo.size()]. This was previously missing in
        // `LtjRunner::run` (the dead-code `LtjAlgorithm::run` had it),
        // which left pinned-var slots at the default `(0, 0)` and
        // produced wrong bindings for index-folded variables. Surfaces
        // through the BTree-LTJ-real ORDER BY path.
        for (i, &binding) in self.algorithm.pinned.iter().enumerate() {
            let slot = self.algorithm.veo.size() + i;
            if slot < tuple.len() {
                tuple[slot] = binding;
            }
        }
        self.search(0, &mut tuple, &mut results, limit);
        results
    }

    fn search(
        &mut self,
        j: usize,
        tuple: &mut Vec<(u8, u32)>,
        results: &mut Vec<ResultTuple>,
        limit: usize,
    ) -> bool {
        if limit > 0 && results.len() >= limit {
            return false;
        }

        if j >= self.algorithm.veo.size() {
            // Same fan-out logic as `LtjAlgorithm::search`: a triple that
            // binds an edge variable emits one row per parallel entry
            // sharing the (s, p, o) prefix, others contribute a single
            // canonical eid. The product accommodates joins where one
            // triple has parallels and the others do not.
            let eid_lists: Vec<Vec<u32>> = self
                .algorithm
                .iterators
                .iter()
                .enumerate()
                .map(|(i, it)| {
                    if self.algorithm.triple_has_edge_var[i] {
                        let eids = it.current_eids_all();
                        if eids.is_empty() {
                            vec![u32::MAX]
                        } else {
                            eids
                        }
                    } else {
                        vec![it.current_eid().unwrap_or(u32::MAX)]
                    }
                })
                .collect();
            let var_bindings = tuple[..self.algorithm.num_vars].to_vec();
            let mut accepted_any = false;
            for combo in cartesian(&eid_lists) {
                accepted_any = true;
                results.push(ResultTuple::new(var_bindings.clone(), combo));
                if limit > 0 && results.len() >= limit {
                    return false;
                }
            }
            if !accepted_any {
                results.push(ResultTuple::new(var_bindings, Vec::new()));
            }
            return true;
        }

        let var_id = self.algorithm.veo.var_at(j);
        let iter_indices: Vec<usize> = self.algorithm.var_to_iterators[var_id as usize].clone();
        let positions: Vec<SpoPos> = self.algorithm.var_to_positions[var_id as usize].clone();

        // Lonely variable optimization
        if iter_indices.len() == 1 && self.algorithm.iterators[iter_indices[0]].in_last_level() {
            let idx = iter_indices[0];
            let pos = positions[0];
            let values = self.algorithm.iterators[idx].seek_all(pos);

            for val in values {
                tuple[j] = (var_id, val);
                if !self.check_filters(j, tuple) {
                    continue;
                }
                self.algorithm.iterators[idx].down(pos, val);
                let ok = self.search(j + 1, tuple, results, limit);
                self.algorithm.iterators[idx].up(pos);
                if !ok {
                    return false;
                }
            }
            return true;
        }

        // General leapfrog
        let mut c = self.algorithm.seek(var_id, None);

        while let Some(val) = c {
            tuple[j] = (var_id, val);

            if self.check_filters(j, tuple) {
                for k in 0..iter_indices.len() {
                    self.algorithm.iterators[iter_indices[k]].down(positions[k], val);
                }
                let ok = self.search(j + 1, tuple, results, limit);
                for k in 0..iter_indices.len() {
                    self.algorithm.iterators[iter_indices[k]].up(positions[k]);
                }
                if !ok {
                    return false;
                }
            }

            c = self.algorithm.seek(var_id, Some(val + 1));
        }

        true
    }

    /// Evaluate filters at level `j` using the graph.
    fn check_filters(&self, j: usize, tuple: &[(u8, u32)]) -> bool {
        if j >= self.algorithm.filters_at_level.len() {
            return true;
        }
        for filter in &self.algorithm.filters_at_level[j] {
            match &filter.kind {
                FilterKind::NodeLabel { var_id, label } => {
                    // Find the bound value for this variable
                    if let Some(&(_, node_id)) = tuple.iter().find(|(v, _)| *v == *var_id) {
                        let actual = self.graph.node_labels(node_id);
                        let required = crate::typing::label_type::LabelType::Label(label.clone());
                        if !crate::typing::label_type::LabelType::is_subtype(&actual, &required) {
                            return false;
                        }
                    }
                }
                FilterKind::NodeProperty { var_id, prop } => {
                    if let Some(&(_, node_id)) = tuple.iter().find(|(v, _)| *v == *var_id) {
                        let props = self.graph.node_props(node_id);
                        if !props.contains_key(prop) {
                            return false;
                        }
                    }
                }
                FilterKind::NodeAttrCmp {
                    var_id,
                    attr,
                    op,
                    value,
                } => {
                    if let Some(&(_, node_id)) = tuple.iter().find(|(v, _)| *v == *var_id) {
                        let props = self.graph.node_props(node_id);
                        match props.get(attr) {
                            Some(actual) => {
                                if !cmp_values(actual, *op, value) {
                                    return false;
                                }
                            }
                            // Missing property → predicate is null → reject
                            None => return false,
                        }
                    }
                }
                FilterKind::NodeInSet { var_id, set } => {
                    if let Some(&(_, node_id)) = tuple.iter().find(|(v, _)| *v == *var_id) {
                        if set.binary_search(&node_id).is_err() {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }
}
