use std::fmt;

use super::expr::Expr;
use super::path_pattern::PathPattern;
use crate::typing::simple_type::SimpleType;

/// ISO 39075 §20.9 `<set quantifier>`. ALL is implicit when omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetQuantifier {
    All,
    Distinct,
}

/// ISO §16.17 `<ordering specification>`. ASC is implicit when omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// ISO §16.17 `<null ordering>` (Feature GA03). When the user does not
/// specify NULLS FIRST or NULLS LAST, the implementation default kicks
/// in — gqlite uses NULLS LAST regardless of `SortDir` per ISO §16.17
/// SR 6 ("the default for `<null ordering>` shall not depend on the
/// context outside of `<sort specification list>`").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullsOrder {
    First,
    Last,
}

/// `Expr` — evaluated against the binding-table assignment
/// (pre-projection sort). `Column(idx)` — index into the projected
/// row, produced by the parser when a sort key resolves to a RETURN
/// alias or to an aggregate already in RETURN (post-projection sort).
#[derive(Debug, Clone, PartialEq)]
pub enum SortKey {
    Expr(Expr),
    Column(usize),
    /// Post-projection sort by a RETURN alias with a CAST applied:
    /// `ORDER BY CAST(<alias> AS <type>)`.
    ColumnCast {
        col: usize,
        ty: SimpleType,
    },
    /// Post-projection sort by a field path into a record-valued
    /// projected column — `ORDER BY <alias>.<field>` where `<alias>` is a
    /// RETURN item aliased to a record (e.g. a `VALUE { ... }` subquery).
    /// `col` indexes the projected row; `path` walks nested record fields.
    ColumnField {
        col: usize,
        path: Vec<String>,
    },
}

/// ISO §16.17 `<sort specification>`.
#[derive(Debug, Clone, PartialEq)]
pub struct SortSpec {
    pub key: SortKey,
    pub dir: SortDir,
    /// `None` means the user did not specify, so the implementation
    /// default applies (see `NullsOrder` doc).
    pub nulls: Option<NullsOrder>,
}

/// ISO §20.9 `<general set function type>` core 5 (always supported).
/// `COLLECT_LIST`/`STDDEV_*` (Feature GF10) deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneralSetKind {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    /// ISO §20.9 list/multiset aggregate (`COLLECT_LIST`, aliases
    /// `COLLECT` / `ARRAY_AGG`). Collects the group's values into a
    /// `Value::List` instead of reducing to a scalar.
    CollectList,
}

/// ISO §20.9 `<aggregate function>`. `COUNT(*)` is a separate variant
/// because it has different semantics (no null-elimination, no DISTINCT)
/// and a special syntactic form distinct from `KIND([Q] expr)`.
/// `PERCENTILE_*` (Feature GF11) would be a third variant — slot reserved.
#[derive(Debug, Clone, PartialEq)]
pub enum Aggregator {
    CountStar,
    GeneralSet {
        kind: GeneralSetKind,
        quantifier: SetQuantifier,
        expr: Expr,
    },
}

/// One projected column. Aggregates are ordinary expressions
/// (`Expr::Agg`), not a variant of their own: `SUM(x)` and `SUM(x) + 0`
/// differ only in the shape of the expression they carry, which is a
/// distinction no consumer needs. A separate `Aggregate` variant existed
/// while aggregates lived outside the expression grammar; every site that
/// received it immediately re-unified the two (`is_aggregate()` below is
/// what is left of that), and keeping the two arms in step is what #95
/// failed at.
#[derive(Debug, Clone, PartialEq)]
pub enum ReturnItem {
    Expr { expr: Expr, alias: Option<String> },
}

impl ReturnItem {
    pub fn alias(&self) -> Option<&str> {
        match self {
            ReturnItem::Expr { alias, .. } => alias.as_deref(),
        }
    }

