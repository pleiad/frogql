pub mod existential;
pub mod order_by_alias;
pub mod pushdown;
pub mod unroll_repeat;

use crate::syntax::path_pattern::PathPattern;

/// Run all optimization passes on a parsed pattern.
/// This is the compilation phase — called once, before execution.
///
/// Order:
/// 1. `pushdown` — extract WHERE conjuncts into descriptor constraints
///    (type predicates and value comparisons against literals). Runs
///    first so the unroll pass sees descriptors with their merged
///    constraints, which then duplicate cleanly across each unrolled
///    arm without re-parsing the WHERE per copy.
/// 2. `unroll_repeat` — rewrite `(P){lb,ub}` into a Union of concats
///    when the inner has no named variables and the range is small.
///    Each arm becomes LTJ-decomposable, so the chain that contains
///    the repetition stops falling back to hash-join.
pub fn compile(pattern: PathPattern) -> PathPattern {
    let p = pushdown::optimize(pattern);
    unroll_repeat::optimize(p)
}
