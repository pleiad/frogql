use std::fmt;

use serde::{Deserialize, Serialize};

use super::label_type::LabelType;
use super::property_type::PropertyType;

/// Combines a label constraint with a property constraint.
/// Used to describe the type of a node or edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptorType {
    pub label: LabelType,
    pub props: PropertyType,
}

impl DescriptorType {
    pub fn new(label: LabelType, props: PropertyType) -> Self {
        Self { label, props }
    }

    /// Wildcard descriptor: matches any label and any properties.
    pub fn star() -> Self {
        Self {
            label: LabelType::Star,
            props: PropertyType::open_empty(),
        }
    }

    pub fn meet(a: &DescriptorType, b: &DescriptorType) -> DescriptorType {
        DescriptorType {
            label: LabelType::meet(&a.label, &b.label),
            props: PropertyType::meet(&a.props, &b.props),
        }
    }

    pub fn is_subtype(t1: &DescriptorType, t2: &DescriptorType) -> bool {
        LabelType::is_subtype(&t1.label, &t2.label)
            && PropertyType::is_subtype(&t1.props, &t2.props)
    }

    pub fn is_empty(&self) -> bool {
        self.label.is_empty() || self.props.is_empty()
    }
}

impl fmt::Display for DescriptorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.label, &self.props) {
            (LabelType::Star, PropertyType::Open(m)) if m.is_empty() => write!(f, "*"),
            _ => write!(f, "{} {}", self.label, self.props),
        }
    }
}
