use std::collections::HashMap;

use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, Path, PathValue};
use crate::runtime::assignment::Assignment;
use crate::runtime::result::{IntermediateResult, ResultRow};
use crate::syntax::descriptor::Descriptor;
use crate::syntax::expr::BinOp;
use crate::syntax::path_pattern::PathPattern;

use super::algorithm::{FilterKind, LtjAlgorithm, LtjRunner, PlacedFilter, ResultTuple};
use super::iterator::{LtjIterator, SpoPos, Term, TriplePattern};
use super::triple_index::TripleIndex;
use super::veo::{Veo, VeoSimple};

// ---- Decomposition result ----

struct Decomposition {
    triples: Vec<TriplePattern>,
    var_id_to_name: Vec<String>,
    filters: Vec<ExtractedFilter>,
    /// Variables that are internal (anonymous) — excluded from final assignment
    internal_vars: Vec<u8>,
    /// For each triple: (src_var, tgt_var, edge_var_name, kind).
    /// `src_var` and `tgt_var` reflect the order of the *triple*, after the
    /// leftward swap; `kind` lets result reconstruction recover the
    /// original pattern direction for path display and edge lookup.
    triple_info: Vec<(u8, u8, Option<String>, EdgeKind)>,
    /// Join boundaries: triple index ranges for each sub-query in a Join.
    /// E.g., for `Q1, Q2` with 3 triples from Q1 and 2 from Q2: [(0,3), (3,5)].
    /// Empty means the whole pattern is a single path.
    join_boundaries: Vec<(usize, usize)>,
}

#[derive(Clone)]
struct ExtractedFilter {
    depends_on: Vec<u8>,
    kind: FilterKind,
}

// ---- Flat element of a concat chain ----

/// Edge direction at the pattern level. LTJ extracts a forward triple
/// `(src, label, tgt)` for every edge regardless of direction; the
/// `EdgeKind` lets the decomposition know whether to swap endpoints
/// (Left) and lets result reconstruction pick the right `PathValue`
/// variant (`EdgeUndirectional` for Undirected).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeKind {
    Right,
    Left,
    Undirected,
}

