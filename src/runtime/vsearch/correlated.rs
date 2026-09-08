//! Correlated `NEAREST`: the query vector comes out of the pattern.
//!
//! The four strategies next door all answer "which nodes satisfy this
//! pattern and are among the `k` nearest to **a** query vector" — one
//! vector, fixed before the search starts, which is why `resolve_spec`
//! can evaluate it against an empty assignment.
//!
//! This module answers a different question:
//!
//! ```text
//!   MATCH (v10)-[:P69]->(v00), (v01)-[:P69]->(v11)
//!   NEAREST 50 v11.hog TO VECTOR(v00, 'hog') AS d
//! ```
//!
//! "for **each** `v00` the pattern binds, which `v11` are among its 50
//! nearest". That is a similarity *join*, not a similarity *search*: the
//! query vector is a function of the row, so there are as many searches
//! as there are distinct anchors.
//!
//! # How
//!
//! Run the pattern once, partition its rows by the anchor binding, and
//! rank each partition against its own query vector. Partitioning first
//! is what keeps it one pattern evaluation rather than one per anchor:
//! the anchors are not known until the pattern has run, and re-running it
//! pinned per anchor would pay for the whole pattern again each time.
//!
//! Ranking inside a partition is delegated to `post_filter::rank_buckets`,
//! so the correlated and uncorrelated arms share one definition of what
//! "the k nearest of this candidate set" means. `FROGQL_VEC_SOURCE`
//! selects where each partition's ranking comes from, exactly as it does
//! for the uncorrelated arms:
//!
//! - `localsort` (exact) ranks only that partition's candidates. Cost is
//!   `O(|partition| log |partition|)` and it never touches a node outside
//!   the match — the right default when the pattern is selective.
//! - `hnsw` walks the corpus-wide proximity graph per partition, testing
//!   membership. Cheap when the partition is a large fraction of the
//!   corpus, and increasingly wasteful as the pattern narrows.
//! - `globalsort` (exact) sorts the whole attribute **per partition**, so
//!   it is the oracle rather than a plan anyone should run at scale.
//!
//! # What is not here
//!
//! `pre` has no correlated form. Pre-filtering's defining property is
//! that it never runs the pattern first, and the anchors are not known
//! until it has; reaching them would need the minimal sub-pattern that
//! binds the anchor, which is query planning this engine does not do. A
//! correlated `pre` therefore falls back here, with the reason recorded,
//! and `stats.arm` reports `correlated+<source>` so a benchmark row never
//! claims an arm that did not run.

use std::collections::{BTreeSet, HashMap};

use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, PathValue};
use crate::runtime::engine::Runtime;
use crate::runtime::result::{ExprResult, IntermediateResult, ResultRow};
use crate::syntax::query::{NearestClause, Query};

use super::{
    bind_distance, effective_source, post_filter, row_node, value_to_vector, NearestSpec, TopK,
    VecCfg, VecSource, VecStats,
};

/// The pattern variables a clause's query vector depends on. Empty means
/// the clause is uncorrelated and the ordinary strategies apply.
pub fn anchor_vars(clause: &NearestClause) -> Vec<String> {
    let mut acc = BTreeSet::new();
    clause.query.referenced_vars(&mut acc);
    // The search variable is not an anchor even if the expression names
    // it: `NEAREST k x.v TO VECTOR(x, 'v')` is every node at distance
    // zero from itself, which is a degenerate query but not a correlation
    // the partitioning can act on.
    acc.remove(&clause.var);
    acc.into_iter().collect()
}

