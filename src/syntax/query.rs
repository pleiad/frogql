use std::fmt;

use super::expr::Expr;
use super::path_pattern::PathPattern;

/// A return item: expression with optional alias.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnItem {
    pub expr: Expr,
    pub alias: Option<String>,
}

/// A full GQL query: MATCH pattern WHERE condition RETURN projections.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// The pattern to match (includes WHERE as Filter if present).
    pub pattern: PathPattern,
    /// Optional RETURN clause. None means return all bindings.
    pub returns: Option<Vec<ReturnItem>>,
    /// Whether RETURN DISTINCT was specified.
    pub distinct: bool,
}

impl Query {
    pub fn pattern_only(pattern: PathPattern) -> Self {
        Query {
            pattern,
            returns: None,
            distinct: false,
        }
    }
}

impl fmt::Display for ReturnItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.expr)?;
        if let Some(alias) = &self.alias {
            write!(f, " AS {alias}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MATCH {}", self.pattern)?;
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