#[derive(Debug)]
enum FlatElement<'a> {
    Node(Option<&'a Descriptor>),
    Edge(EdgeKind, Option<&'a Descriptor>),
}

// ---- Main entry point ----

/// Try to run LTJ on a PathPattern. Returns Some(result) if decomposable.
pub fn try_ltj<G: GraphAccess>(
    graph: &G,
    pattern: &PathPattern,
    index: &TripleIndex,
    limit: usize,
) -> Option<IntermediateResult> {
    try_ltj_inner(graph, pattern, index, limit, &[])
}

/// Like `try_ltj` but with an externally supplied variable pin: `pin_var`
/// is forced to bind to `pin_id` before the LTJ runs, exactly as the
/// internal index-resolved pinning works. Used by the BTree-LTJ-real
/// ORDER BY path: the engine drives the sort variable through the btree
/// in key order, calling this once per (value, id) pair so the LTJ
/// itself never enumerates that variable's domain. The named variable
/// must exist in the pattern; if not, the pin is silently ignored
/// (caller can detect via `var_id_to_name` reflection if needed).
pub fn try_ltj_with_pin<G: GraphAccess>(
    graph: &G,
    pattern: &PathPattern,
    index: &TripleIndex,
    limit: usize,
    pin_var: &str,
    pin_id: u32,
) -> Option<IntermediateResult> {
    try_ltj_inner(graph, pattern, index, limit, &[(pin_var, pin_id)])
}

/// Multi-variable extension of `try_ltj_with_pin`. Each `(name, id)` pair
/// pre-fixes the named variable in the pattern's decomposition before the
/// LTJ runs — equivalent to substituting `Term::Variable(v)` with
/// `Term::Constant(id)` in every triple position. Used by the OPTIONAL
/// MATCH bind-pushdown path: the engine correlates the inner pattern with
/// each outer-row's shared-var bindings, avoiding a global pattern
/// enumeration and the subsequent left-outer hash-join.
pub fn try_ltj_with_pins<G: GraphAccess>(
    graph: &G,
    pattern: &PathPattern,
    index: &TripleIndex,
    limit: usize,
    pins: &[(&str, u32)],
) -> Option<IntermediateResult> {
    try_ltj_inner(graph, pattern, index, limit, pins)
}

fn try_ltj_inner<G: GraphAccess>(
    graph: &G,
    pattern: &PathPattern,
    index: &TripleIndex,
    limit: usize,
    external_pins: &[(&str, u32)],
) -> Option<IntermediateResult> {
    let mut decomp = decompose(pattern, index)?;

    if decomp.triples.is_empty() {
        return None;
    }

    // Pre-pass: resolve `var.attr = literal` predicates against any secondary
    // index the store exposes, then constant-fold the variable's positions in
    // the triples and pre-bind it in the result tuple. Without this, even with
    // the eq weight=1 in the VEO, the lonely-var binding would still scan the
    // variable's position before rejecting (see `veo.rs` rationale comment).
    let mut pinned = if std::env::var("GQLITE_DISABLE_INDEX_FOLD").is_ok() {
        Vec::new()
    } else {
        match fold_indexed_constants(graph, &mut decomp) {
            FoldOutcome::EmptyResult => return Some(IntermediateResult::new(Vec::new())),
            FoldOutcome::Pinned(p) => p,
        }
    };

    // External pins. Used by:
    // (a) the BTree-LTJ-real ORDER BY path — one pin per (sort-value, id);
    // (b) the OPTIONAL MATCH bind-pushdown — one pin per shared variable
    //     between the outer binding row and the inner pattern.
    // Resolve each name to its var_id, substitute term positions to a
    // constant (mirroring `fold_indexed_constants`), and drop the satisfied
    // label / attr filters for the pinned variable.
    for (pin_name, pin_id) in external_pins {
        if let Some(pin_var_id) = decomp
            .var_id_to_name
            .iter()
            .position(|n| n == *pin_name)
            .map(|i| i as u8)
        {
            if !pinned.iter().any(|(v, _)| *v == pin_var_id) {
                pinned.push((pin_var_id, *pin_id));
                for triple in decomp.triples.iter_mut() {
                    for term in triple.terms.iter_mut() {
                        if let Term::Variable(v) = term {
                            if *v == pin_var_id {
                                *term = Term::Constant(*pin_id);
                            }
                        }
                    }
                }
                decomp.filters.retain(|f| match &f.kind {
                    // Pinning a variable to a specific NodeId via a
                    // (label, attr) btree guarantees the label and the
                    // existence of the attribute on that node — drop
                    // those filters. NodeAttrCmp / NodeInSet check the
                    // VALUE of the attribute against a literal / set,
                    // which the pin doesn't fix; keep them so the LTJ
                    // evaluates them at level 0 against the pinned
                    // binding before it descends further.
                    FilterKind::NodeLabel { var_id, .. }
                    | FilterKind::NodeProperty { var_id, .. } => *var_id != pin_var_id,
                    FilterKind::NodeAttrCmp { .. } | FilterKind::NodeInSet { .. } => true,
                });
            }
        }
    }

    if std::env::var("GQLITE_DISABLE_INDEX_FOLD").is_err() {
        fold_range_filters(graph, &mut decomp);
    }
    if !pinned.is_empty() && std::env::var("GQLITE_DEBUG_INDEXES").is_ok() {
        eprintln!(
            "ltj: pinned {} variable(s) via secondary index: {:?}",
            pinned.len(),
            pinned
                .iter()
                .map(|(v, n)| format!("{}={}", decomp.var_id_to_name[*v as usize], n))
                .collect::<Vec<_>>()
        );
    }

    let num_vars = decomp.var_id_to_name.len();
    let pinned_set: std::collections::HashSet<u8> = pinned.iter().map(|(v, _)| *v).collect();

    // Build iterators and var maps
    let mut iterators: Vec<LtjIterator> = Vec::new();
    let mut var_to_iterators: Vec<Vec<usize>> = vec![vec![]; num_vars];
    let mut var_to_positions: Vec<Vec<SpoPos>> = vec![vec![]; num_vars];

    for triple in &decomp.triples {
        let iter = LtjIterator::new(triple.clone(), index);
        let iter_idx = iterators.len();
        iterators.push(iter);

        for (pos_idx, term) in triple.terms.iter().enumerate() {
            if let Term::Variable(var_id) = term {
                let spo_pos = match pos_idx {
                    0 => SpoPos::S,
                    1 => SpoPos::P,
                    2 => SpoPos::O,
                    _ => unreachable!(),
                };
                var_to_iterators[*var_id as usize].push(iter_idx);
                var_to_positions[*var_id as usize].push(spo_pos);
            }
        }
    }

    // Build VEO with selectivity-aware weights. Variables with strong filters
    // (equality on a constant, range comparison, label) bind early; the
    // lonely / non-lonely flag survives only as a tiebreaker for variables
    // with the same selectivity class. Pinned vars are excluded — their
    // NodeId is already known via the index lookup.
    let weights = estimate_var_weights(num_vars, &decomp.filters, index.len());
    let var_info: Vec<(u8, usize, bool)> = (0..num_vars)
        .filter(|v| !pinned_set.contains(&(*v as u8)))
        .map(|v| {
            let v_id = v as u8;
            let iters = &var_to_iterators[v];
            let is_lonely = iters.len() <= 1;
            let weight = if iters.is_empty() {
                usize::MAX
            } else {
                weights[v]
            };
            (v_id, weight, is_lonely)
        })
        .collect();

    let veo = VeoSimple::new(var_info);

    // Place filters
    let mut filters_at_level: Vec<Vec<PlacedFilter>> = vec![vec![]; veo.size()];
    for filter in &decomp.filters {
        let max_level = filter
            .depends_on
            .iter()
            .filter_map(|&var_id| (0..veo.size()).find(|&j| veo.var_at(j) == var_id))
            .max()
            .unwrap_or(0);

        if max_level < filters_at_level.len() {
            filters_at_level[max_level].push(PlacedFilter {
                eval_at_level: max_level,
                kind: filter.kind.clone(),
            });
        }
    }

    // Flag triples that bind an edge variable. The LTJ base case uses this
    // to fan out one row per parallel entry in those triples — the only
    // way to recover edges that share the same (src, label, tgt) prefix,
    // since the trie collapses them by design.
    let triple_has_edge_var: Vec<bool> = decomp
        .triple_info
        .iter()
        .map(|(_, _, edge_var, _)| edge_var.is_some())
        .collect();

    // Run LTJ
    let algorithm = LtjAlgorithm::new(
        iterators,
        var_to_iterators,
        var_to_positions,
        Box::new(veo),
        filters_at_level,
        num_vars,
        pinned,
        triple_has_edge_var,
    );

    let mut runner = LtjRunner::new(algorithm, graph);
    let tuples = runner.run(limit);

    Some(convert_results(graph, &tuples, &decomp))
}

// ---- Index-driven constant folding ----

enum FoldOutcome {
    /// Every Eq predicate that hit the index resolved to at least one node;
    /// the listed variables were pinned to specific NodeIds.
    Pinned(Vec<(u8, u32)>),
    /// At least one Eq predicate hit the index but matched zero nodes.
    /// The whole pattern is unsatisfiable; LTJ skips and returns empty.
    EmptyResult,
}

/// Walk the extracted filters; for each `NodeAttrCmp { Eq, value }` that
/// matches a known secondary index on the variable's label+attr, look up the
/// matching NodeId. If exactly one node matches, pin the variable: substitute
/// the matching NodeId for every Term::Variable(v) in the triples and drop the
/// satisfied Eq + NodeLabel filters for that variable.
fn fold_indexed_constants<G: GraphAccess>(graph: &G, decomp: &mut Decomposition) -> FoldOutcome {
    // var_id → labels declared via NodeLabel filters. A variable can carry
    // more than one label (compound `:A & B`); the index lookup needs at
    // least one to identify the (label, prop) pair.
    let mut var_to_labels: HashMap<u8, Vec<String>> = HashMap::new();
    for f in decomp.filters.iter() {
        if let FilterKind::NodeLabel { var_id, label } = &f.kind {
            var_to_labels
                .entry(*var_id)
                .or_default()
                .push(label.clone());
        }
    }

    let mut pinned: HashMap<u8, u32> = HashMap::new();
    let mut resolved_filter_indices: Vec<usize> = Vec::new();

    for (i, f) in decomp.filters.iter().enumerate() {
        let FilterKind::NodeAttrCmp {
            var_id,
            attr,
            op: BinOp::Eq,
            value,
        } = &f.kind
        else {
            continue;
        };
        if pinned.contains_key(var_id) {
            continue;
        }
        let Some(labels) = var_to_labels.get(var_id) else {
            continue;
        };
        // Try every label the var carries; the first hit wins. With auto-
        // inferred indexes there is at most one anyway.
        let mut hits = None;
        for label in labels {
            if let Some(h) = graph.lookup_node_eq(label, attr, value) {
                hits = Some(h);
                break;
            }
        }
        let Some(hits) = hits else { continue };
        if hits.is_empty() {
            return FoldOutcome::EmptyResult;
        }
        if hits.len() == 1 {
            pinned.insert(*var_id, hits[0]);
            resolved_filter_indices.push(i);
        }
        // Multi-hit: skipped for v1 (would need a NodeInSet filter). The
        // existing filter still runs as a fallback so correctness holds.
    }

    if pinned.is_empty() {
        return FoldOutcome::Pinned(Vec::new());
    }

    // Substitute pinned vars in every triple position.
    for triple in decomp.triples.iter_mut() {
        for term in triple.terms.iter_mut() {
            if let Term::Variable(v) = term {
                if let Some(&node_id) = pinned.get(v) {
                    *term = Term::Constant(node_id);
                }
            }
        }
    }

    // Drop the satisfied Eq filters AND any NodeLabel filters for vars we
    // resolved through the index — the index is keyed by label, so the label
    // constraint is already checked.
    let resolved_set: std::collections::HashSet<usize> =
        resolved_filter_indices.into_iter().collect();
    let drained: Vec<ExtractedFilter> = decomp.filters.drain(..).collect();
    decomp.filters = drained
        .into_iter()
        .enumerate()
        .filter(|(i, f)| {
            if resolved_set.contains(i) {
                return false;
            }
            if let FilterKind::NodeLabel { var_id, .. } = &f.kind {
                if pinned.contains_key(var_id) {
                    return false;
                }
            }
            true
        })
        .map(|(_, f)| f)
        .collect();

    FoldOutcome::Pinned(pinned.into_iter().collect())
}

/// Walk the extracted filters and, for each `NodeAttrCmp` with a range
/// operator (`<`, `<=`, `>`, `>=`) whose `(label, attr)` is btree-indexed,
/// pre-compute the matching set via the btree and replace the filter with a
/// `NodeInSet`. Set membership is O(log n) and avoids reading the bound
/// node's property from the page cache for every candidate.
///
/// Multiple range predicates on the same variable would ideally intersect;
/// for v1 we just substitute each one independently. The runner evaluates
/// every filter, so two `NodeInSet` filters compose correctly (both must
/// pass), they're just less efficient than a single intersected set.
fn fold_range_filters<G: GraphAccess>(graph: &G, decomp: &mut Decomposition) {
    use std::ops::Bound;

    let mut var_to_labels: HashMap<u8, Vec<String>> = HashMap::new();
    for f in decomp.filters.iter() {
        if let FilterKind::NodeLabel { var_id, label } = &f.kind {
            var_to_labels
                .entry(*var_id)
                .or_default()
                .push(label.clone());
        }
    }

    for f in decomp.filters.iter_mut() {
        let FilterKind::NodeAttrCmp {
            var_id,
            attr,
            op,
            value,
        } = &f.kind
        else {
            continue;
        };
        let (lo, hi) = match op {
            BinOp::Lt => (Bound::Unbounded, Bound::Excluded(value.clone())),
            BinOp::Le => (Bound::Unbounded, Bound::Included(value.clone())),
            BinOp::Gt => (Bound::Excluded(value.clone()), Bound::Unbounded),
            BinOp::Ge => (Bound::Included(value.clone()), Bound::Unbounded),
            _ => continue,
        };
        let Some(labels) = var_to_labels.get(var_id) else {
            continue;
        };
        let mut hits = None;
        for label in labels {
            if let Some(h) = graph.lookup_node_range(label, attr, lo.clone(), hi.clone()) {
                hits = Some(h);
                break;
            }
        }
        let Some(mut hits) = hits else { continue };
        hits.sort_unstable();
        hits.dedup();
        f.kind = FilterKind::NodeInSet {
            var_id: *var_id,
            set: std::sync::Arc::new(hits),
        };
    }
}

// ---- Weight estimation for the VEO ----

/// Estimate a per-variable weight for VEO ordering. Smaller weight binds
/// earlier. The estimate uses the filters extracted from descriptors:
///
/// - `NodeAttrCmp` with `Eq` on a constant → point-lookup, weight 1.
/// - `NodeAttrCmp` with a range op → ~10% of the index size.
/// - `NodeLabel` → ~25% of the index size (label sets are typically a
///   fraction of the total node space; without a real cardinality estimator
///   this is a rough surrogate).
/// - No filter → full index size (no narrowing).
///
/// The estimate is intentionally coarse: the goal is to lift a heavily
/// filtered variable above an unfiltered one, not to compute an accurate
/// cardinality. A future iteration can plug in real label-index sizes and
/// per-attribute histograms.
fn estimate_var_weights(num_vars: usize, filters: &[ExtractedFilter], total: usize) -> Vec<usize> {
    let mut has_eq = vec![false; num_vars];
    let mut has_cmp = vec![false; num_vars];
    let mut has_label = vec![false; num_vars];

    for f in filters {
        match &f.kind {
            FilterKind::NodeAttrCmp { var_id, op, .. } => {
                let i = *var_id as usize;
                match op {
                    BinOp::Eq => has_eq[i] = true,
                    BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        has_cmp[i] = true;
                    }
                    _ => {}
                }
            }
            FilterKind::NodeLabel { var_id, .. } => has_label[*var_id as usize] = true,
            FilterKind::NodeProperty { .. } => {}
            // A precomputed set is the most selective predicate available —
            // treat it as the strongest equality (weight 1) so the variable
            // binds early and rejects non-members before recursion.
            FilterKind::NodeInSet { var_id, .. } => has_eq[*var_id as usize] = true,
        }
    }

    let total = total.max(1);
    (0..num_vars)
        .map(|v| {
            if has_eq[v] {
                1
            } else if has_cmp[v] {
                (total / 10).max(2)
            } else if has_label[v] {
                (total / 4).max(2)
            } else {
                total
            }
        })
        .collect()
}

