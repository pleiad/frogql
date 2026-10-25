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
///
/// The switches come from the environment; `compile_with` takes them.
pub fn compile(pattern: PathPattern) -> PathPattern {
    compile_with(pattern, &OptimizerCfg::from_env())
}

/// `compile` with the optimizer's switches given.
pub fn compile_with(pattern: PathPattern, cfg: &OptimizerCfg) -> PathPattern {
    let p = pushdown::optimize(pattern);
    if cfg.unroll_repeat {
        unroll_repeat::optimize(p)
    } else {
        p
    }
}

/// The optimizer's kill switch, carried as a value for the reason
/// `runtime::cfg::RuntimeCfg` carries the runtime's: an environment
/// variable is process-global, so a test toggling one changes every test
/// running beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptimizerCfg {
    /// Unroll a bounded repetition into a union of flat chains
    /// (`FROGQL_DISABLE_REPEAT_UNROLL` clears it).
    pub unroll_repeat: bool,
}

impl Default for OptimizerCfg {
    fn default() -> Self {
        OptimizerCfg {
            unroll_repeat: true,
        }
    }
}

impl OptimizerCfg {
    pub fn from_env() -> OptimizerCfg {
        OptimizerCfg {
            unroll_repeat: std::env::var("FROGQL_DISABLE_REPEAT_UNROLL").is_err(),
        }
    }
}