    /// True when this item is reduced over a group — it carries an
    /// aggregate anywhere in its tree, whether bare (`SUM(x)`) or inside
    /// arithmetic (`COUNT(x) + COUNT(y)`). Such items are projected
    /// per-group, not used as grouping keys.
    ///
    /// Note this asks the *coarse* question. Whether an aggregate actually
    /// collapses rows depends on its argument: one over a group variable
    /// reduces within a single match (§22.7) and keeps one row per match.
    /// Sites that decide row arity must use the runtime's / typechecker's
    /// `item_has_row_aggregate` instead.
    pub fn is_aggregate(&self) -> bool {
        match self {
            ReturnItem::Expr { expr, .. } => expr.contains_agg(),
        }
    }
}

/// ISO 39075 §14.4 `<match statement>`. `Simple` is `MATCH`,
/// `Optional` is `OPTIONAL MATCH` (Feature GQ21 nested-block form is
/// not yet exposed; a single optional pattern is enough for the
/// formalism described in the OOPSLA paper rule TOpt).
///
/// ISO §16.6 prefixes live inside `pattern` as `PathPattern::Selected`,
/// scoped per comma operand.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchStatement {
    Simple { pattern: PathPattern },
    Optional { pattern: PathPattern },
}

impl MatchStatement {
    pub fn pattern(&self) -> &PathPattern {
        match self {
            MatchStatement::Simple { pattern } | MatchStatement::Optional { pattern } => pattern,
        }
    }

    pub fn is_optional(&self) -> bool {
        matches!(self, MatchStatement::Optional { .. })
    }
}

/// What the `k` of a `NEAREST` clause counts.
///
/// The two differ whenever one binding of the search variable yields
/// several result rows, which for a join pattern is the common case.
/// Both are supported because which one a study wants depends on the
/// question: "the 10 nearest images that appear in a matching pattern"
/// is `DistinctVar`; "the 10 best rows" is `Rows`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KMode {
    /// `k` distinct values of the search variable, each having at least
    /// one match. Default.
    DistinctVar,
    /// `k` result rows, ordered by the search variable's distance.
    Rows,
}

/// A vector-search constraint on one variable of the pattern.
///
/// ```text
/// NEAREST <k> [ROWS] <var>.<attr> TO <expr> [AS <distvar>]
/// ```
///
/// Modelled on the "magic predicate" idiom SPARQL engines use for
/// nearest-neighbour iterators (`?img proc:hnswIterator ("idx" ?v ?d)`),
/// but written as a clause rather than a pattern operand. Position in a
/// SPARQL basic graph pattern does not fix evaluation order either, so
/// nothing is lost, and a clause avoids threading a new variant through
/// every `PathPattern` traversal.
///
/// It sits between the MATCH chain and RETURN so `dist_var` is in scope
/// for projection, GROUP BY, and ORDER BY.
#[derive(Debug, Clone, PartialEq)]
pub struct NearestClause {
    /// How many neighbours to keep. `0` yields an empty result.
    pub k: u32,
    pub mode: KMode,
    /// The pattern variable constrained to the nearest neighbours.
    pub var: String,
    /// Which vector attribute of `var` to search.
    pub attr: String,
    /// The query vector. Either a literal list of numbers or a
    /// `VECTOR(<node id>, '<attr>')` call reading a stored vector — the
    /// analogue of taking the embedding of an example entity.
    pub query: Expr,
    /// Optional binding for the distance, like SPARQL's `?dist`.
    pub dist_var: Option<String>,
}