// ---- Flatten concat chains ----

/// Flatten a left-associative concat tree into a linear sequence of elements.
fn flatten_concat<'a>(p: &'a PathPattern, out: &mut Vec<FlatElement<'a>>) -> bool {
    match p {
        PathPattern::Node(d) => {
            out.push(FlatElement::Node(d.as_ref()));
            true
        }
        PathPattern::EdgeRight(d) => {
            out.push(FlatElement::Edge(EdgeKind::Right, d.as_ref()));
            true
        }
        PathPattern::EdgeLeft(d) => {
            out.push(FlatElement::Edge(EdgeKind::Left, d.as_ref()));
            true
        }
        PathPattern::EdgeUndirected(d) => {
            out.push(FlatElement::Edge(EdgeKind::Undirected, d.as_ref()));
            true
        }
        PathPattern::Concat(p1, p2) => flatten_concat(p1, out) && flatten_concat(p2, out),
        _ => false, // Not decomposable (any-direction `-[]-`, unions, repetition, …)
    }
}

// ---- Decomposition ----

fn decompose(pattern: &PathPattern, index: &TripleIndex) -> Option<Decomposition> {
    let mut triples = Vec::new();
    let mut var_name_to_id: HashMap<String, u8> = HashMap::new();
    let mut var_id_to_name: Vec<String> = Vec::new();
    let mut filters = Vec::new();
    let mut internal_vars = Vec::new();
    let mut triple_info = Vec::new();
    let mut fresh_counter = 0u32;
    let mut join_boundaries = Vec::new();

    let _last_var = decompose_pattern_top(
        pattern,
        index,
        &mut triples,
        &mut var_name_to_id,
        &mut var_id_to_name,
        &mut filters,
        &mut internal_vars,
        &mut triple_info,
        &mut fresh_counter,
        &mut join_boundaries,
    )?;

    if triples.is_empty() {
        return None;
    }

    Some(Decomposition {
        triples,
        var_id_to_name,
        filters,
        internal_vars,
        triple_info,
        join_boundaries,
    })
}