/// Evaluate a correlated clause under the configured strategy.
///
/// Both in-LTJ arms have a correlated form, and they share the mechanism
/// that makes one possible: the anchor is forced above the search
/// variable in the variable elimination order, so by the time a visit
/// reaches that level the anchor is bound and its vector is the ranking's
/// query vector. Everything that was per-query then becomes per-anchor —
/// the vector, the top-`k` threshold, and the corpus stream for a
/// non-local source.
///
/// They differ where they always did. `interleave` consults the ranking
/// inside each visit, so it holds one anchor at a time. `memo` collects
/// every visit's candidates first, filed by anchor, then walks each
/// anchor's ranking once — trading the memory of holding every anchor's
/// candidates for one walk per anchor instead of one per visit.
///
/// `pre` has no correlated form and falls back to partitioning, with the
/// reason recorded.
pub fn run_correlated<G: GraphAccess>(
    rt: &Runtime<'_, G>,
    query: &Query,
    clause: &NearestClause,
    anchors: &[String],
    cfg: &VecCfg,
) -> IntermediateResult {
    let mut stats = VecStats::default();
    // One anchor variable is what the in-LTJ form can pin; an expression
    // over several has no single node whose vector to read.
    if cfg.strategy.is_in_ltj() && anchors.len() == 1 {
        let mode = match cfg.strategy {
            super::Strategy::Memo => crate::runtime::ltj::algorithm::NnMode::Memo,
            _ => crate::runtime::ltj::algorithm::NnMode::Interleave,
        };
        if let Some(ir) = try_in_ltj(rt, query, clause, &anchors[0], cfg, &mut stats, mode) {
            stats.accepted = ir.rows.len() as u64;
            if cfg.debug {
                stats.print();
            }
            rt.set_last_vec_stats(stats);
            return ir;
        }
    }
    // Carry whatever the in-LTJ arm recorded on its way out, so the
    // fallback says which shape it declined rather than repeating the
    // generic "no correlated form".
    run(
        rt,
        query,
        clause,
        anchors,
        cfg,
        stats.fallback_reason.take(),
    )
}

/// The in-LTJ arm, when the shape allows it. `None` when the pattern does
/// not decompose with the search variable below the anchor, or when the
/// anchor is not a plain pattern variable — the caller then partitions.
fn try_in_ltj<G: GraphAccess>(
    rt: &Runtime<'_, G>,
    query: &Query,
    clause: &NearestClause,
    anchor: &str,
    cfg: &VecCfg,
    stats: &mut VecStats,
    mode: crate::runtime::ltj::algorithm::NnMode,
) -> Option<IntermediateResult> {
    let set = rt.graph.vectors(&clause.attr)?;
    if clause.k == 0 {
        return None;
    }
    let spec = NearestSpec {
        k: clause.k as usize,
        mode: clause.mode,
        var: clause.var.clone(),
        attr: clause.attr.clone(),
        // Empty: the vector is read per anchor inside the search.
        q: Vec::new(),
        anchor: Some(anchor.to_string()),
        dist_var: clause.dist_var.clone(),
    };
    super::in_ltj::run(rt, query, &spec, cfg, set, stats, mode)
}

pub fn run<G: GraphAccess>(
    rt: &Runtime<'_, G>,
    query: &Query,
    clause: &NearestClause,
    anchors: &[String],
    cfg: &VecCfg,
    carried_reason: Option<String>,
) -> IntermediateResult {
    let mut stats = VecStats::default();
    let out = eval(rt, query, clause, anchors, cfg, &mut stats, carried_reason);
    stats.accepted = out.rows.len() as u64;
    if cfg.debug {
        stats.print();
    }
    rt.set_last_vec_stats(stats);
    out
}

