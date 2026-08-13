//! Strategy 2: drive the neighbour stream from inside the join.
//!
//! The search variable is placed at a chosen level of the variable
//! elimination order. Each time the backtracking search reaches that
//! level it materialises the candidate set for the variable — already
//! narrowed by the partial binding of everything above it — hashes it,
//! then walks the neighbour stream nearest-first and descends only into
//! candidates that are hits, with normal backtracking.
//!
//! **What the level does.** At level 0 the neighbour stream drives the
//! whole join and this degenerates to pre-filtering, except that the
//! candidate set is the variable's whole domain rather than the corpus.
//! Deeper down, the set at each visit is smaller but the level is
//! reached many times, once per binding of the variables above. Which
//! trades better is the question the benchmark exists to answer, so the
//! position is a knob (`FROGQL_VEC_LEVEL`) rather than a policy.
//!
//! **Why the answer is still correct off level 0.** The level is visited
//! many times and each visit is internally sorted by distance, but the
//! concatenation of the visits is not. A global bounded threshold fixes
//! that: `DistThreshold` holds the `k` best distances accepted so far,
//! and a visit stops as soon as the stream passes it. The threshold only
//! ever tightens, so a neighbour rejected once can never be needed
//! later. Ranking then happens once, at the end, over everything
//! accepted.

use crate::model::graph_access::GraphAccess;
use crate::runtime::engine::Runtime;
use crate::runtime::ltj::algorithm::VecCtx;
use crate::runtime::ltj::pattern_extract::{self, NnPlan};
use crate::runtime::result::IntermediateResult;
use crate::syntax::query::{KMode, Query};
use crate::vector::cursor::NnStream;
use crate::vector::store::VectorSet;

use super::topk::DistThreshold;
use super::{
    arm_label, effective_source, finish, row_node, NearestSpec, Strategy, TopK, VecCfg, VecSource,
    VecStats,
};

pub fn run<G: GraphAccess>(
    rt: &Runtime<'_, G>,
    query: &Query,
    spec: &NearestSpec,
    cfg: &VecCfg,
    set: &VectorSet,
    stats: &mut VecStats,
) -> Option<IntermediateResult> {
    let pattern = query.collapsed_pattern();
    let index = rt.warm_triple_index();

    let source = effective_source(cfg.source, set);
    let local = source == VecSource::LocalSort;
    // A local source never walks a corpus-wide stream, so it gets an
    // empty one rather than paying to build a ranking it will not read.
    let cursor: Box<dyn crate::vector::cursor::NnCursor> = if local {
        Box::new(crate::vector::cursor::EmptyCursor)
    } else {
        set.cursor(&spec.q, source == VecSource::Hnsw)
    };
    let mut ctx = VecCtx::new(
        NnStream::new(cursor),
        DistThreshold::new(spec.k, spec.mode),
        cfg.tau_eps,
        local,
        set,
        &spec.q,
    );
    let mut dists: Vec<Option<f32>> = Vec::new();

    let ir = pattern_extract::try_ltj_nearest(
        rt.graph,
        &pattern,
        &index,
        NnPlan {
            var: &spec.var,
            level: cfg.level,
            ctx: &mut ctx,
            dists: &mut dists,
        },
    );

    let ir = match ir {
        Some(ir) => ir,
        None => {
            stats.fallback_reason = Some(format!(
                "the pattern does not decompose with `{}` at a variable-elimination level",
                spec.var
            ));
            return None;
        }
    };

    stats.arm = arm_label(Strategy::InLtj, source);
    stats.ltj_visits = ctx.visits;
    stats.candidates_hashed = ctx.candidates_hashed;
    stats.nn_pops = ctx.nn_pops;
    stats.prefix_replays = ctx.stream.replays;
    stats.prefix_extends = ctx.stream.extends;
    stats.nn_expanded = ctx.stream.expanded();
    stats.pattern_runs = 1;

    // Final ranking. The search accepted everything inside the threshold,
    // which can be more than `k` — the threshold prunes, it does not
    // select — so the sink picks the winners under the same rule it
    // applies for every other strategy.
    let mut sink = TopK::new(spec.k, spec.mode);
    match spec.mode {
        KMode::Rows => {
            for (row, dist) in ir.rows.into_iter().zip(dists.iter()) {
                if let Some(d) = dist {
                    sink.offer_row(*d, row);
                }
            }
        }
        KMode::DistinctVar => {
            // Regroup by binding: `offer_var` takes a binding with all of
            // its rows at once, since `k` counts bindings.
            let mut order: Vec<u32> = Vec::new();
            let mut groups: std::collections::HashMap<u32, (f32, Vec<_>)> =
                std::collections::HashMap::new();
            for (row, dist) in ir.rows.into_iter().zip(dists.iter()) {
                let (Some(id), Some(d)) = (row_node(&row, &spec.var), *dist) else {
                    continue;
                };
                let entry = groups.entry(id).or_insert_with(|| {
                    order.push(id);
                    (d, Vec::new())
                });
                entry.1.push(row);
            }
            for id in order {
                if let Some((d, rows)) = groups.remove(&id) {
                    sink.offer_var(id, d, rows);
                }
            }
        }
    }

    Some(finish(sink, spec, stats))
}
