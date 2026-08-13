//! Vector-search evaluation strategies.
//!
//! The question is "which nodes satisfy this graph pattern **and** are
//! among the `k` nearest to a query vector". Three answers are
//! implemented, and the point of the module is that they are
//! interchangeable:
//!
//! - **post-filter** — run the pattern, then rank what it produced.
//! - **pre-filter** — walk the neighbour stream, re-running the pattern
//!   with the search variable pinned to each candidate.
//! - **in-LTJ** — interleave, driving the neighbour stream from inside
//!   the join's backtracking search at a chosen variable-elimination
//!   level.
//!
//! All three go in through `run_nearest` and come out as an
//! `IntermediateResult`, so everything downstream in `run_query`
//! (projection, DISTINCT, ORDER BY, LIMIT) is identical across arms.
//! That is what makes a latency comparison between them mean anything.

pub mod in_ltj;
pub mod post_filter;
pub mod pre_filter;
pub mod topk;

use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, Value};
use crate::runtime::engine::Runtime;
use crate::runtime::result::ExprResult;
use crate::runtime::result::{IntermediateResult, ResultRow};
use crate::syntax::query::{KMode, NearestClause, Query};
use crate::vector::store::VectorSet;

pub use topk::TopK;

/// A `NearestClause` with its query vector already evaluated.
///
/// Resolution happens once, before any strategy runs, so the cost of
/// materialising the vector never lands inside the part being measured
/// and every arm starts from identical inputs.
#[derive(Debug, Clone)]
pub struct NearestSpec {
    pub k: usize,
    pub mode: KMode,
    pub var: String,
    pub attr: String,
    pub q: Vec<f32>,
    pub dist_var: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    PostFilter,
    PreFilter,
    InLtj,
}

impl Strategy {
    pub fn parse(s: &str) -> Option<Strategy> {
        match s.to_ascii_lowercase().as_str() {
            "post" | "postfilter" | "post-filter" => Some(Strategy::PostFilter),
            "pre" | "prefilter" | "pre-filter" => Some(Strategy::PreFilter),
            "inltj" | "in-ltj" | "ltj" => Some(Strategy::InLtj),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Strategy::PostFilter => "post",
            Strategy::PreFilter => "pre",
            Strategy::InLtj => "inltj",
        }
    }
}

/// Where a strategy gets its nearest-first stream from.
///
/// This axis is deliberately three-valued rather than an
/// index/no-index boolean, because "no index" hides two genuinely
/// different algorithms with different cost models:
///
/// - `GlobalSort` ranks the **whole attribute** once, then every visit
///   to the search level re-scans that global list from the top testing
///   membership in the level's candidate set. `O(n log n)` up front,
///   and how deep each visit scans depends on where the k-th answer
///   sits globally — not on how many candidates the visit has.
/// - `LocalSort` ranks **only the candidates of the current visit**.
///   `O(|C| log |C|)` per visit, no global structure, and it never
///   looks at a node outside the level.
///
/// `Hnsw` shares `GlobalSort`'s walk exactly — same re-scan, same
/// membership tests — and differs only in that it materialises the
/// ranking lazily instead of sorting everything up front. That is why
/// they are two values and not one: the saving is in *building* the
/// stream, not in walking it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VecSource {
    /// Incremental HNSW cursor. Approximate.
    Hnsw,
    /// Rank the current visit's candidates only. Exact. Meaningful for
    /// the in-LTJ strategy, where a "visit" has its own candidate set;
    /// for post-filter it ranks the whole match, and for pre-filter,
    /// which has no level, it falls back to `GlobalSort`.
    LocalSort,
    /// Rank the whole attribute once, then test membership. Exact.
    GlobalSort,
}

