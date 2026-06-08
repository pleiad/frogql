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
use syntax::query::{MatchStatement, Query, ReturnItem, Aggregator, SortKey};
use syntax::expr::Expr;
use typing::checker::Typechecker;
use typing::variable_type::Schema;
use std::collections::{HashSet, HashMap};

fn collect_expr_vars(expr: &Expr, acc: &mut HashSet<String>) {
    match expr {
        Expr::Var(name) => {
            acc.insert(name.clone());
        }
        Expr::AttrLookup { var, .. } => {
            acc.insert(var.clone());
        }
        Expr::FieldAccess { base, .. } => {
            collect_expr_vars(base, acc);
        }
        Expr::Binop { left, right, .. } => {
            collect_expr_vars(left, acc);
            collect_expr_vars(right, acc);
        }
        Expr::Unop { operand, .. } | Expr::IsNull { operand, .. } => {
            collect_expr_vars(operand, acc);
        }
        Expr::Coalesce(args) | Expr::Call { args, .. } => {
            for a in args {
                collect_expr_vars(a, acc);
            }
        }
        Expr::Record { fields } => {
            for (_, e) in fields {
                collect_expr_vars(e, acc);
            }
        }
        Expr::ValueSubquery { body } | Expr::Exists { body } | Expr::NotExists { body } => {
            collect_query_vars(body, acc);
        }
        Expr::Case { branches, else_expr } => {
            for (cond, value) in branches {
                collect_expr_vars(cond, acc);
                collect_expr_vars(value, acc);
            }
            if let Some(value) = else_expr {
                collect_expr_vars(value, acc);
            }
        }
        Expr::ListComprehension { var, source, filter, body } => {
            collect_expr_vars(source, acc);
            let mut inner = HashSet::new();
            if let Some(f) = filter {
                collect_expr_vars(f, &mut inner);
            }
            collect_expr_vars(body, &mut inner);
            inner.remove(var);
            acc.extend(inner);
        }
        Expr::Agg(agg) => match agg.as_ref() {
            Aggregator::CountStar => {}
            Aggregator::GeneralSet { expr, .. } => {
                collect_expr_vars(expr, acc);
            }
        },
        Expr::Const(_) | Expr::Type(_) => {}
    }
}

fn collect_query_vars(q: &Query, acc: &mut HashSet<String>) {
    if let Some(returns) = &q.returns {
        for item in returns {
            match item {
                ReturnItem::Expr { expr, .. } => {
                    collect_expr_vars(expr, acc);
                }
                ReturnItem::Aggregate { agg, .. } => match agg {
                    Aggregator::CountStar => {}
                    Aggregator::GeneralSet { expr, .. } => {
                        collect_expr_vars(expr, acc);
                    }
                },
            }
        }
    } else {
        for m in &q.matches {
            acc.extend(m.pattern().freevars());
        }
    }
    if let Some(gb) = &q.group_by {
        for e in gb {
            collect_expr_vars(e, acc);
        }
    }
    if let Some(ob) = &q.order_by {
        for spec in ob {
            if let SortKey::Expr(expr) = &spec.key {
                collect_expr_vars(expr, acc);
            }
        }
    }
    let mut var_counts = HashMap::new();
    for m in &q.matches {
        for v in m.pattern().freevars() {
            *var_counts.entry(v).or_insert(0) += 1;
        }
    }
    for (v, count) in var_counts {
        if count > 1 {
            acc.insert(v);
        }
    }
    for m in &q.matches {
        collect_pattern_filter_vars(m.pattern(), acc);
    }
}

