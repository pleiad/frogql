//! Strategy 2: drive the neighbour stream from inside the join.
//!
//! The search variable is placed at a chosen level of the variable
//! elimination order. Each time the backtracking search reaches that
//! level it materialises the candidate set for the variable — already
//! conditioned on the partial binding of everything above it — hashes
//! it, then walks the neighbour stream nearest-first and descends only
//! into candidates that are hits.
//!
//! Not yet implemented; the arm reports a fallback and the caller
//! degrades to post-filtering, so a query is answered correctly either
//! way and the benchmark records which arm actually ran.

use crate::model::graph_access::GraphAccess;
use crate::runtime::engine::Runtime;
use crate::runtime::result::IntermediateResult;
use crate::syntax::query::Query;
use crate::vector::store::VectorSet;

use super::{NearestSpec, VecCfg, VecStats};

pub fn run<G: GraphAccess>(
    _rt: &Runtime<'_, G>,
    _query: &Query,
    _spec: &NearestSpec,
    _cfg: &VecCfg,
    _set: &VectorSet,
    stats: &mut VecStats,
) -> Option<IntermediateResult> {
    stats.fallback_reason = Some("in-LTJ interleaving is not implemented yet".to_string());
    None
}