impl VecSource {
    pub fn parse(s: &str) -> Option<VecSource> {
        match s.to_ascii_lowercase().as_str() {
            "hnsw" | "index" => Some(VecSource::Hnsw),
            "localsort" | "local" => Some(VecSource::LocalSort),
            "globalsort" | "global" | "brute" => Some(VecSource::GlobalSort),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            VecSource::Hnsw => "hnsw",
            VecSource::LocalSort => "localsort",
            VecSource::GlobalSort => "globalsort",
        }
    }

    /// Is this an exact ranking? Only the exact sources are pinned to
    /// mutual equality by the differential test; HNSW's recall is a
    /// measurement, not an invariant.
    pub fn is_exact(self) -> bool {
        !matches!(self, VecSource::Hnsw)
    }
}

/// Which strategy to run and how. Read from the environment by default
/// and overridable programmatically by the benchmark driver.
#[derive(Debug, Clone)]
pub struct VecCfg {
    pub strategy: Strategy,
    /// Where the nearest-first stream comes from.
    pub source: VecSource,
    /// Requested position of the search variable in the variable
    /// elimination order. In-LTJ only; clamped to a legal level.
    pub level: usize,
    /// Slack on the threshold cut, as a relative fraction. An
    /// approximate cursor's distances are not strictly non-decreasing,
    /// so cutting at exactly the threshold can drop a neighbour the
    /// stream was about to reveal. Zero is the exact rule.
    pub tau_eps: f32,
    /// Print the arm taken and its counters to stderr.
    pub debug: bool,
}

impl Default for VecCfg {
    fn default() -> Self {
        VecCfg {
            strategy: Strategy::PostFilter,
            source: VecSource::Hnsw,
            level: 0,
            tau_eps: 0.0,
            debug: false,
        }
    }
}

impl VecCfg {
    /// `FROGQL_VEC_STRATEGY=post|pre|inltj`,
    /// `FROGQL_VEC_SOURCE=hnsw|localsort|globalsort`,
    /// `FROGQL_VEC_LEVEL=<n>`, `FROGQL_VEC_TAU_EPS=<f>`, `FROGQL_DEBUG_VEC`.
    pub fn from_env() -> VecCfg {
        let mut cfg = VecCfg::default();
        if let Ok(s) = std::env::var("FROGQL_VEC_STRATEGY") {
            if let Some(v) = Strategy::parse(&s) {
                cfg.strategy = v;
            }
        }
        if let Ok(s) = std::env::var("FROGQL_VEC_SOURCE") {
            if let Some(v) = VecSource::parse(&s) {
                cfg.source = v;
            }
        }
        if let Ok(s) = std::env::var("FROGQL_VEC_LEVEL") {
            if let Ok(v) = s.parse() {
                cfg.level = v;
            }
        }
        if let Ok(s) = std::env::var("FROGQL_VEC_TAU_EPS") {
            if let Ok(v) = s.parse() {
                cfg.tau_eps = v;
            }
        }
        cfg.debug = std::env::var("FROGQL_DEBUG_VEC").is_ok();
        cfg
    }
}

/// Instrumentation. The headline number of the whole experiment is
/// `nn_pops` per accepted result: with a selective pattern, the in-LTJ
/// and pre-filter arms walk a proximity graph built over the *whole*
/// corpus, so reaching a candidate that also satisfies the pattern can
/// cost a large fraction of layer 0. Post-filter degrades gracefully
/// exactly where those two blow up.
#[derive(Debug, Clone, Default)]
pub struct VecStats {
    /// Which arm actually ran. Never infer it from the config: a strategy
    /// that hits a precondition miss falls back, and a benchmark that
    /// reports the requested arm rather than the executed one lies.
    pub arm: &'static str,
    /// Reason the requested arm was not used, if it was not.
    pub fallback_reason: Option<String>,
    /// Neighbours consumed from the stream.
    pub nn_pops: u64,
    /// Nodes the metric index expanded (HNSW), or rows it scanned (brute).
    pub nn_expanded: u64,
    pub prefix_replays: u64,
    pub prefix_extends: u64,
    /// Times the in-LTJ search reached the search variable's level.
    pub ltj_visits: u64,
    /// Candidates hashed across all those visits.
    pub candidates_hashed: u64,
    /// Whole-pattern evaluations (pre-filter: one per candidate).
    pub pattern_runs: u64,
    pub rows_buffered: u64,
    pub rows_evicted: u64,
    /// Results produced.
    pub accepted: u64,
}