fn eval<G: GraphAccess>(
    rt: &Runtime<'_, G>,
    query: &Query,
    clause: &NearestClause,
    anchors: &[String],
    cfg: &VecCfg,
    stats: &mut VecStats,
    carried_reason: Option<String>,
) -> IntermediateResult {
    // A missing sidecar is not an error: nothing satisfies "among the k
    // nearest" then, and returning the unfiltered pattern would be
    // silently wrong.
    let set = match rt.graph.vectors(&clause.attr) {
        Some(s) => s,
        None => {
            stats.arm = "none";
            stats.fallback_reason = Some(format!("no vector attribute `{}` loaded", clause.attr));
            return IntermediateResult::new(Vec::new());
        }
    };
    let source = effective_source(cfg.source, set);
    stats.arm = correlated_arm(source);
    stats.fallback_reason = match carried_reason {
        // The in-LTJ arm was tried and declined; its reason is the
        // specific one.
        Some(r) => Some(r),
        None if cfg.strategy == super::Strategy::PreFilter => Some(
            "pre has no correlated form: the anchors are not known until the pattern has run"
                .to_string(),
        ),
        // Reached with an in-LTJ strategy only when the query vector
        // depends on more than one pattern variable, so there is no
        // single node whose vector the search could read.
        None if cfg.strategy.is_in_ltj() => Some(format!(
            "{} needs exactly one anchor variable; this query vector depends on {}",
            cfg.strategy.name(),
            anchors.len()
        )),
        None => None,
    };
    if clause.k == 0 {
        return IntermediateResult::new(Vec::new());
    }

    // One pattern evaluation for every anchor. A LIMIT here would cut in
    // row-arrival order, which has nothing to do with distance.
    let ir = rt.run_match_chain_plain(query, 0);
    stats.pattern_runs += 1;

    // Partition by the anchor binding, remembering first-seen order so
    // the output does not depend on hash iteration order.
    let mut order: Vec<Vec<PathValue>> = Vec::new();
    let mut parts: HashMap<Vec<PathValue>, Vec<ResultRow>> = HashMap::new();
    for row in ir.rows {
        let key: Vec<PathValue> = anchors
            .iter()
            .map(|v| row.assignment.get(v).cloned().unwrap_or(PathValue::Nothing))
            .collect();
        match parts.get_mut(&key) {
            Some(bucket) => bucket.push(row),
            None => {
                order.push(key.clone());
                parts.insert(key, vec![row]);
            }
        }
    }
    stats.anchor_groups = order.len() as u64;

    let mut out: Vec<ResultRow> = Vec::new();
    for key in order {
        if rt.budget().expired() {
            break;
        }
        let rows = match parts.remove(&key) {
            Some(r) => r,
            None => continue,
        };
        // Every row in the partition agrees on the anchor bindings, so
        // the query vector is the same for all of them: evaluating it on
        // the first is evaluating it on the partition.
        let representative = &rows[0];
        let value = match rt.run_expr(&representative.assignment, &clause.query) {
            ExprResult::Success(v) => v,
            // A partition whose anchor carries no vector selects nothing,
            // the same way an unresolvable query vector does globally.
            ExprResult::Failure(_) => continue,
        };
        let q = match value_to_vector(&value) {
            Some(q) => q,
            None => continue,
        };
        if set.validate_query(&q).is_err() {
            continue;
        }

        let spec = NearestSpec {
            k: clause.k as usize,
            mode: clause.mode,
            var: clause.var.clone(),
            attr: clause.attr.clone(),
            q,
            anchor: None,
            dist_var: clause.dist_var.clone(),
        };

        let mut buckets: HashMap<Id, Vec<ResultRow>> = HashMap::new();
        for row in rows {
            if let Some(id) = row_node(&row, &spec.var) {
                buckets.entry(id).or_default().push(row);
            }
        }
        if buckets.is_empty() {
            continue;
        }
        stats.candidates_hashed += buckets.len() as u64;

        let mut sink = TopK::new(spec.k, spec.mode);
        post_filter::rank_buckets(
            set,
            &spec,
            source,
            &mut buckets,
            &mut sink,
            stats,
            rt.budget(),
        );
        stats.rows_buffered += sink.buffered;
        stats.rows_evicted += sink.evicted;
        for (dist, mut row) in sink.drain_sorted() {
            bind_distance(&mut row, &spec, dist);
            out.push(row);
        }
    }

    IntermediateResult::new(out)
}

/// Arm label for the correlated partition-and-rank plan. Kept apart from
/// `arm_label` so a benchmark row can never confuse it with the
/// uncorrelated post-filter it borrows its ranking step from.
fn correlated_arm(source: VecSource) -> &'static str {
    match source {
        VecSource::Hnsw => "correlated+hnsw",
        VecSource::LocalSort => "correlated+localsort",
        VecSource::GlobalSort => "correlated+globalsort",
    }
}
