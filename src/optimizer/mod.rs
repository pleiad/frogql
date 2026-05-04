pub mod existential;
pub mod pushdown;

use crate::syntax::path_pattern::PathPattern;

/// Run all optimization passes on a parsed pattern.
/// This is the compilation phase — called once, before execution.
pub fn compile(pattern: PathPattern) -> PathPattern {
    pushdown::optimize(pattern)
}
