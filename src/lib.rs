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
use syntax::query::{MatchStatement, Query};
use typing::checker::Typechecker;
use typing::variable_type::Schema;

/// Optimize a Query. Two cases:
///
/// - All-Simple: collapse the chain into one `PathPattern::Join`, optimize
///   it, and store as a single `MatchStatement::Simple`. Sound because
///   §14.4 GR 1 makes MATCH a natural join, which is exactly
///   `PathPattern::Join`'s semantics.
///
/// - Has-Optional: collapse is unsound (OPTIONAL is a left-outer-join).
///   Optimize each match's pattern in place and preserve the chain
///   structure for the typechecker and runtime to walk per match.
///
/// After the per-pattern passes, the existential folder walks every
/// `Expr` (WHERE / GROUP BY / RETURN, recursive into nested EXISTS
/// bodies) and rewrites empty bodies to `false` / `true` literals.
fn optimize_query(q: Query, schema: &Schema) -> Query {
    // Collapsing the chain into a single Join is sound only when no clause
    // contains a path pattern prefix: a `Selected` (selective/restrictive)
    // pattern is evaluated in isolation (§16.6 NOTE 233), which a collapsed
    // Join would erase. Treat such clauses like OPTIONAL and optimize each
    // pattern in place, preserving the chain.
    let mut q = if !q.has_any_optional() && !q.has_any_selected() {
        let pattern = optimizer::compile(q.collapsed_pattern());
        Query {
            matches: vec![MatchStatement::Simple { pattern }],
            ..q
        }
    } else {
        let matches = q
            .matches
            .into_iter()
            .map(|m| match m {
                MatchStatement::Simple { pattern } => MatchStatement::Simple {
                    pattern: optimizer::compile(pattern),
                },
                MatchStatement::Optional { pattern } => MatchStatement::Optional {
                    pattern: optimizer::compile(pattern),
                },
            })
            .collect();
        Query { matches, ..q }
    };
    optimizer::existential::fold_empty_existentials(&mut q, schema);
    optimizer::order_by_alias::optimize(&mut q);
    q
}

/// Phase-tagged compile failure.
#[derive(Debug, Clone, PartialEq)]
pub enum CompileError {
    Parse(String),
    Type(Vec<String>),
}

impl CompileError {
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

/// Successful compile. `guaranteed_empty` is the typechecker's
/// emptiness verdict (§10); when true, callers may skip the runtime.
#[derive(Debug, Clone)]
pub struct CompileResult {
    pub query: Query,
    pub warnings: Vec<String>,
    pub guaranteed_empty: bool,
}

/// Canonical compile pipeline. `compile_query` and the REPL both use this.
/// The typechecker uses `Schema::star()`; callers with a custom catalog
/// schema should use [`compile_query_with_diagnostics_with`].
pub fn compile_query_with_diagnostics(input: &str) -> Result<CompileResult, CompileError> {
    compile_query_with_diagnostics_with(&Schema::star(), input)
}

/// Same as [`compile_query_with_diagnostics`] but typechecks against the
/// supplied schema. Used by the REPL / Python bindings to honor the
/// active GRAPH TYPE.
pub fn compile_query_with_diagnostics_with(
    schema: &Schema,
    input: &str,
) -> Result<CompileResult, CompileError> {
    let ast = parser::parse_query(input).map_err(CompileError::Parse)?;
    let q = elaborate::elaborate_query(ast);
    let mut tc = Typechecker::new(schema.clone());
    let r = tc.check_query(&q);
    if !r.ok {
        return Err(CompileError::Type(tc.errors));
    }
    let warnings = tc.warnings;
    let guaranteed_empty = r.empty;
    Ok(CompileResult {
        query: optimize_query(q, schema),
        warnings,
        guaranteed_empty,
    })
}

/// Compile a GQL path pattern string: parse → elaborate → typecheck → optimize.
///
/// Typechecking uses the permissive `Schema::star()` and rejects the query if
/// the checker reports errors (unbound variables, irreconcilable contexts).
/// Use [`compile_unchecked`] to skip typechecking.
pub fn compile(query: &str) -> Result<PathPattern, String> {
    let ast = parser::parse(query)?;
    let q = Query::pattern_only(ast);
    let q = elaborate::elaborate_query(q);
    let mut tc = Typechecker::untyped();
    let r = tc.check_query(&q);
    if !r.ok {
        return Err(tc.errors.join("; "));
    }
    Ok(optimize_query(q, &Schema::star()).collapsed_pattern())
}

/// Compile a full GQL query: parse → elaborate → typecheck → optimize.
/// Thin wrapper over [`compile_query_with_diagnostics`].
/// Use [`compile_query_unchecked`] to skip typechecking.
pub fn compile_query(input: &str) -> Result<Query, String> {
    compile_query_with_diagnostics(input)
        .map(|r| r.query)
        .map_err(|e| e.message())
}

/// Compile a full GQL query against an explicit schema. Identical to
/// [`compile_query`] but consults the supplied schema instead of the
/// permissive default. Used by the REPL after a `USE GRAPH TYPE` to
/// honor the active type.
pub fn compile_query_with(schema: &Schema, input: &str) -> Result<Query, String> {
    compile_query_with_diagnostics_with(schema, input)
        .map(|r| r.query)
        .map_err(|e| e.message())
}

/// Compile a path pattern without typechecking. Same plan as
/// [`compile`] would have produced before the typechecker landed.
pub fn compile_unchecked(query: &str) -> Result<PathPattern, String> {
    let ast = parser::parse(query)?;
    let q = Query::pattern_only(ast);
    let q = elaborate::elaborate_query(q);
    Ok(optimize_query(q, &Schema::star()).collapsed_pattern())
}

/// Compile a full GQL query without typechecking. Same plan as
/// [`compile_query`] would have produced before the typechecker landed.
pub fn compile_query_unchecked(input: &str) -> Result<Query, String> {
    let q = parser::parse_query(input)?;
    let q = elaborate::elaborate_query(q);
    Ok(optimize_query(q, &Schema::star()))
}
