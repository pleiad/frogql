use std::fmt;

use super::expr::Expr;
use super::path_pattern::PathPattern;

/// `<set quantifier>` per ISO 39075 §20.9. `ALL` is the default when the
/// quantifier is omitted in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetQuantifier {
    All,
    Distinct,
}

/// `<general set function type>` per ISO 39075 §20.9. The five listed here
/// are core (always supported); `COLLECT_LIST` and `STDDEV_*` are Feature
/// GF10 (opt-in) and intentionally not modeled in this phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneralSetKind {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

/// An aggregate function in a RETURN clause. Mirrors the three syntactic
/// shapes of ISO 39075 §20.9 `<aggregate function>`:
///
/// - `COUNT(*)`            — `CountStar`, a special form distinct from `COUNT(expr)`.
/// - `KIND([DISTINCT|ALL] expr)` — `GeneralSet`, the core 5 (Count/Sum/Avg/Min/Max).
/// - `PERCENTILE_*(...)`   — Feature GF11, not modeled in this phase.
///
/// Aggregates are computed across the set of result rows, not row-by-row.
/// Per the ISO General Rules, `<general set function>`s eliminate null
/// inputs (with a warning) before applying the kind; `COUNT(*)` does not.
#[derive(Debug, Clone, PartialEq)]
pub enum Aggregator {
    /// `COUNT(*)` — counts every row in the (group of) result(s).
    CountStar,
    /// `<general set function>` — `kind([quantifier] expr)`.
    GeneralSet {
        kind: GeneralSetKind,
        quantifier: SetQuantifier,
        expr: Expr,
    },
}

/// A RETURN-clause item: either a plain expression projection or an aggregate.
///
/// A query that mixes both (e.g. `RETURN x.country, COUNT(*)`) implicitly
/// groups by the non-aggregate items. See `Runtime::run_query` for the
/// grouping semantics.
#[derive(Debug, Clone, PartialEq)]
pub enum ReturnItem {
    /// A plain expression: `RETURN x.name AS who`.
    Expr { expr: Expr, alias: Option<String> },
    /// An aggregate: `RETURN COUNT(*) AS total`.
    Aggregate {
        agg: Aggregator,
        alias: Option<String>,
    },
}

impl ReturnItem {
    /// Returns the alias if present, regardless of variant.
    pub fn alias(&self) -> Option<&str> {
        match self {
            ReturnItem::Expr { alias, .. } | ReturnItem::Aggregate { alias, .. } => {
                alias.as_deref()
            }
        }
    }

    /// True if this item is an aggregate. Used by the runtime to choose
    /// between row-by-row projection and the group-and-aggregate path.
    pub fn is_aggregate(&self) -> bool {
        matches!(self, ReturnItem::Aggregate { .. })
    }
}

/// A full GQL query: MATCH pattern WHERE condition GROUP BY ... RETURN projections.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// The pattern to match (includes WHERE as Filter if present).
    pub pattern: PathPattern,
    /// Optional explicit GROUP BY (ISO §16.15 + Feature GQ15).
    /// `None` → implicit Cypher-style grouping (non-aggregate RETURN items
    ///         become the group key automatically).
    /// `Some(vec)` → use these expressions as grouping keys; non-aggregate
    ///              RETURN items must structurally appear in this list.
    ///
    /// **Deviation from ISO**: the standard's `<grouping element>` is a
    /// `<binding variable reference>` only (requires LET to lift expressions
    /// into named bindings). gqlite accepts arbitrary `Expr`s here for
    /// usability, matching SQL / Cypher conventions.
    pub group_by: Option<Vec<Expr>>,
    /// Optional RETURN clause. None means return all bindings.
    pub returns: Option<Vec<ReturnItem>>,
    /// Whether RETURN DISTINCT was specified.
    pub distinct: bool,
}

impl Query {
    pub fn pattern_only(pattern: PathPattern) -> Self {
        Query {
            pattern,
            group_by: None,
            returns: None,
            distinct: false,
        }
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
            ReturnItem::Aggregate { agg, alias } => {
                write!(f, "{agg}")?;
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
        write!(f, "MATCH {}", self.pattern)?;
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
        Ok(())
    }
}