impl fmt::Display for NearestClause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NEAREST {}", self.k)?;
        if self.mode == KMode::Rows {
            write!(f, " ROWS")?;
        }
        write!(f, " {}.{} TO {}", self.var, self.attr, self.query)?;
        if let Some(d) = &self.dist_var {
            write!(f, " AS {d}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// ISO §14.3 `<simple linear query statement>` — the chain of MATCH
    /// statements. Always non-empty for parsed queries. May mix
    /// `MatchStatement::Simple` and `MatchStatement::Optional`.
    pub matches: Vec<MatchStatement>,
    /// Non-ISO extension: a vector-search constraint on one pattern
    /// variable. See `NearestClause`.
    pub nearest: Option<NearestClause>,
    /// ISO §16.15 explicit GROUP BY. None → no grouping (only valid when
    /// RETURN is pure-aggregate or pure-projection; the typechecker rejects
    /// mixed RETURN without an explicit GROUP BY).
    ///
    /// Deviation from ISO: accepts arbitrary `Expr`s as grouping elements
    /// (the standard restricts to `<binding variable reference>` + LET).
    pub group_by: Option<Vec<Expr>>,
    pub returns: Option<Vec<ReturnItem>>,
    pub distinct: bool,
    /// ISO §14.9 `<order by clause>` + §16.17 `<sort specification list>`.
    /// `None` means no ordering — rows come out in whatever order the
    /// runtime produced them (LTJ VEO / candidate-list iteration). When
    /// `Some(specs)`, runtime sorts the working table by `specs` BEFORE
    /// applying LIMIT (per §14.9 GR 5: ORDER BY → OFFSET → LIMIT).
    ///
    /// Default direction is ASC per §16.17 SR 5b. Default null ordering
    /// is implementation-defined (gqlite uses NULLS LAST regardless of
    /// direction per §16.17 SR 6).
    pub order_by: Option<Vec<SortSpec>>,
    /// ISO/IEC 39075:2024 `<limit clause>` (feature GQ13). `None` means
    /// "no LIMIT clause was written"; callers may still pass a runtime
    /// cap via `Runtime::run_query(_, cap)` and it will apply. `Some(N)`
    /// means the user wrote `LIMIT N` — when both an in-query LIMIT
    /// and a runtime cap are set, the smaller wins.
    ///
    /// `Some(0)` is semantically distinct from `None`: per the spec,
    /// `LIMIT 0` returns an empty binding table (the user explicitly
    /// asked for zero rows). `Runtime::run_query` short-circuits on
    /// `Some(0)` before any pattern work runs, so the runtime's
    /// `0 = unbounded` convention at the integer-parameter boundary
    /// doesn't accidentally swallow it.
    ///
    /// Spec divergence: the spec defines `LIMIT N` as part of an
    /// `<order by and page statement>`, with the implicit-ordering
    /// clause "the implementation must first sort the collection of
    /// all records into a new ordered binding table using an
    /// implementation-dependent order before applying the limit." We
    /// don't sort: rows are returned in whatever order the runtime
    /// produces (LTJ VEO, candidate-list iteration order, etc.), which
    /// is stable in practice but unspecified. The spec's Optimization
    /// Note explicitly allows this when the implementation does not
    /// claim the result is ordered. We don't. ORDER BY is the other
    /// half of GQ13 and lands in a separate change.
    pub limit: Option<u32>,
}

impl Query {
    pub fn pattern_only(pattern: PathPattern) -> Self {
        Query {
            matches: vec![MatchStatement::Simple { pattern }],
            ..Query::empty()
        }
    }

    /// A query with no clauses and no match statements. Not valid on its
    /// own — `matches` must be non-empty — but it gives every
    /// construction site a `..Query::empty()` tail so adding a clause
    /// does not touch two dozen struct literals.
    pub fn empty() -> Self {
        Query {
            matches: Vec::new(),
            nearest: None,
            group_by: None,
            returns: None,
            distinct: false,
            order_by: None,
            limit: None,
        }
    }

    /// Collapse the match chain into a single `PathPattern` by joining
    /// adjacent matches with `PathPattern::Join` (the comma-join). Sound
    /// only when every match is `Simple`: ISO §14.4 General Rule 1
    /// specifies MATCH expands the working table by the new binding
    /// table, which for non-OPTIONAL matches matches the natural-join
    /// semantics of gqlite's existing comma-join.
    ///
    /// `OPTIONAL MATCH` is a left-outer-join, not a natural join, and
    /// cannot be collapsed this way. Callers must check
    /// `has_any_optional()` first; mixed queries are evaluated by
    /// walking `matches` directly in the typechecker and runtime.
    pub fn collapsed_pattern(&self) -> PathPattern {
        debug_assert!(
            !self.has_any_optional(),
            "collapsed_pattern() is unsound when any match is OPTIONAL — \
             callers must dispatch on has_any_optional()"
        );
        let mut iter = self.matches.iter().map(|m| m.pattern().clone());
        let first = iter
            .next()
            .expect("Query::matches must contain at least one match statement");
        iter.fold(first, |acc, p| {
            PathPattern::Join(Box::new(acc), Box::new(p))
        })
    }

    pub fn has_any_optional(&self) -> bool {
        self.matches.iter().any(|m| m.is_optional())
    }

    /// True when any MATCH pattern contains an ISO §16.6 prefix.
    pub fn has_any_selected(&self) -> bool {
        self.matches.iter().any(|m| m.pattern().has_selected())
    }
}

impl fmt::Display for GeneralSetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            GeneralSetKind::Count => "COUNT",
            GeneralSetKind::Sum => "SUM",
            GeneralSetKind::Avg => "AVG",
            GeneralSetKind::Min => "MIN",
            GeneralSetKind::Max => "MAX",
            GeneralSetKind::CollectList => "COLLECT_LIST",
        };
        f.write_str(s)
    }
}

