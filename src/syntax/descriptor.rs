use std::fmt;

use crate::typing::descriptor_type::DescriptorType;

use super::expr::Expr;

/// A descriptor attached to a node or edge pattern.
/// Has an optional variable name, a type constraint, and possibly raw
/// value-equality filters that the elaboration pass lowers into WHERE.
#[derive(Debug, Clone, PartialEq)]
pub struct Descriptor {
    /// Optional variable binding (e.g., "x" in `(x: Account)`)
    pub var: Option<String>,
    /// Type constraint (label + property)
    pub dtype: DescriptorType,
    /// Raw value-equality filters from `{name: 'Alice'}` surface syntax.
    /// Always empty after the elaboration pass — it rewrites these into a
    /// surrounding `PathPattern::Filter(..., var.attr = value AND ...)`.
    pub value_filters: Vec<(String, Expr)>,
}

impl Descriptor {
    pub fn new(var: Option<String>, dtype: DescriptorType) -> Self {
        Self { var, dtype, value_filters: Vec::new() }
    }

    pub fn with_filters(
        var: Option<String>,
        dtype: DescriptorType,
        value_filters: Vec<(String, Expr)>,
    ) -> Self {
        Self { var, dtype, value_filters }
    }

    pub fn var_only(name: &str) -> Self {
        Self {
            var: Some(name.to_string()),
            dtype: DescriptorType::star(),
            value_filters: Vec::new(),
        }
    }

    pub fn type_only(dtype: DescriptorType) -> Self {
        Self { var: None, dtype, value_filters: Vec::new() }
    }
}

impl fmt::Display for Descriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.var, &self.dtype) {
            (Some(v), d) => write!(f, "{v}: {d}"),
            (None, d) => write!(f, "{d}"),
        }
    }
}
