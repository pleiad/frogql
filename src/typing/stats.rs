//! Lightweight always-on counters for the typechecker's hot primitives.
//!
//! The counters answer "which component dominates a `check_query` call"
//! more precisely than wall time can at the µs scale: an increment is
//! ~1 ns against schema scans that run recursive `is_subtype`/`meet`
//! per entry, so they stay compiled in unconditionally. Thread-local so
//! concurrent embedders never contend or mix streams.
//!
//! Usage (see `src/bin/pattern_typecheck.rs`):
//! `stats::reset()` → run the region → `stats::snapshot()`.

use std::cell::Cell;

/// Counter values accumulated since the last [`reset`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TcStats {
    /// Calls to `VariableType::refine` that hit a scan arm (Node or Edge).
    pub refine_calls: u64,
    /// Scan-arm refine calls answered from the schema's memo cache.
    pub refine_cache_hits: u64,
    /// Total `schema.nodes` entries walked across all refine Node-arm scans.
    pub node_entries_scanned: u64,
    /// Total `schema.edges` entries walked across all refine Edge-arm scans.
    pub edge_entries_scanned: u64,
    /// Calls to `VariableType::refine_to_nodes` (PathType descriptor re-refinement).
    pub refine_to_nodes_calls: u64,
    /// Calls to `TypeEnvironment::meet` (Concat/Join/match-chain).
    pub env_meet_calls: u64,
    /// Calls to `TypeEnvironment::union` (pattern Union arms).
    pub env_union_calls: u64,
    /// Calls to `TypeEnvironment::outer_join` (OPTIONAL MATCH).
    pub env_outer_join_calls: u64,
    /// Calls to `TypeEnvironment::to_group` (Repeat/Questioned wrapping).
    pub env_to_group_calls: u64,
    /// Calls to `PathType::meet` (every Concat, recursing through Unions).
    pub pathtype_meet_calls: u64,
}

const ZERO: TcStats = TcStats {
    refine_calls: 0,
    refine_cache_hits: 0,
    node_entries_scanned: 0,
    edge_entries_scanned: 0,
    refine_to_nodes_calls: 0,
    env_meet_calls: 0,
    env_union_calls: 0,
    env_outer_join_calls: 0,
    env_to_group_calls: 0,
    pathtype_meet_calls: 0,
};

thread_local! {
    static STATS: Cell<TcStats> = const { Cell::new(ZERO) };
}

/// Zero all counters for the current thread.
pub fn reset() {
    STATS.with(|s| s.set(ZERO));
}

/// Read the current counter values for this thread (does not reset).
pub fn snapshot() -> TcStats {
    STATS.with(|s| s.get())
}

#[inline]
fn bump(f: impl FnOnce(&mut TcStats)) {
    STATS.with(|s| {
        let mut v = s.get();
        f(&mut v);
        s.set(v);
    });
}

#[inline]
pub(crate) fn record_refine_node_scan(entries: usize) {
    bump(|s| {
        s.refine_calls += 1;
        s.node_entries_scanned += entries as u64;
    });
}

#[inline]
pub(crate) fn record_refine_edge_scan(entries: usize) {
    bump(|s| {
        s.refine_calls += 1;
        s.edge_entries_scanned += entries as u64;
    });
}

#[inline]
pub(crate) fn record_refine_cache_hit() {
    bump(|s| s.refine_cache_hits += 1);
}

#[inline]
pub(crate) fn record_refine_to_nodes() {
    bump(|s| s.refine_to_nodes_calls += 1);
}

#[inline]
pub(crate) fn record_env_meet() {
    bump(|s| s.env_meet_calls += 1);
}

#[inline]
pub(crate) fn record_env_union() {
    bump(|s| s.env_union_calls += 1);
}

#[inline]
pub(crate) fn record_env_outer_join() {
    bump(|s| s.env_outer_join_calls += 1);
}

#[inline]
pub(crate) fn record_env_to_group() {
    bump(|s| s.env_to_group_calls += 1);
}

#[inline]
pub(crate) fn record_pathtype_meet() {
    bump(|s| s.pathtype_meet_calls += 1);
}
