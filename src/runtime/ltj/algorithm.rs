use crate::model::graph_access::GraphAccess;
use crate::model::value::Value;
use crate::runtime::cmp_values;
use crate::syntax::expr::BinOp;

use super::iterator::{LtjIterator, SpoPos};
use super::veo::Veo;
use crate::runtime::vsearch::topk::DistThreshold;
use crate::vector::cursor::NnStream;

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
    /// Distance from the vector-search query to this tuple's binding of
    /// the search variable. `None` for every ordinary query. Carried here
    /// because the search knows it and the row does not yet exist — rows
    /// are built from tuples only after the search returns.
    pub dist: Option<f32>,
}

impl ResultTuple {
    pub fn new(vars: Vec<(u8, u32)>, triple_eids: Vec<u32>) -> Self {
        ResultTuple {
            vars,
            triple_eids,
            dist: None,
        }
    }
}

/// Live state for a vector-search level, threaded through the recursion.
///
/// It is a parameter rather than a field of `LtjAlgorithm` on purpose:
/// `search` is `&mut self` and recurses on `self`, so a `&mut` borrow of
/// a field held across the recursive call is an aliasing error, and a
/// `RefCell` would turn that compile error into a runtime panic.
pub struct VecCtx<'v> {
    /// Neighbours, nearest first, shared across every visit to the
    /// level. Unused when `local` is set — a local ranking has nothing
    /// to share between visits, since each visit ranks its own set.
    pub stream: NnStream<'v>,
    /// Rank only the current visit's candidates instead of walking a
    /// corpus-wide stream. This is the axis that separates the two
    /// exact sources: `false` re-scans a global ranking on every visit,
    /// `true` never looks at a node outside the level.
    pub local: bool,
    /// The attribute and query vector, needed to rank locally.
    pub set: &'v crate::vector::store::VectorSet,
    pub q: &'v [f32],
    /// The running top-k threshold, updated as matches are accepted.
    pub cut: DistThreshold,
    /// Slack on the threshold: an approximate cursor's order is only
    /// approximately sorted, so an exact cut can drop a neighbour the
    /// stream was about to reveal.
    pub tau_eps: f32,
    /// The candidate currently being descended into.
    pub cur_id: u32,
    pub cur_dist: f32,
    /// Instrumentation.
    pub visits: u64,
    pub candidates_hashed: u64,
    pub nn_pops: u64,
}