/// Top-level decomposition that tracks join boundaries.
// LTJ decomposition state — bundle of mutable accumulators that have to
// thread through the recursive walk. Refactor candidate, not a bug.
#[allow(clippy::too_many_arguments)]
fn decompose_pattern_top(
    pattern: &PathPattern,
    index: &TripleIndex,
    triples: &mut Vec<TriplePattern>,
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    filters: &mut Vec<ExtractedFilter>,
    internal: &mut Vec<u8>,
    triple_info: &mut Vec<(u8, u8, Option<String>, EdgeKind)>,
    fresh: &mut u32,
    join_boundaries: &mut Vec<(usize, usize)>,
) -> Option<u8> {
    match pattern {
        PathPattern::Join(p1, p2) => {
            let start1 = triples.len();
            decompose_pattern_top(
                p1,
                index,
                triples,
                names,
                id_to_name,
                filters,
                internal,
                triple_info,
                fresh,
                join_boundaries,
            )?;
            // If p1 was not itself a join, record its boundary
            if join_boundaries.is_empty() || join_boundaries.last().unwrap().1 != triples.len() {
                join_boundaries.push((start1, triples.len()));
            }

            let start2 = triples.len();
            let result = decompose_pattern_top(
                p2,
                index,
                triples,
                names,
                id_to_name,
                filters,
                internal,
                triple_info,
                fresh,
                join_boundaries,
            )?;
            // If p2 was not itself a join, record its boundary
            if join_boundaries.last().unwrap().1 != triples.len() {
                join_boundaries.push((start2, triples.len()));
            }

            Some(result)
        }
        _ => decompose_pattern(
            pattern,
            index,
            triples,
            names,
            id_to_name,
            filters,
            internal,
            triple_info,
            fresh,
        ),
    }
}

