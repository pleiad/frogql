use std::collections::{HashMap, HashSet};

use crate::syntax::descriptor::Descriptor;

use super::variable_type::{Schema, VariableType};

/// A type environment mapping variable names to their inferred `VariableType`.
///
/// Used during type checking to track the types of all variables in scope.
#[derive(PartialEq, Eq, Clone, Debug, Default)]
pub struct TypeEnvironment {
    bindings: HashMap<String, VariableType>,
}

impl TypeEnvironment {
    pub fn new() -> Self {
        TypeEnvironment {
            bindings: HashMap::new(),
        }
    }

    /// If the descriptor has a variable name, bind it to `t`.
    /// If not, return an empty environment.
    pub fn create_context(descriptor: &Descriptor, t: VariableType) -> Self {
        let mut env = TypeEnvironment::new();
        if let Some(var) = &descriptor.var {
            env.set(var, t);
        }
        env
    }

    pub fn set(&mut self, key: &str, value: VariableType) {
        self.bindings.insert(key.to_string(), value);
    }

    pub fn get(&self, key: &str) -> Option<&VariableType> {
        self.bindings.get(key)
    }

    pub fn keys(&self) -> Vec<&String> {
        self.bindings.keys().collect()
    }

    /// Pointwise join (least upper bound) of two environments.
    ///
    /// For keys present in both sides, the result binds the join of the two
    /// types. For keys present in only one side, the result binds the type
    /// joined with `Null` — the variable may be absent in the other branch.
    /// This matches the rule `Γ₁ ⊔ Γ₂` in the paper.
    pub fn union(a: &TypeEnvironment, b: &TypeEnvironment) -> TypeEnvironment {
        let keys: HashSet<&String> = a.bindings.keys().chain(b.bindings.keys()).collect();
        let mut result = HashMap::with_capacity(keys.len());
        for key in keys {
            let merged = match (a.bindings.get(key), b.bindings.get(key)) {
                (Some(ta), Some(tb)) => VariableType::join(ta, tb),
                (Some(ta), None) => VariableType::join(ta, &VariableType::Null),
                (None, Some(tb)) => VariableType::join(&VariableType::Null, tb),
                (None, None) => unreachable!(),
            };
            result.insert(key.clone(), merged);
        }
        TypeEnvironment { bindings: result }
    }

    /// Pointwise meet (greatest lower bound) of two environments with schema
    /// refinement. fppc's meet returns `Result<_, String>` because its
    /// `VariableType::meet` errors on shape mismatch; gqlite's `meet` returns
    /// `Zero` instead, so we surface the failure as `Err` here when the meet
    /// collapses to bottom on a variable that wasn't already empty in either
    /// input. Behavior at the checker level is equivalent.
    pub fn meet(
        schema: &Schema,
        a: &TypeEnvironment,
        b: &TypeEnvironment,
    ) -> Result<TypeEnvironment, String> {
        let mut result = a.bindings.clone();
        for (key, other) in &b.bindings {
            let merged = match result.get(key) {
                Some(self_t) => {
                    let met = VariableType::meet(self_t, other);
                    if met == VariableType::Zero && !self_t.is_empty() && !other.is_empty() {
                        return Err(format!(
                            "Cannot reconcile types for variable {}: {} and {}",
                            key, self_t, other
                        ));
                    }
                    VariableType::refine(schema, &met)
                }
                None => other.clone(),
            };
            result.insert(key.clone(), merged);
        }
        Ok(TypeEnvironment { bindings: result })
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.values().any(VariableType::is_empty)
    }

    /// Wrap each binding in `VariableType::Group`. Used for repeated/quantified
    /// patterns where variables become groups of values. (Mirrors fppc's
    /// `to_list` — gqlite uses `Group` for the same role.)
    pub fn to_group(&self) -> TypeEnvironment {
        TypeEnvironment {
            bindings: self
                .bindings
                .iter()
                .map(|(k, v)| (k.clone(), VariableType::Group(Box::new(v.clone()))))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nstar() -> VariableType {
        VariableType::node_star()
    }

    #[test]
    fn test_union_shared_key_joins_pointwise() {
        let mut a = TypeEnvironment::new();
        let mut b = TypeEnvironment::new();
        a.set("x", nstar());
        b.set("x", nstar());
        let u = TypeEnvironment::union(&a, &b);
        // Equal types collapse under join.
        assert_eq!(u.get("x"), Some(&nstar()));
    }

    #[test]
    fn test_union_key_only_in_left_joins_with_null() {
        let mut a = TypeEnvironment::new();
        let b = TypeEnvironment::new();
        a.set("x", nstar());
        let u = TypeEnvironment::union(&a, &b);
        assert_eq!(
            u.get("x"),
            Some(&VariableType::Union(
                Box::new(nstar()),
                Box::new(VariableType::Null),
            )),
        );
    }

    #[test]
    fn test_union_key_only_in_right_joins_with_null() {
        let a = TypeEnvironment::new();
        let mut b = TypeEnvironment::new();
        b.set("y", nstar());
        let u = TypeEnvironment::union(&a, &b);
        assert_eq!(
            u.get("y"),
            Some(&VariableType::Union(
                Box::new(VariableType::Null),
                Box::new(nstar()),
            )),
        );
    }
}
