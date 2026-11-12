//! The engine's kill switches, as a value a caller sets.
//!
//! Every optimization keeps a switch that turns it off, so a differential
//! test can pin "optimized ≡ baseline". They used to be read from the
//! environment at the point of use, which made each one process-global: a
//! test toggling a switch changed the code path of every other test running
//! beside it, so each file that toggled one had to be a test binary of its
//! own — and macOS charges seconds per freshly linked binary before its
//! first test runs. `RuntimeCfg` is the same switches carried by a
//! `Runtime`. `from_env` reads the variables that set them, so the CLI and
//! the bindings behave as before; a test sets a field instead.
//!
//! What stays in the environment is what a `Runtime` does not decide: the
//! optimizer's unrolling (`optimizer::OptimizerCfg`), the store's open-time
//! choices (`FROGQL_DISABLE_AUTO_INDEXES`, `FROGQL_AUTO_INDEX_KINDS`,
//! `FROGQL_DISABLE_OVERLAY_INDEX`, `FROGQL_DISABLE_VECTORS` — the last also
//! reachable as `VectorStore::disable`), the vector-search knobs (`VecCfg`,
//! already per-`Runtime`), and the tracing and debug prints.

use crate::runtime::ltj::triple_index::TripleIndex;

/// Which ORDER BY strategy runs (`FROGQL_ORDERBY_FORCE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderByForce {
    /// The heuristic: the btree-driven paths when they apply, else top-k
    /// selection when `LIMIT` is small against the input, else pdqsort.
    /// The btree walk (`try_btree_ltj_real`) is declined for a pattern
    /// anchored by an indexed equality; see `Runtime::pattern_is_anchored`.
    #[default]
    Auto,
    /// The heuristic, except that the btree walk runs whenever its
    /// preconditions hold, anchored pattern or not: the behaviour before
    /// the anchor rule, kept so the two can be compared.
    Btree,
    /// A full unstable sort.
    Pdqsort,
    /// Top-k selection whenever there is a top-k to take.
    Topk,
    /// A full stable sort. Benchmark-only: GQL leaves peer order
    /// implementation-dependent, so stability buys nothing by default.
    Driftsort,
}

impl OrderByForce {
    /// Any other spelling (`auto`, …) is the heuristic.
    pub fn parse(s: &str) -> OrderByForce {
        match s {
            "pdqsort" => OrderByForce::Pdqsort,
            "topk" => OrderByForce::Topk,
            "driftsort" => OrderByForce::Driftsort,
            "btree" | "btree-ltj" | "btree-ltj-real" => OrderByForce::Btree,
            _ => OrderByForce::Auto,
        }
    }

    /// Whether the btree-driven ORDER BY paths may run. Forcing a sort
    /// strategy turns them off.
    pub fn allows_btree(self) -> bool {
        matches!(self, OrderByForce::Auto | OrderByForce::Btree)
    }

    /// Whether the btree walk declines an anchored pattern.
    pub fn declines_anchored(self) -> bool {
        self == OrderByForce::Auto
    }
}