// Same accumulator bundle as decompose_pattern_top.
#[allow(clippy::too_many_arguments)]
fn decompose_pattern(
    pattern: &PathPattern,
    index: &TripleIndex,
    triples: &mut Vec<TriplePattern>,
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    filters: &mut Vec<ExtractedFilter>,
    internal: &mut Vec<u8>,
    triple_info: &mut Vec<(u8, u8, Option<String>, EdgeKind)>,
    fresh: &mut u32,
) -> Option<u8> {
    match pattern {
        PathPattern::Join(p1, p2) => {
            // Join at non-top level: decompose both sides without boundary tracking
            decompose_pattern(
                p1,
                index,
                triples,
                names,
                id_to_name,
                filters,
                internal,
                triple_info,
                fresh,
            )?;
            decompose_pattern(
                p2,
                index,
                triples,
                names,
                id_to_name,
                filters,
                internal,
                triple_info,
                fresh,
            )
        }

        PathPattern::Concat(_, _) => {
            // Flatten the concat chain into [Node, Edge, Node, Edge, Node, ...]
            let mut elems = Vec::new();
            if !flatten_concat(pattern, &mut elems) {
                return None;
            }
            decompose_flat_chain(
                &elems,
                index,
                triples,
                names,
                id_to_name,
                filters,
                internal,
                triple_info,
                fresh,
            )
        }

        PathPattern::Node(desc) => {
            // Standalone node — no triple, just return the var
            Some(node_var(
                desc.as_ref(),
                names,
                id_to_name,
                filters,
                internal,
                fresh,
            ))
        }

        PathPattern::Filter(inner, _expr) => decompose_pattern(
            inner,
            index,
            triples,
            names,
            id_to_name,
            filters,
            internal,
            triple_info,
            fresh,
        ),

        _ => None,
    }
}

