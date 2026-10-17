pub mod connection;
pub mod elaborate;
pub mod model;
pub mod optimizer;
pub mod pager;
pub mod parser;
pub mod runtime;
pub mod store;
pub mod syntax;
pub mod typing;
pub mod vector;

pub use connection::{Connection, Element, Error, Modified, Outcome, Row, Rows};
pub use model::value::Value;

use std::collections::HashSet;

use syntax::path_pattern::PathPattern;
use syntax::query::{MatchStatement, Query};
use typing::checker::Typechecker;
use typing::variable_type::Schema;

/// Optimize the query while preserving OPTIONAL and §16.6 prefix boundaries.
fn optimize_query(q: Query, schema: &Schema) -> Query {
    // Selected patterns are evaluated in isolation; do not collapse across them.
    let mut q = if !q.has_any_optional() && !q.has_any_selected() {
        let pattern = optimizer::compile(q.collapsed_pattern());
        Query {
            matches: vec![MatchStatement::Simple { pattern }],
            ..q
        }
    } else {
        Query {
            matches: collapse_simple_runs(q.matches),
            ..q
        }
    };
    optimizer::existential::fold_empty_existentials(&mut q, schema);
    optimizer::order_by_alias::optimize(&mut q);
    q
}

/// Compile a mixed match chain clause by clause, first collapsing every
/// maximal run of consecutive plain `MATCH` clauses into one comma-join.
///
/// Inside such a run each clause natural-joins the working table (ISO
/// §14.4 GR1), and the natural join is associative, so `MATCH A MATCH B`
/// equals `MATCH A, B` wherever the run sits in the chain — the same
/// equivalence the all-`Simple` branch above uses for the whole chain.
/// Collapsing lets the LTJ intersect across the run instead of
/// enumerating each clause alone and hash-joining afterwards. `OPTIONAL`
/// (a left outer join) and §16.6-prefixed clauses (evaluated in
/// isolation) stay as boundaries between runs.
///
/// A clause joins the run only when every variable it shares with the run
/// is a singleton node variable on both sides. The comma-join does not yet
/// unify an edge or group variable repeated across its operands
/// (`MATCH (a)-[e]->(b), (x)-[e]->(y)` returns the cross product), while
/// the clause-by-clause hash join does; such a clause starts a new run so
/// this rewrite never changes a result.
fn collapse_simple_runs(matches: Vec<MatchStatement>) -> Vec<MatchStatement> {
    let mut out = Vec::with_capacity(matches.len());
    let mut run: Option<PathPattern> = None;
    let flush = |run: &mut Option<PathPattern>, out: &mut Vec<MatchStatement>| {
        if let Some(pattern) = run.take() {
            out.push(MatchStatement::Simple {
                pattern: optimizer::compile(pattern),
            });
        }
    };
    for m in matches {
        match m {
            MatchStatement::Simple { pattern } if !pattern.has_selected() => {
                if run
                    .as_ref()
                    .is_some_and(|acc| !joins_on_nodes_only(acc, &pattern))
                {
                    flush(&mut run, &mut out);
                }
                run = Some(match run.take() {
                    None => pattern,
                    Some(acc) => PathPattern::Join(Box::new(acc), Box::new(pattern)),
                });
            }
            MatchStatement::Simple { pattern } => {
                flush(&mut run, &mut out);
                out.push(MatchStatement::Simple {
                    pattern: optimizer::compile(pattern),
                });
            }
            MatchStatement::Optional { pattern } => {
                flush(&mut run, &mut out);
                out.push(MatchStatement::Optional {
                    pattern: optimizer::compile(pattern),
                });
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Whether every variable `a` and `b` share is a singleton node variable in
/// both — the only kind the comma-join unifies across its operands.
fn joins_on_nodes_only(a: &PathPattern, b: &PathPattern) -> bool {
    let (na, nb) = (singleton_node_vars(a), singleton_node_vars(b));
    a.freevars()
        .intersection(&b.freevars())
        .all(|v| na.contains(v) && nb.contains(v))
}

/// Node variables bound outside any quantifier (so not group variables).
fn singleton_node_vars(p: &PathPattern) -> HashSet<String> {
    match p {
        PathPattern::Node(d) => d.iter().flat_map(|d| d.var.clone()).collect(),
        PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
            let mut s = singleton_node_vars(a);
            s.extend(singleton_node_vars(b));
            s
        }
        PathPattern::Filter(p, _)
        | PathPattern::Questioned(p)
        | PathPattern::Selected { pattern: p, .. }
        | PathPattern::Named { pattern: p, .. } => singleton_node_vars(p),
        PathPattern::Repeat { .. }
        | PathPattern::EdgeRight(_)
        | PathPattern::EdgeLeft(_)
        | PathPattern::EdgeUndirected(_)
        | PathPattern::EdgeAnyDirection(_) => HashSet::new(),
    }
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
