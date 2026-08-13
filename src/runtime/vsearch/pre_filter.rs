//! Strategy 3: walk the neighbour stream, re-running the pattern with
//! the search variable pinned to each candidate.
//!
//! For every neighbour the index emits, substitute it as a constant for
//! the search variable and evaluate the whole pattern. Keep the ones that
//! match; stop when the sink is satisfied.
//!
//! Almost free to implement, because pinning a variable to a node id is
//! what the LTJ already does for correlated EXISTS and OPTIONAL
//! pushdown. The cost model is the interesting part: exactly one whole
//! pattern evaluation per neighbour examined, so it wins when the first
//! few neighbours also satisfy the pattern and loses badly when the
//! pattern is selective and the index has to walk deep to find a
//! survivor. `pattern_runs` is the number to watch.
//!
//! Note this is a special case of the in-LTJ strategy with the search
//! variable at level 0 — but only when it is at level 0. Placing it
//! deeper is something only the in-LTJ arm can do.

use crate::model::graph_access::GraphAccess;
use crate::runtime::engine::Runtime;
use crate::runtime::result::IntermediateResult;
use crate::syntax::query::{KMode, Query};
use crate::vector::store::VectorSet;

use super::{finish, NearestSpec, TopK, VecCfg, VecStats};

pub fn run<G: GraphAccess>(
    rt: &Runtime<'_, G>,
    query: &Query,
    spec: &NearestSpec,
    cfg: &VecCfg,
    set: &VectorSet,
    stats: &mut VecStats,
) -> Option<IntermediateResult> {
    let pattern = query.collapsed_pattern();

    // Probe once: if the pattern does not decompose into triples there is
    // nothing to pin, and the caller degrades to post-filtering. Doing it
    // up front avoids discovering it halfway through a neighbour walk.
    rt.run_pinned(&pattern, &[], 0)?;

    stats.arm = if cfg.use_index && set.has_index() {
        "pre+index"
    } else {
        "pre+brute"
    };

    let mut sink = TopK::new(spec.k, spec.mode);
    let mut cursor = super::cursor(set, spec, cfg);

    // In DistinctVar mode a candidate only has to *have* a match, so one
    // row is enough to decide. In Rows mode every row is a result.
    let per_candidate_limit = match spec.mode {
        KMode::DistinctVar => 1,
        KMode::Rows => 0,
    };

    while let Some((id, dist)) = cursor.next() {
        stats.nn_pops += 1;

        // The stream is non-decreasing, so once the sink is full and the
        // stream has passed its threshold nothing further can be
        // accepted. `tau_eps` widens the cut because an approximate
        // cursor's order is only approximately sorted.
        if sink.is_full() {
            let tau = sink.threshold();
            if dist > tau * (1.0 + cfg.tau_eps) {
                break;
            }
        }

        let matched = match rt.run_pinned(&pattern, &[(&spec.var, id)], per_candidate_limit) {
            Some(ir) => ir,
            // A shape that decomposed on the empty probe but not with a
            // pin should not exist; treat it as "no match" rather than
            // silently reporting a wrong top-k.
            None => continue,
        };
        stats.pattern_runs += 1;
        if matched.rows.is_empty() {
            continue;
        }

        match spec.mode {
            KMode::DistinctVar => {
                // Re-run unbounded: the probe above stopped at the first
                // row, but the answer must carry all of them.
                let rows = if per_candidate_limit == 0 {
                    matched.rows
                } else {
                    match rt.run_pinned(&pattern, &[(&spec.var, id)], 0) {
                        Some(full) => {
                            stats.pattern_runs += 1;
                            full.rows
                        }
                        None => matched.rows,
                    }
                };
                sink.offer_var(id, dist, rows);
            }
            KMode::Rows => {
                for row in matched.rows {
                    sink.offer_row(dist, row);
                }
            }
        }
    }
    stats.nn_expanded += cursor.expanded();

    Some(finish(sink, spec, stats))
}