/// Decompose a flat chain `Node (Edge Node?)*` into triples. The
/// trailing node is optional after every edge: when two edges sit
/// adjacent (`Edge Edge`, written by users as `~[:r]~~[:r]~` or
/// produced by the unroll pass for fixed-bound repetitions) we
/// synthesise a fresh anonymous node as the boundary, mirroring what
/// the runtime's `Concat` evaluator would do via path-merge on
/// `last_node_id` / `first_node_id`. Same for an edge with nothing
/// after it (a trailing dangling edge).
// Same accumulator bundle as decompose_pattern_top.
#[allow(clippy::too_many_arguments)]
fn decompose_flat_chain(
    elems: &[FlatElement],
    index: &TripleIndex,
    triples: &mut Vec<TriplePattern>,
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    filters: &mut Vec<ExtractedFilter>,
    internal: &mut Vec<u8>,
    triple_info: &mut Vec<(u8, u8, Option<String>, EdgeKind)>,
    fresh: &mut u32,
) -> Option<u8> {
    if elems.is_empty() {
        return None;
    }

    // First element must be a Node
    let FlatElement::Node(first_desc) = &elems[0] else {
        return None;
    };
    let mut current_node = node_var(*first_desc, names, id_to_name, filters, internal, fresh);

    let mut i = 1;
    while i < elems.len() {
        let FlatElement::Edge(kind, edge_desc) = &elems[i] else {
            return None;
        };

        // The boundary after this edge is either an explicit Node (use
        // its descriptor and consume both) or implicit (fresh internal
        // var; consume only the edge). The implicit branch is what
        // turns `Edge Edge` and trailing-edge cases into well-formed
        // triples.
        let (next_var, advance) = match elems.get(i + 1) {
            Some(FlatElement::Node(tgt_desc)) => (
                node_var(*tgt_desc, names, id_to_name, filters, internal, fresh),
                2,
            ),
            _ => (fresh_var(names, id_to_name, internal, fresh), 1),
        };

        let edge_var_name = edge_desc.and_then(|d| d.var.clone());
        let p_term = build_p_term(*edge_desc, index, names, id_to_name, internal, fresh);

        // Forward (`-[L]->`) and undirected (`~[L]~`) emit a triple
        // (current, L, next). Leftward (`<-[L]-`) emits (next, L, current)
        // — i.e. the physical directed edge that flows from `next` into
        // `current`. Undirected schema entries are present in both senses
        // in the index, so a forward triple lookup catches them.
        let (src_var, tgt_var) = match kind {
            EdgeKind::Right | EdgeKind::Undirected => (current_node, next_var),
            EdgeKind::Left => (next_var, current_node),
        };

        triples.push(TriplePattern {
            terms: [Term::Variable(src_var), p_term, Term::Variable(tgt_var)],
        });
        triple_info.push((src_var, tgt_var, edge_var_name, *kind));

        current_node = next_var;
        i += advance;
    }

    if triples.is_empty() {
        return None; // Just a node, no edges
    }

    Some(current_node)
}