impl VecStats {
    pub fn print(&self) {
        eprintln!(
            "vsearch arm={} accepted={} nn_pops={} nn_expanded={} pattern_runs={} \
             ltj_visits={} candidates={} replays={} extends={} buffered={} evicted={}{}",
            self.arm,
            self.accepted,
            self.nn_pops,
            self.nn_expanded,
            self.pattern_runs,
            self.ltj_visits,
            self.candidates_hashed,
            self.prefix_replays,
            self.prefix_extends,
            self.rows_buffered,
            self.rows_evicted,
            match &self.fallback_reason {
                Some(r) => format!(" fallback={r}"),
                None => String::new(),
            }
        );
    }
}

/// Resolve a parsed clause into a spec, evaluating the query vector once.
pub fn resolve_spec<G: GraphAccess>(
    rt: &Runtime<'_, G>,
    clause: &NearestClause,
) -> Result<NearestSpec, String> {
    let empty = crate::runtime::assignment::Assignment::new();
    let value = match rt.run_expr(&empty, &clause.query) {
        ExprResult::Success(v) => v,
        ExprResult::Failure(e) => {
            return Err(format!("NEAREST query vector could not be evaluated: {e}"))
        }
    };
    let q = value_to_vector(&value).ok_or_else(|| {
        format!(
            "NEAREST ... TO must be a vector (a list of numbers), got {}",
            short_value(&value)
        )
    })?;
    Ok(NearestSpec {
        k: clause.k as usize,
        mode: clause.mode,
        var: clause.var.clone(),
        attr: clause.attr.clone(),
        q,
        dist_var: clause.dist_var.clone(),
    })
}

