use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::model::value::PathValue;

/// Maps pattern variables to matched runtime values.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Assignment {
    pub m: HashMap<String, PathValue>,
}

impl Assignment {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with a single binding (if var is Some).
    pub fn from_optional(var: Option<&str>, val: PathValue) -> Self {
        let mut a = Self::new();
        if let Some(v) = var {
            a.m.insert(v.to_string(), val);
        }
        a
    }

    pub fn get(&self, x: &str) -> Option<&PathValue> {
        self.m.get(x)
    }

    pub fn extend(&mut self, x: String, val: PathValue) {
        self.m.insert(x, val);
    }

    pub fn keys(&self) -> HashSet<String> {
        self.m.keys().cloned().collect()
    }

    /// Can this assignment be unified with another?
    /// Shared variables must map to the same value.
    pub fn can_unify(&self, other: &Assignment) -> bool {
        for (k, v) in &self.m {
            if let Some(ov) = other.m.get(k) {
                if v != ov {
                    return false;
                }
            }
        }
        true
    }

    /// Merge two assignments. Panics if they conflict.
    pub fn unify(&self, other: &Assignment) -> Assignment {
        debug_assert!(self.can_unify(other));
        let mut result = self.clone();
        for (k, v) in &other.m {
            result.m.insert(k.clone(), v.clone());
        }
        result
    }

    /// Fill missing variables with Nothing.
    pub fn fill_nones(&mut self, dom: &HashSet<String>) {
        for x in dom {
            if !self.m.contains_key(x) {
                self.m.insert(x.clone(), PathValue::Nothing);
            }
        }
    }

    /// Fill missing variables with empty List (for repetition base case).
    pub fn fill_empty_list(&mut self, dom: &HashSet<String>) {
        for x in dom {
            if !self.m.contains_key(x) {
                self.m.insert(x.clone(), PathValue::Group(vec![]));
            }
        }
    }

    /// Wrap all values in singleton lists (for repetition grouping).
    pub fn to_group(&mut self) {
        for val in self.m.values_mut() {
            *val = PathValue::Group(vec![val.clone()]);
        }
    }

    /// Concatenate grouped assignments (both must have List values).
    pub fn concat_group(&self, other: &Assignment) -> Assignment {
        let mut result = Assignment::new();
        for (k, v) in &self.m {
            match (v, other.m.get(k)) {
                (PathValue::Group(a), Some(PathValue::Group(b))) => {
                    let mut combined = a.clone();
                    combined.extend_from_slice(b);
                    result.m.insert(k.clone(), PathValue::Group(combined));
                }
                _ => {
                    result.m.insert(k.clone(), v.clone());
                }
            }
        }
        result
    }
}

impl fmt::Display for Assignment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut entries: Vec<_> = self.m.iter().collect();
        entries.sort_by_key(|(k, _)| (*k).clone());
        let parts: Vec<String> = entries.iter().map(|(k, v)| format!("{k} ↦ {v}")).collect();
        write!(f, "{}", parts.join(", "))
    }
}