// ---- Helper functions ----

fn node_var(
    desc: Option<&Descriptor>,
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    filters: &mut Vec<ExtractedFilter>,
    internal: &mut Vec<u8>,
    fresh: &mut u32,
) -> u8 {
    let var_id = if let Some(d) = desc {
        if let Some(name) = &d.var {
            get_or_create(name, names, id_to_name)
        } else {
            fresh_var(names, id_to_name, internal, fresh)
        }
    } else {
        fresh_var(names, id_to_name, internal, fresh)
    };

    // Extract label filters
    if let Some(d) = desc {
        for label in d.dtype.label.required_labels() {
            filters.push(ExtractedFilter {
                depends_on: vec![var_id],
                kind: FilterKind::NodeLabel {
                    var_id,
                    label: label.to_string(),
                },
            });
        }
        // Extract value predicates pushed down by the optimizer
        for (attr, op, value) in &d.value_preds {
            filters.push(ExtractedFilter {
                depends_on: vec![var_id],
                kind: FilterKind::NodeAttrCmp {
                    var_id,
                    attr: attr.clone(),
                    op: *op,
                    value: value.clone(),
                },
            });
        }
    }

    var_id
}

fn get_or_create(name: &str, names: &mut HashMap<String, u8>, id_to_name: &mut Vec<String>) -> u8 {
    if let Some(&id) = names.get(name) {
        id
    } else {
        let id = id_to_name.len() as u8;
        id_to_name.push(name.to_string());
        names.insert(name.to_string(), id);
        id
    }
}

fn fresh_var(
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    internal: &mut Vec<u8>,
    counter: &mut u32,
) -> u8 {
    let name = format!("_ltj_{}", counter);
    *counter += 1;
    let id = get_or_create(&name, names, id_to_name);
    internal.push(id);
    id
}

/// Build the P-term of a triple pattern from an edge descriptor: a label
/// constant when there's exactly one required label, a fresh variable
/// otherwise (wildcard / multi-label).
fn build_p_term(
    edge_desc: Option<&Descriptor>,
    index: &TripleIndex,
    names: &mut HashMap<String, u8>,
    id_to_name: &mut Vec<String>,
    internal: &mut Vec<u8>,
    fresh: &mut u32,
) -> Term {
    if let Some(d) = edge_desc {
        let labels = d.dtype.label.required_labels();
        if labels.len() == 1 {
            return match index.label_to_id.get(labels[0]) {
                Some(&lid) => Term::Constant(lid),
                None => Term::Constant(u32::MAX), // label not in graph
            };
        }
    }
    let p_var = fresh_var(names, id_to_name, internal, fresh);
    Term::Variable(p_var)
}

// ---- Result conversion ----

