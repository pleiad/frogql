use std::fmt;

use crate::typing::descriptor_type::DescriptorType;

/// A descriptor attached to a node or edge pattern.
/// Has an optional variable name and a type constraint.
#[derive(Debug, Clone, PartialEq)]
pub struct Descriptor {
    /// Optional variable binding (e.g., "x" in `(x: Account)`)
    pub var: Option<String>,
    /// Type constraint (label + property)
    pub dtype: DescriptorType,
}

impl Descriptor {
    pub fn new(var: Option<String>, dtype: DescriptorType) -> Self {
        Self { var, dtype }
    }

    pub fn var_only(name: &str) -> Self {
        Self {
            var: Some(name.to_string()),
            dtype: DescriptorType::star(),
        }
    }

    pub fn type_only(dtype: DescriptorType) -> Self {
        Self { var: None, dtype }
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
