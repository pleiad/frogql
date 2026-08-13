//! Strategy 1: run the pattern, then rank what it produced.
//!
//! The baseline, and the only arm that works on every query shape, so it
//! is also the universal fallback. With `use_index = false` it is exact,
//! which makes it the oracle every other arm's recall is scored against.
//!
//! Two sub-modes, matching the two ways the literature describes
//! post-filtering:
//!
//! - **without a metric index** — compute the distance to every binding
//!   the pattern produced and keep the best `k`. Cost is linear in the
//!   number of *candidates*, not in the corpus.
//! - **with a metric index** — hash the candidates, then walk the global
//!   neighbour stream and stop once `k` of them have been hit. Cost is
//!   whatever the index charges to reach the `k`-th surviving candidate,
//!   which is small when the pattern is unselective and catastrophic
//!   when it is selective. That asymmetry is the thing worth measuring.

use std::collections::HashMap;

use crate::model::graph_access::GraphAccess;
use crate::model::value::Id;
use crate::runtime::engine::Runtime;
use crate::runtime::result::{IntermediateResult, ResultRow};
use crate::syntax::query::{KMode, Query};
use crate::vector::store::VectorSet;

use super::{finish, row_node, NearestSpec, TopK, VecCfg, VecStats};

pub fn run<G: GraphAccess>(
    rt: &Runtime<'_, G>,
    query: &Query,
    spec: &NearestSpec,
    cfg: &VecCfg,
    set: &VectorSet,
    stats: &mut VecStats,
) -> IntermediateResult {
    stats.arm = if cfg.use_index && set.has_index() {
        "post+index"
    } else {
        "post+brute"
    };

    // The whole pattern, unlimited: a LIMIT here would cut in row-arrival
    // order, which has nothing to do with distance.
    let ir = rt.run_match_chain_plain(query, 0);
    stats.pattern_runs += 1;

    // Bucket rows by the search variable's binding. A row whose search
    // variable is not a node cannot be ranked and is dropped.
    let mut buckets: HashMap<Id, Vec<ResultRow>> = HashMap::new();
    for row in ir.rows {
        if let Some(id) = row_node(&row, &spec.var) {
            buckets.entry(id).or_default().push(row);
        }
    }
    stats.candidates_hashed = buckets.len() as u64;

    let mut sink = TopK::new(spec.k, spec.mode);
    if buckets.is_empty() {
        return finish(sink, spec, stats);
    }

    if cfg.use_index && set.has_index() {
        walk_index(set, spec, cfg, &mut buckets, &mut sink, stats);
    } else {
        walk_candidates(set, spec, &mut buckets, &mut sink, stats);
    }

    finish(sink, spec, stats)
}

/// Exact sub-mode: distance to every candidate, sorted.
fn walk_candidates(
    set: &VectorSet,
    spec: &NearestSpec,
    buckets: &mut HashMap<Id, Vec<ResultRow>>,
    sink: &mut TopK,
    stats: &mut VecStats,
) {
    let ids: Vec<Id> = buckets.keys().copied().collect();
    let mut cursor = set.cursor_over(&spec.q, &ids);
    stats.nn_expanded += cursor.expanded();
    while let Some((id, dist)) = cursor.next() {
        stats.nn_pops += 1;
        if !offer(spec, buckets, sink, id, dist) {
            break;
        }
    }
}

/// Index sub-mode: walk the corpus-wide neighbour stream, testing
/// membership in the candidate hash, and stop as soon as the sink is
/// satisfied.
fn walk_index(
    set: &VectorSet,
    spec: &NearestSpec,
    cfg: &VecCfg,
    buckets: &mut HashMap<Id, Vec<ResultRow>>,
    sink: &mut TopK,
    stats: &mut VecStats,
) {
    let mut cursor = set.cursor(&spec.q, cfg.use_index);
    let mut remaining = buckets.len();
    while let Some((id, dist)) = cursor.next() {
        stats.nn_pops += 1;
        if !buckets.contains_key(&id) {
            continue;
        }
        remaining -= 1;
        if !offer(spec, buckets, sink, id, dist) {
            break;
        }
        // Every surviving candidate has been seen: nothing further in the
        // stream can be one, so walking on would be pure waste. This is
        // what stops an unreachable-tail corpus from being traversed in
        // full when the pattern is very selective.
        if remaining == 0 {
            break;
        }
    }
    stats.nn_expanded += cursor.expanded();
}

/// Feed one candidate to the sink. Returns false when the sink can accept
/// nothing further and the walk should stop.
///
/// The stream is non-decreasing, so once the sink is full every remaining
/// candidate is at least as far as its threshold and is rejected. The
/// exception is `DistinctVar` with an approximate cursor, where an
/// inversion can still bring something closer; `TopK` handles that
/// correctly, and the early stop below only fires when the cursor is
/// exact or the sink is genuinely saturated.
fn offer(
    spec: &NearestSpec,
    buckets: &mut HashMap<Id, Vec<ResultRow>>,
    sink: &mut TopK,
    id: Id,
    dist: f32,
) -> bool {
    let rows = match buckets.remove(&id) {
        Some(r) => r,
        None => return true,
    };
    match spec.mode {
        KMode::DistinctVar => sink.offer_var(id, dist, rows),
        KMode::Rows => {
            for row in rows {
                sink.offer_row(dist, row);
            }
        }
    }
    !sink.is_full() || dist < sink.threshold()
}
