//! Strategy 2: split the join at the search variable and drive the
//! second half from the neighbour stream.
//!
//! The search variable is placed at a chosen level of the variable
//! elimination order, and evaluation runs in two phases.
//!
//! **Phase 1 — memoise the prefix.** Run the join down to that level and
//! stop. Every candidate that survives the levels above is recorded in a
//! table keyed by the candidate, holding *all* the prefixes that reach
//! it. One node is reachable by many paths, so a key carries a list, and
//! phase 2 has to follow every entry of it.
//!
//! **Phase 2 — one global walk.** Walk the ranking nearest-first, once.
//! A candidate the table does not hold costs a hash lookup and nothing
//! else. A candidate it does hold is resumed: replay its stored prefix
//! and search the levels below. Stop as soon as the walk passes the
//! running top-`k` threshold.
//!
//! **Why two phases rather than one interleaved walk.** The level is
//! reached once per binding of everything above it. Walking a
//! nearest-first stream *inside* each of those visits re-walks it: each
//! visit is internally sorted but their concatenation is not, so the
//! best candidate overall may live under the last prefix enumerated and
//! no visit can stop early on distance until a later one has already
//! contributed. Hoisting the walk out makes the distance cut global,
//! which is the only version of the cut that ends the search rather than
//! trimming each visit.
//!
//! **What the level does.** It sets where the join is cut. At level 0 the
//! prefix is empty, the table is the variable's whole domain, and this
//! is pre-filtering with the domain memoised. Deeper down the table is
//! smaller and better conditioned, but phase 1 pays more of the join
//! before the ranking is consulted at all. Which trades better is the
//! question the benchmark exists to answer, so the position stays a knob
//! (`FROGQL_VEC_LEVEL`) rather than a policy.