fn collect_pattern_filter_vars(p: &PathPattern, acc: &mut HashSet<String>) {
    match p {
        PathPattern::Filter(inner, expr) => {
            collect_expr_vars(expr, acc);
            collect_pattern_filter_vars(inner, acc);
        }
        PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
            collect_pattern_filter_vars(a, acc);
            collect_pattern_filter_vars(b, acc);
        }
        PathPattern::Repeat { pattern, .. } | PathPattern::Questioned(pattern)
        | PathPattern::Selected { pattern, .. } | PathPattern::Named { pattern, .. } => {
            collect_pattern_filter_vars(pattern, acc);
        }
        _ => {}
    }
}

fn remove_unused_pattern_vars(p: PathPattern, used: &HashSet<String>) -> PathPattern {
    match p {
        PathPattern::Node(Some(mut d)) => {
            if let Some(ref var) = d.var {
                if !used.contains(var) {
                    d.var = None;
                }
            }
            PathPattern::Node(Some(d))
        }
        PathPattern::EdgeRight(Some(mut d)) => {
            if let Some(ref var) = d.var {
                if !used.contains(var) {
                    d.var = None;
                }
            }
            PathPattern::EdgeRight(Some(d))
        }
        PathPattern::EdgeLeft(Some(mut d)) => {
            if let Some(ref var) = d.var {
                if !used.contains(var) {
                    d.var = None;
                }
            }
            PathPattern::EdgeLeft(Some(d))
        }
        PathPattern::EdgeUndirected(Some(mut d)) => {
            if let Some(ref var) = d.var {
                if !used.contains(var) {
                    d.var = None;
                }
            }
            PathPattern::EdgeUndirected(Some(d))
        }
        PathPattern::EdgeAnyDirection(Some(mut d)) => {
            if let Some(ref var) = d.var {
                if !used.contains(var) {
                    d.var = None;
                }
            }
            PathPattern::EdgeAnyDirection(Some(d))
        }
        PathPattern::Concat(a, b) => PathPattern::Concat(
            Box::new(remove_unused_pattern_vars(*a, used)),
            Box::new(remove_unused_pattern_vars(*b, used)),
        ),
        PathPattern::Union(a, b) => PathPattern::Union(
            Box::new(remove_unused_pattern_vars(*a, used)),
            Box::new(remove_unused_pattern_vars(*b, used)),
        ),
        PathPattern::Join(a, b) => PathPattern::Join(
            Box::new(remove_unused_pattern_vars(*a, used)),
            Box::new(remove_unused_pattern_vars(*b, used)),
        ),
        PathPattern::Filter(inner, expr) => PathPattern::Filter(
            Box::new(remove_unused_pattern_vars(*inner, used)),
            expr,
        ),
        PathPattern::Repeat { pattern, lb, ub } => PathPattern::Repeat {
            pattern: Box::new(remove_unused_pattern_vars(*pattern, used)),
            lb,
            ub,
        },
        PathPattern::Questioned(inner) => {
            PathPattern::Questioned(Box::new(remove_unused_pattern_vars(*inner, used)))
        }
        PathPattern::Selected { prefix, pattern } => PathPattern::Selected {
            prefix,
            pattern: Box::new(remove_unused_pattern_vars(*pattern, used)),
        },
        PathPattern::Named { var, pattern } => PathPattern::Named {
            var,
            pattern: Box::new(remove_unused_pattern_vars(*pattern, used)),
        },
        other => other,
    }
}

/// Optimize the query while preserving OPTIONAL and §16.6 prefix boundaries.
fn optimize_query(mut q: Query, schema: &Schema) -> Query {
    let mut used_vars = HashSet::new();
    collect_query_vars(&q, &mut used_vars);
    for m in &mut q.matches {
        match m {
            MatchStatement::Simple { pattern } => {
                *pattern = remove_unused_pattern_vars(std::mem::replace(pattern, PathPattern::Node(None)), &used_vars);
            }
            MatchStatement::Optional { pattern } => {
                *pattern = remove_unused_pattern_vars(std::mem::replace(pattern, PathPattern::Node(None)), &used_vars);
            }
        }
    }

    // Selected patterns are evaluated in isolation; do not collapse across them.
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