impl fmt::Display for Aggregator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Aggregator::CountStar => write!(f, "COUNT(*)"),
            Aggregator::GeneralSet {
                kind,
                quantifier: SetQuantifier::All,
                expr,
            } => write!(f, "{kind}({expr})"),
            Aggregator::GeneralSet {
                kind,
                quantifier: SetQuantifier::Distinct,
                expr,
            } => write!(f, "{kind}(DISTINCT {expr})"),
        }
    }
}

impl fmt::Display for ReturnItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReturnItem::Expr { expr, alias } => {
                write!(f, "{expr}")?;
                if let Some(a) = alias {
                    write!(f, " AS {a}")?;
                }
            }
        }
        Ok(())
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for m in &self.matches {
            if !first {
                f.write_str(" ")?;
            }
            first = false;
            let (kw, pattern) = match m {
                MatchStatement::Simple { pattern } => ("MATCH", pattern),
                MatchStatement::Optional { pattern } => ("OPTIONAL MATCH", pattern),
            };
            write!(f, "{kw} {pattern}")?;
        }
        if let Some(n) = &self.nearest {
            write!(f, " {n}")?;
        }
        if let Some(gb) = &self.group_by {
            let exprs: Vec<String> = gb.iter().map(|e| e.to_string()).collect();
            write!(f, " GROUP BY {}", exprs.join(", "))?;
        }
        if let Some(returns) = &self.returns {
            write!(f, " RETURN ")?;
            if self.distinct {
                write!(f, "DISTINCT ")?;
            }
            let items: Vec<String> = returns.iter().map(|r| r.to_string()).collect();
            write!(f, "{}", items.join(", "))?;
        }
        if let Some(specs) = &self.order_by {
            let parts: Vec<String> = specs.iter().map(|s| s.to_string()).collect();
            write!(f, " ORDER BY {}", parts.join(", "))?;
        }
        if let Some(n) = self.limit {
            write!(f, " LIMIT {n}")?;
        }
        Ok(())
    }
}

impl fmt::Display for SortKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SortKey::Expr(e) => write!(f, "{e}"),
            SortKey::Column(idx) => write!(f, "#{idx}"),
            SortKey::ColumnCast { col, ty } => write!(f, "CAST(#{col} AS {ty})"),
            SortKey::ColumnField { col, path } => write!(f, "#{col}.{}", path.join(".")),
        }
    }
}

impl fmt::Display for SortSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.key)?;
        match self.dir {
            SortDir::Asc => {} // ASC is the default — print only when DESC.
            SortDir::Desc => write!(f, " DESC")?,
        }
        match self.nulls {
            None => {}
            Some(NullsOrder::First) => write!(f, " NULLS FIRST")?,
            Some(NullsOrder::Last) => write!(f, " NULLS LAST")?,
        }
        Ok(())
    }
}
