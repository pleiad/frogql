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
use typing::checker::Typechecker;

/// Compile a GQL path pattern string: parse → elaborate → typecheck → optimize.
///
/// Typechecking uses the permissive `Schema::star()` and rejects the query if
/// the checker reports errors (unbound variables, irreconcilable contexts).
/// Use [`compile_unchecked`] to skip typechecking.
pub fn compile(query: &str) -> Result<PathPattern, String> {
    let ast = parser::parse(query)?;
    // Elaboration runs on whole queries; wrap the bare pattern for the pass.
    let q = Query::pattern_only(ast);
    let q = elaborate::elaborate_query(q);
    let mut tc = Typechecker::untyped();
    let r = tc.check_pattern(&q.pattern);
    if !r.ok {
        return Err(tc.errors.join("; "));
    }
    Ok(optimizer::compile(q.pattern))
}

/// Compile a full GQL query (MATCH ... WHERE ... RETURN):
/// parse → elaborate → typecheck → optimize.
///
/// Typechecking uses the permissive `Schema::star()` and rejects the query
/// if the checker reports errors. Use [`compile_query_unchecked`] to skip.
pub fn compile_query(input: &str) -> Result<Query, String> {
    let q = parser::parse_query(input)?;
    let q = elaborate::elaborate_query(q);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    if !r.ok {
        return Err(tc.errors.join("; "));
    }
    let optimized_pattern = optimizer::compile(q.pattern);
    Ok(Query { pattern: optimized_pattern, ..q })
}

/// Compile a path pattern without typechecking. Same plan as
/// [`compile`] would have produced before the typechecker landed.
pub fn compile_unchecked(query: &str) -> Result<PathPattern, String> {
    let ast = parser::parse(query)?;
    let q = Query::pattern_only(ast);
    let q = elaborate::elaborate_query(q);
    Ok(optimizer::compile(q.pattern))
}

/// Compile a full GQL query without typechecking. Same plan as
/// [`compile_query`] would have produced before the typechecker landed.
pub fn compile_query_unchecked(input: &str) -> Result<Query, String> {
    let q = parser::parse_query(input)?;
    let q = elaborate::elaborate_query(q);
    let optimized_pattern = optimizer::compile(q.pattern);
    Ok(Query { pattern: optimized_pattern, ..q })
}