fn convert_results<G: GraphAccess>(
    graph: &G,
    tuples: &[ResultTuple],
    decomp: &Decomposition,
) -> IntermediateResult {
    let mut rows = Vec::new();

    // Determine which triple ranges correspond to separate paths.
    // If join_boundaries is non-empty, each boundary produces a separate path.
    // Otherwise, all triples form a single path.
    let ranges: Vec<(usize, usize)> = if decomp.join_boundaries.is_empty() {
        vec![(0, decomp.triple_info.len())]
    } else {
        decomp.join_boundaries.clone()
    };

    for tuple in tuples {
        let mut assignment = Assignment::new();

        // Build assignment, excluding internal variables
        for &(var_id, value) in &tuple.vars {
            if decomp.internal_vars.contains(&var_id) {
                continue;
            }
            let name = &decomp.var_id_to_name[var_id as usize];
            assignment.extend(name.clone(), PathValue::Node(value));
        }

        // Build one path per range
        let mut paths = Vec::new();
        for &(start, end) in &ranges {
            let mut path_elements = Vec::new();
            for ti in start..end {
                let (src_var, tgt_var, ref edge_var, kind) = decomp.triple_info[ti];
                let src_id = tuple
                    .vars
                    .iter()
                    .find(|(v, _)| *v == src_var)
                    .map(|(_, id)| *id);
                let tgt_id = tuple
                    .vars
                    .iter()
                    .find(|(v, _)| *v == tgt_var)
                    .map(|(_, id)| *id);

                if let (Some(src), Some(tgt)) = (src_id, tgt_id) {
                    // Orient the triple along the chain's traversal direction.
                    // `decompose_flat_chain` stores a Left (reverse `<-`) edge as
                    // (src = next node, tgt = current boundary), so the node already
                    // in the path is the *tgt* and the newly reached node is the
                    // *src*. Pushing `tgt` blindly (as the old code did) re-emitted
                    // the boundary node and dropped the new endpoint — producing a
                    // path with a duplicated node that then failed ACYCLIC/SIMPLE/
                    // TRAIL checks and corrupted SHORTEST lengths and named-path
                    // functions. `entry` is the boundary, `exit` is the new node.
                    let (entry, exit) = match kind {
                        EdgeKind::Right | EdgeKind::Undirected => (src, tgt),
                        EdgeKind::Left => (tgt, src),
                    };
                    if path_elements.is_empty() {
                        path_elements.push(PathValue::Node(entry));
                    }
                    // Prefer the eid carried out of the LTJ trie (matches
                    // the bound (src, label, tgt) exactly). Fall back to
                    // adjacency only when the per-triple eid is unset, e.g.
                    // when the iterator never bottomed out — defensive,
                    // shouldn't fire on well-formed patterns. `find_edge` keeps
                    // the triple's stored (src, tgt) orientation — that is the
                    // edge's real direction, independent of traversal sense.
                    let resolved_eid = tuple
                        .triple_eids
                        .get(ti)
                        .copied()
                        .filter(|&e| e != u32::MAX)
                        .or_else(|| find_edge(graph, src, tgt));
                    if let Some(eid) = resolved_eid {
                        // Use edge_path_value so undirected edges land as
                        // PathValue::EdgeUndirectional rather than being
                        // forced into the directional variant.
                        let pv = graph.edge_path_value(eid);
                        path_elements.push(pv.clone());
                        if let Some(ref ev) = edge_var {
                            assignment.extend(ev.clone(), pv);
                        }
                    }
                    path_elements.push(PathValue::Node(exit));
                }
            }
            paths.push(Path(path_elements));
        }

        rows.push(ResultRow::with_paths(paths, assignment));
    }

    IntermediateResult::new(rows)
}

/// Resolve the edge id for a (src, tgt) pair. Walks outgoing-directional
/// adjacency first; falls back to undirected adjacency when the matching
/// triple was sourced from an undirected edge (those entries are emitted
/// in both senses by `TripleIndex::from_graph`, so either endpoint may
/// appear as the triple's "source").
fn find_edge<G: GraphAccess>(graph: &G, src: Id, tgt: Id) -> Option<Id> {
    if let Some(&eid) = graph
        .outgoing_edges(src)
        .iter()
        .find(|&&eid| graph.tgt(eid) == tgt)
    {
        return Some(eid);
    }
    graph.undirected_edges_of(src).iter().copied().find(|&eid| {
        let s = graph.src(eid);
        let t = graph.tgt(eid);
        (s == src && t == tgt) || (s == tgt && t == src)
    })
}