impl<'v> VecCtx<'v> {
    pub fn new(
        stream: NnStream<'v>,
        cut: DistThreshold,
        tau_eps: f32,
        local: bool,
        set: &'v crate::vector::store::VectorSet,
        q: &'v [f32],
    ) -> VecCtx<'v> {
        VecCtx {
            stream,
            local,
            set,
            q,
            cut,
            tau_eps,
            cur_id: 0,
            cur_dist: 0.0,
            visits: 0,
            candidates_hashed: 0,
            nn_pops: 0,
        }
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
    /// VEO level at which the vector-search variable binds, and the
    /// variable itself. `None` for every ordinary query.
    nn_level: Option<(usize, u8)>,
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
    ) -> Self {
        LtjAlgorithm {
            iterators,
            var_to_iterators,
            var_to_positions,
            veo,
            filters_at_level,
            num_vars,
            pinned,
            nn_level: None,
        }
    }

    /// Bind `var` to the neighbour stream at VEO level `level`.
    pub fn with_nn_level(mut self, level: usize, var: u8) -> Self {
        self.nn_level = Some((level, var));
        self
    }

    /// Run the LTJ algorithm against `graph`. Returns result tuples.
    ///
    /// `G` is a method-level generic on purpose: the search state above
    /// (iterators, VEO, placed filters, pins) never mentions the backend,
    /// so parameterizing the struct would spread `G` across every field
    /// for the sole benefit of `check_filters`. Taking the graph as an
    /// argument keeps the state backend-agnostic without the wrapper type
    /// this used to need — `LtjRunner`, whose only job was to hold this
    /// reference, and which carried a second copy of `search` that drifted
    /// out of sync (see the pinned-slot comment below).
    pub fn run<G: GraphAccess>(&mut self, graph: &G, limit: usize) -> Vec<ResultTuple> {
        let mut results = Vec::new();
        let mut tuple = vec![(0u8, 0u32); self.num_vars];
        // Pre-populate pinned bindings into the slots beyond the VEO so
        // result construction sees them. The search loop only writes to
        // tuple[0..veo.size()]. Omitting this leaves index-folded variables
        // at the default `(0, 0)` and produces wrong bindings; it surfaces
        // through the BTree-LTJ-real ORDER BY path. The bug was live for as
        // long as there were two copies of this function — the dead one had
        // the loop, the executed one did not.
        for (i, &binding) in self.pinned.iter().enumerate() {
            let slot = self.veo.size() + i;
            if slot < tuple.len() {
                tuple[slot] = binding;
            }
        }
        self.search(graph, 0, &mut tuple, &mut results, limit, None);
        results
    }

    /// Run with the vector-search level active. Every returned tuple
    /// carries the distance of its search-variable binding.
    pub fn run_nearest<G: GraphAccess>(
        &mut self,
        graph: &G,
        ctx: &mut VecCtx<'_>,
    ) -> Vec<ResultTuple> {
        let mut results = Vec::new();
        let mut tuple = vec![(0u8, 0u32); self.num_vars];
        for (i, &binding) in self.pinned.iter().enumerate() {
            let slot = self.veo.size() + i;
            if slot < tuple.len() {
                tuple[slot] = binding;
            }
        }
        // No limit: an arrival-order cut has nothing to do with distance.
        // The threshold in `ctx.cut` is what bounds the work instead.
        self.search(graph, 0, &mut tuple, &mut results, 0, Some(ctx));
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

    /// Every value the leapfrog would produce for `var_id` under the
    /// current partial binding, materialised.
    ///
    /// The ordinary search consumes candidates one at a time and in
    /// ascending order; the vector-search level needs the whole set up
    /// front so it can visit them in distance order instead. That is
    /// legal because `leap` is a pure query against state only
    /// `down`/`up` mutate: draining the candidates leaves the iterators
    /// exactly as it found them, and they may then be descended into in
    /// any order.
    fn collect_candidates(&mut self, var_id: u8) -> Vec<u32> {
        let iters = &self.var_to_iterators[var_id as usize];
        if iters.len() == 1 && self.iterators[iters[0]].in_last_level() {
            let idx = iters[0];
            let pos = self.var_to_positions[var_id as usize][0];
            return self.iterators[idx].seek_all(pos);
        }
        let mut out = Vec::new();
        let mut c = self.seek(var_id, None);
        while let Some(v) = c {
            out.push(v);
            // The live loop below advances with a bare `val + 1`;
            // u32::MAX is a real sentinel in this codebase (an unknown
            // label folds to it), so guard the wrap here.
            c = match v.checked_add(1) {
                Some(next) => self.seek(var_id, Some(next)),
                None => None,
            };
        }
        out
    }

    /// Recursive search: bind the variable at level `j`, evaluate the
    /// filters placed there, descend, backtrack.
    fn search<G: GraphAccess>(
        &mut self,
        graph: &G,
        j: usize,
        tuple: &mut Vec<(u8, u32)>,
        results: &mut Vec<ResultTuple>,
        limit: usize,
        mut ctx: Option<&mut VecCtx<'_>>,
    ) -> bool {
        if limit > 0 && results.len() >= limit {
            return false;
        }

        // Base case: all variables bound. Every triple fans out one tuple
        // per parallel entry sharing the bound (s,p,o), whether or not the
        // edge variable is projected — GQL binding tables are bags, so two
        // parallel edges with the same (src, label, tgt) are two matches
        // (issue #71). The cartesian product across triples preserves
        // multi-triple joins — typically all but one factor is a singleton,
        // so this stays O(rows) in practice. Users who want set semantics
        // ask for `RETURN DISTINCT`.
        if j >= self.veo.size() {
            // Every triple is fully bound here, so an iterator with no
            // edge id means its triple does not exist and this tuple is
            // not a match.
            //
            // The ordinary search can never see that: the leapfrog only
            // descends into values the index actually holds, so by the
            // base case every triple has at least one entry. It happens
            // when *all* the variables were fixed before the search — the
            // secondary-index constant fold plus caller-supplied pins —
            // which bypasses the leapfrog entirely and leaves nothing to
            // validate the combination. Emitting a row there invented an
            // edge: `(a)-[:follows]->(b) WHERE a.id = 0 AND b.id = 3`
            // returned a match whether or not that edge existed.
            let mut eid_lists: Vec<Vec<u32>> = Vec::with_capacity(self.iterators.len());
            for it in self.iterators.iter() {
                let eids = it.current_eids_all();
                if eids.is_empty() {
                    return true;
                }
                eid_lists.push(eids);
            }
            let var_bindings = tuple[..self.num_vars].to_vec();
            let dist = ctx.as_ref().map(|c| c.cur_dist);
            for combo in cartesian(&eid_lists) {
                let mut rt = ResultTuple::new(var_bindings.clone(), combo);
                rt.dist = dist;
                results.push(rt);
                if limit > 0 && results.len() >= limit {
                    return false;
                }
            }
            // Tighten the threshold as soon as a match exists, so the
            // sibling candidates of this very visit are already pruned
            // against it. Reading `cut` after the descent instead would
            // waste the whole subtree's worth of narrowing.
            if let Some(c) = ctx {
                let (id, d) = (c.cur_id, c.cur_dist);
                c.cut.accept(id, d);
            }
            return true;
        }

        let var_id = self.veo.var_at(j);
        let iter_indices: Vec<usize> = self.var_to_iterators[var_id as usize].clone();
        let positions: Vec<SpoPos> = self.var_to_positions[var_id as usize].clone();

        // Vector-search level: instead of enumerating this variable's
        // candidates in index order, enumerate them nearest-first.
        //
        // The candidate set is already conditioned on the partial binding
        // of everything above, so it is normally small; hashing it and
        // walking the shared neighbour stream costs a membership test per
        // neighbour and no distance computation at all.
        //
        // The threshold cut is what makes this cheap. Within one visit
        // the stream is non-decreasing, so once a neighbour is past the
        // running top-k threshold every later one is too, and the visit
        // stops. It is re-read on every iteration rather than hoisted:
        // the recursive descent between two iterations can accept matches
        // and tighten it.
        if self.nn_level == Some((j, var_id)) {
            if let Some(vc) = ctx {
                vc.visits += 1;
                let candidates = self.collect_candidates(var_id);
                if candidates.is_empty() {
                    return true;
                }
                vc.candidates_hashed += candidates.len() as u64;

                // Two ways to visit the candidates nearest-first:
                //
                // - local: rank just this visit's set. Every element is
                //   a candidate, so no membership test is needed and
                //   nothing outside the level is ever touched.
                // - global: walk the corpus-wide stream from the top and
                //   test membership. Costs however deep the threshold
                //   lets the scan run, which has nothing to do with how
                //   many candidates this visit has.
                let ranked: Option<Vec<(u32, f32)>> = if vc.local {
                    Some(vc.set.rank_candidates(vc.q, &candidates))
                } else {
                    None
                };
                let hits: Option<std::collections::HashSet<u32>> = if vc.local {
                    None
                } else {
                    Some(candidates.into_iter().collect())
                };

                let mut rank = 0usize;
                loop {
                    let tau = vc.cut.get();
                    let next = match &ranked {
                        Some(v) => v.get(rank).copied(),
                        None => vc.stream.at(rank),
                    };
                    let Some((id, dist)) = next else {
                        break;
                    };
                    rank += 1;
                    vc.nn_pops += 1;
                    if tau.is_finite() && dist > tau * (1.0 + vc.tau_eps) {
                        break;
                    }
                    if let Some(h) = &hits {
                        if !h.contains(&id) {
                            continue;
                        }
                    }

                    tuple[j] = (var_id, id);
                    if !self.check_filters(graph, j, tuple) {
                        continue;
                    }
                    vc.cur_id = id;
                    vc.cur_dist = dist;

                    for k in 0..iter_indices.len() {
                        self.iterators[iter_indices[k]].down(positions[k], id);
                    }
                    let ok = self.search(graph, j + 1, tuple, results, limit, Some(vc));
                    for k in 0..iter_indices.len() {
                        self.iterators[iter_indices[k]].up(positions[k]);
                    }
                    if !ok {
                        return false;
                    }
                }
                return true;
            }
        }

        // Optimization: lonely variable at last level — use seek_all
        if iter_indices.len() == 1 && self.iterators[iter_indices[0]].in_last_level() {
            let idx = iter_indices[0];
            let pos = positions[0];
            let values = self.iterators[idx].seek_all(pos);

            for val in values {
                tuple[j] = (var_id, val);

                if !self.check_filters(graph, j, tuple) {
                    continue;
                }

                self.iterators[idx].down(pos, val);
                let ok = self.search(graph, j + 1, tuple, results, limit, ctx.as_deref_mut());
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

            if self.check_filters(graph, j, tuple) {
                // Descend in all iterators containing this variable
                for k in 0..iter_indices.len() {
                    self.iterators[iter_indices[k]].down(positions[k], val);
                }

                let ok = self.search(graph, j + 1, tuple, results, limit, ctx.as_deref_mut());

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

    /// Evaluate the filters placed at level `j` against the graph. Called
    /// after binding the level's variable and before descending, so a
    /// rejected candidate prunes its whole subtree.
    fn check_filters<G: GraphAccess>(&self, graph: &G, j: usize, tuple: &[(u8, u32)]) -> bool {
        if j >= self.filters_at_level.len() {
            return true;
        }
        for filter in &self.filters_at_level[j] {
            match &filter.kind {
                FilterKind::NodeLabel { var_id, label } => {
                    // Find the bound value for this variable
                    if let Some(&(_, node_id)) = tuple.iter().find(|(v, _)| *v == *var_id) {
                        let actual = graph.node_labels(node_id);
                        let required = crate::typing::label_type::LabelType::Label(label.clone());
                        if !crate::typing::label_type::LabelType::is_subtype(&actual, &required) {
                            return false;
                        }
                    }
                }
                FilterKind::NodeProperty { var_id, prop } => {
                    if let Some(&(_, node_id)) = tuple.iter().find(|(v, _)| *v == *var_id) {
                        let props = graph.node_props(node_id);
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
                        let props = graph.node_props(node_id);
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