/// The switches one `Runtime` consults. `Default` is every optimization on,
/// which is what an empty environment selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeCfg {
    /// Build the LTJ index as succinct tries rather than sorted arrays
    /// (`FROGQL_LTJ_REPR=compact|array`, legacy `FROGQL_LTJ_COMPACT=0`).
    /// Read when the index is built, so set it before the first query.
    pub ltj_compact: bool,
    /// Ignore `<db>.ltj` and rebuild the index from the graph
    /// (`FROGQL_LTJ_SOURCE=build`).
    pub ltj_force_build: bool,
    /// Write `<db>.ltj` after building the index (`FROGQL_LTJ_PERSIST=0`
    /// clears it).
    pub ltj_persist: bool,
    /// Keep the index across DML with a delta beside it, instead of dropping
    /// it (`FROGQL_DISABLE_LTJ_DELTA=1` clears it).
    pub ltj_delta: bool,
    /// Run any-direction (`-[e]-`) patterns through the mirrored index
    /// (`FROGQL_DISABLE_ANYDIR_LTJ` clears it). Consult it before
    /// materialising the mirror, not inside the callee: Rust evaluates
    /// arguments eagerly, so a check inside `try_ltj_mixed` alone still pays
    /// the whole mirror build (~232 MiB / ~1 s at SF0.1) before discarding
    /// the path.
    pub anydir_ltj: bool,
    /// Re-pick the variable order per binding, issue #101 (`FROGQL_VEO=simple`
    /// clears it). Promoted from opt-in on the LDBC evidence: the full sweep
    /// returns identical results under either order, IC4 runs 123.6 ms →
    /// 0.7 ms and IC11 1.7 → 1.0, most ICs are flat, and IC9 pays 10–15%.
    /// One dataset is the whole of the evidence; the RDF corpus is not
    /// measured.
    pub adaptive_veo: bool,
    /// Constant-fold indexed equalities and ranges before the LTJ search
    /// (`FROGQL_DISABLE_INDEX_FOLD` clears it).
    pub index_fold: bool,
    /// Expand a bounded edge repetition from the rows on its left
    /// (`FROGQL_DISABLE_SEEDED_REPEAT` clears it).
    pub seeded_repeat: bool,
    /// The node-dominated BFS for single-edge shortest paths
    /// (`FROGQL_DISABLE_SHORTEST_BFS` clears it).
    pub shortest_bfs: bool,
    /// Restrict a later MATCH clause to the nodes earlier clauses bound
    /// (`FROGQL_DISABLE_MATCH_PUSHDOWN` clears it).
    pub match_pushdown: bool,
    /// Evaluate OPTIONAL MATCH per outer row with its shared variables
    /// pinned (`FROGQL_DISABLE_OPTIONAL_PUSHDOWN` clears it).
    pub optional_pushdown: bool,
    /// Probe a correlated EXISTS with its correlation variables pinned
    /// (`FROGQL_DISABLE_EXISTS_PIN` clears it).
    pub exists_pin: bool,
    /// Pin or restrict a correlated `VALUE { … }` body
    /// (`FROGQL_DISABLE_VALUE_SUBQUERY_PIN` clears it).
    pub value_subquery_pin: bool,
    /// `FROGQL_ORDERBY_FORCE`.
    pub order_by: OrderByForce,
}

impl Default for RuntimeCfg {
    fn default() -> Self {
        RuntimeCfg {
            ltj_compact: true,
            ltj_force_build: false,
            ltj_persist: true,
            ltj_delta: true,
            anydir_ltj: true,
            adaptive_veo: true,
            index_fold: true,
            seeded_repeat: true,
            shortest_bfs: true,
            match_pushdown: true,
            optional_pushdown: true,
            exists_pin: true,
            value_subquery_pin: true,
            order_by: OrderByForce::Auto,
        }
    }
}

impl RuntimeCfg {
    /// The switches the `FROGQL_*` variables select. Each keeps the spelling
    /// it had when it was read at the point of use: the `DISABLE` switches
    /// fire on any value, `FROGQL_DISABLE_LTJ_DELTA` only on `1`.
    pub fn from_env() -> RuntimeCfg {
        let set = |name: &str| std::env::var(name).is_ok();
        RuntimeCfg {
            ltj_compact: TripleIndex::compact_selected(),
            ltj_force_build: std::env::var("FROGQL_LTJ_SOURCE").as_deref() == Ok("build"),
            ltj_persist: std::env::var("FROGQL_LTJ_PERSIST").as_deref() != Ok("0"),
            ltj_delta: !crate::runtime::ltj::delta::delta_disabled(),
            anydir_ltj: !set("FROGQL_DISABLE_ANYDIR_LTJ"),
            adaptive_veo: !std::env::var("FROGQL_VEO")
                .is_ok_and(|v| v.eq_ignore_ascii_case("simple")),
            index_fold: !set("FROGQL_DISABLE_INDEX_FOLD"),
            seeded_repeat: !set("FROGQL_DISABLE_SEEDED_REPEAT"),
            shortest_bfs: !set("FROGQL_DISABLE_SHORTEST_BFS"),
            match_pushdown: !set("FROGQL_DISABLE_MATCH_PUSHDOWN"),
            optional_pushdown: !set("FROGQL_DISABLE_OPTIONAL_PUSHDOWN"),
            exists_pin: !set("FROGQL_DISABLE_EXISTS_PIN"),
            value_subquery_pin: !set("FROGQL_DISABLE_VALUE_SUBQUERY_PIN"),
            order_by: std::env::var("FROGQL_ORDERBY_FORCE")
                .map_or(OrderByForce::Auto, |s| OrderByForce::parse(&s)),
        }
    }
}
