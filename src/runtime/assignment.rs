use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::model::value::{PathValue, Value};

/// Maps pattern variables to matched runtime values.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Assignment {
    pub m: HashMap<String, PathValue>,
    /// Plain scalars bound by a clause rather than by the pattern — for
    /// now only the distance of a `NEAREST ... AS d`.
    ///
    /// A separate map rather than a new `PathValue` variant: `PathValue`
    /// models graph elements (nodes, edges, groups, paths) and is matched
    /// exhaustively in a dozen places, none of which has anything
    /// sensible to do with a float. Empty for every query that does not
    /// bind one, so it costs an empty `HashMap` per row. The precedent is
    /// `Runtime::comprehension_scope`, which already carries
    /// scalar-valued bindings outside the binding table.
    pub scalars: HashMap<String, Value>,
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

    /// A clause-bound scalar, if `x` names one.
    pub fn get_scalar(&self, x: &str) -> Option<&Value> {
        self.scalars.get(x)
    }

    pub fn set_scalar(&mut self, x: String, v: Value) {
        self.scalars.insert(x, v);
    }

    pub fn extend(&mut self, x: String, val: PathValue) {
        self.m.insert(x, val);
    }

    pub fn keys(&self) -> HashSet<String> {
        self.m.keys().chain(self.scalars.keys()).cloned().collect()
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
        for (k, v) in &self.scalars {
            if let Some(ov) = other.scalars.get(k) {
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
        for (k, v) in &other.scalars {
            result.scalars.insert(k.clone(), v.clone());
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
        let mut parts: Vec<(String, String)> = self
            .m
            .iter()
            .map(|(k, v)| (k.clone(), format!("{k} ↦ {v}")))
            .chain(
                self.scalars
                    .iter()
                    .map(|(k, v)| (k.clone(), format!("{k} ↦ {v}"))),
            )
            .collect();
        parts.sort_by(|a, b| a.0.cmp(&b.0));
        let parts: Vec<String> = parts.into_iter().map(|(_, s)| s).collect();
        write!(f, "{}", parts.join(", "))
    }
}
