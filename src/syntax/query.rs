use std::fmt;

use super::expr::Expr;
use super::path_pattern::PathPattern;

/// ISO 39075 §20.9 `<set quantifier>`. ALL is implicit when omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetQuantifier {
    All,
    Distinct,
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

#[derive(Debug, Clone, PartialEq)]
pub enum ReturnItem {
    Expr {
        expr: Expr,
        alias: Option<String>,
    },
    Aggregate {
        agg: Aggregator,
        alias: Option<String>,
    },
}

impl ReturnItem {
    pub fn alias(&self) -> Option<&str> {
        match self {
            ReturnItem::Expr { alias, .. } | ReturnItem::Aggregate { alias, .. } => {
                alias.as_deref()
            }
        }
    }

    pub fn is_aggregate(&self) -> bool {
        matches!(self, ReturnItem::Aggregate { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub pattern: PathPattern,
    /// ISO §16.15 explicit GROUP BY. None → no grouping (only valid when
    /// RETURN is pure-aggregate or pure-projection; the typechecker rejects
    /// mixed RETURN without an explicit GROUP BY).
    ///
    /// Deviation from ISO: accepts arbitrary `Expr`s as grouping elements
    /// (the standard restricts to `<binding variable reference>` + LET).
    pub group_by: Option<Vec<Expr>>,
    pub returns: Option<Vec<ReturnItem>>,
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
