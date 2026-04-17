pub mod model;
pub mod typing;
pub mod syntax;
pub mod runtime;
pub mod parser;
pub mod pager;
pub mod store;
pub mod optimizer;
pub mod elaborate;

use syntax::path_pattern::PathPattern;
use syntax::query::Query;

/// Compile a GQL path pattern string: parse → elaborate → optimize.
pub fn compile(query: &str) -> Result<PathPattern, String> {
    let ast = parser::parse(query)?;
    // Elaboration runs on whole queries; wrap the bare pattern for the pass.
    let q = Query::pattern_only(ast);
    let q = elaborate::elaborate_query(q);
    Ok(optimizer::compile(q.pattern))
}

/// Compile a full GQL query (MATCH ... WHERE ... RETURN): parse → elaborate → optimize.
pub fn compile_query(input: &str) -> Result<Query, String> {
    let q = parser::parse_query(input)?;
    let q = elaborate::elaborate_query(q);
    let optimized_pattern = optimizer::compile(q.pattern);
    Ok(Query { pattern: optimized_pattern, ..q })
}