fn value_to_vector(v: &Value) -> Option<Vec<f32>> {
    match v {
        Value::List(items) => items
            .iter()
            .map(|x| match x {
                Value::Float(f) => Some(*f as f32),
                Value::Int(n) => Some(*n as f32),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn short_value(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::List(_) => "a list containing non-numbers".to_string(),
        other => format!("{other:?}"),
    }
}

/// The single entry point. Every strategy returns an
/// `IntermediateResult` whose rows are ordered nearest-first and carry
/// the distance binding when the clause asked for one.
pub fn run_nearest<G: GraphAccess>(
    rt: &Runtime<'_, G>,
    query: &Query,
    spec: &NearestSpec,
    cfg: &VecCfg,
) -> IntermediateResult {
    let mut stats = VecStats::default();
    let out = dispatch(rt, query, spec, cfg, &mut stats);
    stats.accepted = out.rows.len() as u64;
    if cfg.debug {
        stats.print();
    }
    rt.set_last_vec_stats(stats);
    out
}

fn dispatch<G: GraphAccess>(
    rt: &Runtime<'_, G>,
    query: &Query,
    spec: &NearestSpec,
    cfg: &VecCfg,
    stats: &mut VecStats,
) -> IntermediateResult {
    // A missing attribute is not an error: the sidecar may be absent, or
    // suspended because the session has pending DML. Nothing satisfies
    // "among the k nearest" then, so the answer is empty rather than the
    // unfiltered pattern, which would be silently wrong.
    let set = match rt.graph.vectors(&spec.attr) {
        Some(s) => s,
        None => {
            stats.arm = "none";
            stats.fallback_reason = Some(format!("no vector attribute `{}` loaded", spec.attr));
            return IntermediateResult::new(Vec::new());
        }
    };
    if let Err(e) = set.validate_query(&spec.q) {
        stats.arm = "none";
        stats.fallback_reason = Some(e);
        return IntermediateResult::new(Vec::new());
    }
    if spec.k == 0 {
        stats.arm = "none";
        return IntermediateResult::new(Vec::new());
    }

    // The interleaving arms need one decomposable pattern. OPTIONAL and
    // §16.6-prefixed matches are evaluated by their own machinery, which
    // has no level to hook into, so they degrade rather than mislead.
    let interleavable = !query.has_any_optional() && !query.has_any_selected();

    // Each interleaving arm returns None when it meets a shape it cannot
    // hook into, having recorded why. Post-filter answers every shape, so
    // it is the fallback in all of those cases: the query is answered
    // correctly either way, and `stats.arm` records what actually ran.
    let attempted = match cfg.strategy {
        Strategy::PostFilter => None,
        _ if !interleavable => {
            stats.fallback_reason = Some(format!(
                "{} needs a single non-optional, non-selective match",
                cfg.strategy.name()
            ));
            None
        }
        Strategy::PreFilter => pre_filter::run(rt, query, spec, cfg, set, stats),
        Strategy::InLtj => in_ltj::run(rt, query, spec, cfg, set, stats),
    };
    match attempted {
        Some(ir) => ir,
        None => post_filter::run(rt, query, spec, cfg, set, stats),
    }
}

/// The node id `spec.var` is bound to in `row`, if it is a node.
pub(crate) fn row_node(row: &ResultRow, var: &str) -> Option<Id> {
    match row.assignment.get(var) {
        Some(crate::model::value::PathValue::Node(id)) => Some(*id),
        _ => None,
    }
}

/// Attach the distance binding, when the clause asked for one.
pub(crate) fn bind_distance(row: &mut ResultRow, spec: &NearestSpec, dist: f32) {
    if let Some(d) = &spec.dist_var {
        row.assignment
            .set_scalar(d.clone(), Value::Float(dist as f64));
    }
}

/// Turn the sink's contents into the runtime's row shape.
pub(crate) fn finish(sink: TopK, spec: &NearestSpec, stats: &mut VecStats) -> IntermediateResult {
    stats.rows_buffered = sink.buffered;
    stats.rows_evicted = sink.evicted;
    let rows = sink
        .drain_sorted()
        .into_iter()
        .map(|(dist, mut row)| {
            bind_distance(&mut row, spec, dist);
            row
        })
        .collect();
    IntermediateResult::new(rows)
}

/// The source that can actually be served.
///
/// Asking for `Hnsw` on an attribute with no graph built cannot be
/// honoured; the exact global ranking is the closest thing, and it is
/// what gets reported. Silently keeping the requested label would make
/// a benchmark row claim a walk that never happened.
pub(crate) fn effective_source(requested: VecSource, set: &VectorSet) -> VecSource {
    match requested {
        VecSource::Hnsw if !set.has_index() => VecSource::GlobalSort,
        other => other,
    }
}

/// The arm label for `stats.arm`: strategy plus the source that actually
/// ran. Never derived from the request alone — a strategy that degraded
/// must not be reported under the name of the one that was asked for.
pub(crate) fn arm_label(strategy: Strategy, source: VecSource) -> &'static str {
    match (strategy, source) {
        (Strategy::PostFilter, VecSource::Hnsw) => "post+hnsw",
        (Strategy::PostFilter, VecSource::LocalSort) => "post+localsort",
        (Strategy::PostFilter, VecSource::GlobalSort) => "post+globalsort",
        (Strategy::PreFilter, VecSource::Hnsw) => "pre+hnsw",
        (Strategy::PreFilter, VecSource::LocalSort) => "pre+localsort",
        (Strategy::PreFilter, VecSource::GlobalSort) => "pre+globalsort",
        (Strategy::InLtj, VecSource::Hnsw) => "inltj+hnsw",
        (Strategy::InLtj, VecSource::LocalSort) => "inltj+localsort",
        (Strategy::InLtj, VecSource::GlobalSort) => "inltj+globalsort",
    }
}
