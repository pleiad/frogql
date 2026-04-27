pub mod elaborate;
pub mod model;
pub mod optimizer;
pub mod pager;
pub mod parser;
pub mod runtime;
pub mod store;
pub mod syntax;
pub mod typing;

use syntax::path_pattern::PathPattern;
use syntax::query::Query;
use typing::checker::Typechecker;

// =====================================================================
// Structured compile diagnostics
// =====================================================================

/// Phase-tagged compile failure. Lets callers (notably the REPL)
/// distinguish a parse failure from a typecheck failure without
/// inspecting strings.
#[derive(Debug, Clone, PartialEq)]
pub enum CompileError {
    /// Parser rejected the input.
    Parse(String),
    /// Typechecker rejected the input. The vector is `Typechecker.errors`
    /// — every diagnostic the checker accumulated, in encounter order.
    Type(Vec<String>),
}

impl CompileError {
    /// Render the error as a single human-readable line.
    /// Mirrors the format used by [`compile_query`]'s `Err(String)` for
    /// backward compatibility.
    pub fn message(&self) -> String {
        match self {
            CompileError::Parse(s) => format!("Parse error: {s}"),
            CompileError::Type(es) => format!("Type error: {}", es.join("; ")),
        }
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// Successful compile result with typechecker warnings preserved.
#[derive(Debug, Clone)]
pub struct CompileResult {
    pub query: Query,
    /// Non-blocking diagnostics from the typechecker (e.g. typo'd
    /// attribute names, filter expressions whose type isn't `bool`).
    pub warnings: Vec<String>,
}

/// Compile a full GQL query and return both the optimized `Query` and
/// every typechecker warning encountered.
///
/// This is the canonical compile pipeline. Both [`compile_query`] and
/// the REPL build on top of this — there is no second copy of
/// "what compiling means."
pub fn compile_query_with_diagnostics(
    input: &str,
) -> Result<CompileResult, CompileError> {
    let ast = parser::parse_query(input).map_err(CompileError::Parse)?;
    let q = elaborate::elaborate_query(ast);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    if !r.ok {
        return Err(CompileError::Type(tc.errors));
    }
    let warnings = tc.warnings;
    let optimized_pattern = optimizer::compile(q.pattern);
    Ok(CompileResult {
        query: Query { pattern: optimized_pattern, ..q },
        warnings,
    })
}

// =====================================================================
// Backward-compatible entry points
// =====================================================================

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
/// Thin wrapper over [`compile_query_with_diagnostics`] that flattens
/// the result for callers that want a plain `Result<Query, String>`.
/// Use [`compile_query_unchecked`] to skip typechecking entirely.
pub fn compile_query(input: &str) -> Result<Query, String> {
    compile_query_with_diagnostics(input)
        .map(|r| r.query)
        .map_err(|e| e.message())
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
    Ok(Query {
        pattern: optimized_pattern,
        ..q
    })
}