use crate::model::graph_access::GraphAccess;
use crate::runtime::engine::Runtime;
use crate::runtime::ltj::algorithm::{NnMode, VecCtx};
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
    mode: NnMode,
) -> Option<IntermediateResult> {
    let pattern = query.collapsed_pattern();

    // A residual `WHERE` is dropped by the decomposition — sound for
    // callers that reach LTJ through `run_path_pattern`, which re-applies
    // it, and unsound here, where the decomposition is invoked directly.
    // Post-filtering the output would not repair it: the search prunes
    // with a running top-`k` threshold, so a row the predicate rejects has
    // already tightened the cut and excluded neighbours that belonged in
    // the answer. Degrade to post-filtering, which evaluates the whole
    // query.
    if pattern.has_residual_filter() {
        stats.fallback_reason = Some(
            "the query has a WHERE the in-LTJ arms cannot evaluate inside the search".to_string(),
        );
        return None;
    }

    let index = rt.warm_triple_index();

    let source = effective_source(cfg.source, set);
    let local = source == VecSource::LocalSort;
    // A local source ranks the context table's keys directly, so it
    // never reads a corpus-wide stream and gets an empty one rather than
    // paying to build a ranking it will not use. A correlated clause has
    // no query vector yet either — its first stream is built when the
    // search reaches its first anchor.
    let cursor: Box<dyn crate::vector::cursor::NnCursor> = if local || spec.anchor.is_some() {
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
        mode,
    );
    // Only the correlated path rebuilds its stream, and it has to
    // reproduce the source the caller asked for.
    ctx.use_hnsw = source == VecSource::Hnsw;
    let mut dists: Vec<Option<f32>> = Vec::new();

    let ir = pattern_extract::try_ltj_nearest(
        rt.graph,
        &pattern,
        &index,
        NnPlan {
            var: &spec.var,
            anchor: spec.anchor.as_deref(),
            level: cfg.level,
            ctx: &mut ctx,
            dists: &mut dists,
        },
    );

    let ir = match ir {
        Some(ir) => ir,
        None => {
            stats.fallback_reason = Some(match &spec.anchor {
                Some(a) => format!(
                    "the pattern does not decompose with `{}` at a variable-elimination \
                     level below `{a}`",
                    spec.var
                ),
                None => format!(
                    "the pattern does not decompose with `{}` at a variable-elimination level",
                    spec.var
                ),
            });
            return None;
        }
    };

    let strategy = match mode {
        NnMode::Interleave => Strategy::Interleave,
        NnMode::Memo => Strategy::Memo,
    };
    stats.arm = arm_label(strategy, source);
    stats.ltj_visits = ctx.visits;
    stats.candidates_hashed = ctx.candidates_hashed;
    stats.nn_pops = ctx.nn_pops;
    stats.prefix_replays = ctx.stream.replays;
    stats.prefix_extends = ctx.stream.extends;
    stats.nn_expanded = ctx.stream.expanded();
    stats.suffix_resumes = ctx.resumes;
    // One prefix pass, however many suffixes it resumes. The pair
    // (`candidates_hashed`, `suffix_resumes`) is what the memo buys:
    // the first is how many candidates the join produced, the second how
    // many of them the neighbour order made worth completing.
    stats.pattern_runs = 1;

    // Final ranking. The search accepted everything inside the threshold,
    // which can be more than `k` — the threshold prunes, it does not
    // select — so the sink picks the winners under the same rule it
    // applies for every other strategy.
    //
    // A correlated clause counts `k` **per anchor**, so it gets one sink
    // per anchor rather than one for the query. Partitioning here rather
    // than inside the search keeps the two cases sharing every line
    // below: the search already reset its threshold per anchor, and this
    // applies the same selection rule the other strategies use.
    if let Some(anchor) = &spec.anchor {
        return Some(select_per_anchor(ir, &dists, spec, anchor, stats));
    }

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

/// Select the `k` nearest per anchor, then concatenate in first-seen
/// anchor order.
///
/// The rows arrive in join order, carrying every candidate the per-anchor
/// threshold let through — which is at least `k` per anchor and usually
/// more, since the threshold prunes rather than selects. Grouping by the
/// anchor binding and running the ordinary sink inside each group is what
/// makes the correlated in-LTJ arm answer the same thing the correlated
/// post-filter arm does.
fn select_per_anchor(
    ir: IntermediateResult,
    dists: &[Option<f32>],
    spec: &NearestSpec,
    anchor: &str,
    stats: &mut VecStats,
) -> IntermediateResult {
    use std::collections::HashMap;

    let mut order: Vec<u32> = Vec::new();
    let mut groups: HashMap<u32, Vec<(f32, crate::runtime::result::ResultRow)>> = HashMap::new();
    for (row, dist) in ir.rows.into_iter().zip(dists.iter()) {
        let (Some(a), Some(d)) = (row_node(&row, anchor), *dist) else {
            continue;
        };
        groups.entry(a).or_insert_with(|| {
            order.push(a);
            Vec::new()
        });
        groups.get_mut(&a).expect("just inserted").push((d, row));
    }
    stats.anchor_groups = order.len() as u64;

    let mut out: Vec<crate::runtime::result::ResultRow> = Vec::new();
    for a in order {
        let Some(rows) = groups.remove(&a) else {
            continue;
        };
        let mut sink = TopK::new(spec.k, spec.mode);
        match spec.mode {
            KMode::Rows => {
                for (d, row) in rows {
                    sink.offer_row(d, row);
                }
            }
            KMode::DistinctVar => {
                // `k` counts bindings, so a binding is offered once with
                // all of its rows.
                let mut seen: Vec<u32> = Vec::new();
                let mut by_id: HashMap<u32, (f32, Vec<_>)> = HashMap::new();
                for (d, row) in rows {
                    let Some(id) = row_node(&row, &spec.var) else {
                        continue;
                    };
                    by_id.entry(id).or_insert_with(|| {
                        seen.push(id);
                        (d, Vec::new())
                    });
                    by_id.get_mut(&id).expect("just inserted").1.push(row);
                }
                for id in seen {
                    if let Some((d, rows)) = by_id.remove(&id) {
                        sink.offer_var(id, d, rows);
                    }
                }
            }
        }
        stats.rows_buffered += sink.buffered;
        stats.rows_evicted += sink.evicted;
        for (dist, mut row) in sink.drain_sorted() {
            super::bind_distance(&mut row, spec, dist);
            out.push(row);
        }
    }
    IntermediateResult::new(out)
}
