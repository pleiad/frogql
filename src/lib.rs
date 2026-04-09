pub mod model;
pub mod typing;
pub mod syntax;
pub mod runtime;
pub mod parser;
pub mod pager;
pub mod store;
pub mod optimizer;

use syntax::path_pattern::PathPattern;
use syntax::query::Query;

/// Compile a GQL path pattern string: parse → optimize → return executable pattern.
pub fn compile(query: &str) -> Result<PathPattern, String> {
    let ast = parser::parse(query)?;
    Ok(optimizer::compile(ast))
}

/// Compile a full GQL query (MATCH ... WHERE ... RETURN).
pub fn compile_query(input: &str) -> Result<Query, String> {
    let q = parser::parse_query(input)?;
    let optimized_pattern = optimizer::compile(q.pattern);
    Ok(Query { pattern: optimized_pattern, ..q })
}
