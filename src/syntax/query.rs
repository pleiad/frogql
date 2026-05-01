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

/// ISO 39075 §14.4 `<match statement>`. `Simple` is `MATCH`,
/// `Optional` is `OPTIONAL MATCH` (Feature GQ21 nested-block form is
/// not yet exposed; a single optional pattern is enough for the
/// formalism described in the OOPSLA paper rule TOpt).
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

#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// ISO §14.3 `<simple linear query statement>` — the chain of MATCH
    /// statements. Always non-empty for parsed queries. Currently every
    /// element is `MatchStatement::Simple`; a future PR will add OPTIONAL.
    pub matches: Vec<MatchStatement>,
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
            matches: vec![MatchStatement::Simple { pattern }],
            group_by: None,
            returns: None,
            distinct: false,
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
        let mut first = true;
        for m in &self.matches {
            if !first {
                f.write_str(" ")?;
            }
            first = false;
            match m {
                MatchStatement::Simple { pattern } => write!(f, "MATCH {pattern}")?,
                MatchStatement::Optional { pattern } => write!(f, "OPTIONAL MATCH {pattern}")?,
            }
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
        Ok(())
    }
}
