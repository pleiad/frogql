pub mod model;
pub mod typing;
pub mod syntax;
pub mod runtime;
pub mod parser;
pub mod pager;
pub mod store;
pub mod optimizer;

use syntax::path_pattern::PathPattern;

/// Compile a GQL query string: parse → optimize → return executable pattern.
/// This is the entry point for query compilation.
pub fn compile(query: &str) -> Result<PathPattern, String> {
    let ast = parser::parse(query)?;
    Ok(optimizer::compile(ast))
}
