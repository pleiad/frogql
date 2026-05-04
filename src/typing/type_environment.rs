use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::syntax::descriptor::Descriptor;

use super::variable_type::{Schema, VariableType};

/// A type environment mapping variable names to their inferred `VariableType`.
///
/// Bindings are stored as `Rc<VariableType>` so cloning the environment —
/// which happens on every `Concat`/`Join` via `meet` — only bumps refcounts
/// instead of deep-cloning each binding's descriptor tree.
#[derive(PartialEq, Eq, Clone, Debug, Default)]
pub struct TypeEnvironment {
    bindings: HashMap<String, Rc<VariableType>>,
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
        self.bindings.insert(key.to_string(), Rc::new(value));
    }

    pub fn get(&self, key: &str) -> Option<&VariableType> {
        self.bindings.get(key).map(Rc::as_ref)
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
                (Some(ta), Some(tb)) => VariableType::join((**ta).clone(), (**tb).clone()),
                (Some(ta), None) => VariableType::join((**ta).clone(), VariableType::Null),
                (None, Some(tb)) => VariableType::join(VariableType::Null, (**tb).clone()),
                (None, None) => unreachable!(),
            };
            result.insert(key.clone(), Rc::new(merged));
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
            let merged: Rc<VariableType> = match result.get(key) {
                Some(self_t) => {
                    let met = VariableType::meet(self_t, other);
                    if met == VariableType::Zero && !self_t.is_empty() && !other.is_empty() {
                        return Err(format!(
                            "Cannot reconcile types for variable {}: {} and {}",
                            key, self_t, other
                        ));
                    }
                    Rc::new(VariableType::refine(schema, &met))
                }
                // Right-only key: keep the binding as-is.
                None => Rc::clone(other),
            };
            result.insert(key.clone(), merged);
        }
        Ok(TypeEnvironment { bindings: result })
    }

    /// Left outer join — the typing operator `Γ₁ ⟕ Γ₂` for OPTIONAL MATCH,
    /// rule TLEFTJOIN of the paper:
    ///
    /// ```text
    ///                          S ⊢ T_{i1} ⊓ T_{i2} ▷ T'_i
    /// ────────────────────────────────────────────────────────────────────
    /// S ⊢ {x_i ↦ T_{i1}, x_j ↦ T_j} ⟕ {x_i ↦ T_{i2}, x_k ↦ T_k} ▷
    ///       {x_i ↦ T_{i1} ⊔ T'_i, x_j ↦ T_j, x_k ↦ T_k ⊔ Null}
    /// ```
    ///
    /// where `i` ranges over `dom(Γ₁) ∩ dom(Γ₂)`, `j` over the left-only
    /// keys, and `k` over the right-only keys. The judgment
    /// `S ⊢ T ▷ T'` is `refine(schema, T)`. For each shared variable the
    /// refined meet captures the success branch and the join with the
    /// left-side type captures the unsuccess branch (so an unsatisfiable
    /// optional collapses gracefully to the left-side binding instead of
    /// poisoning the whole environment).
    pub fn outer_join(
        schema: &Schema,
        a: &TypeEnvironment,
        b: &TypeEnvironment,
    ) -> TypeEnvironment {
        let mut result: HashMap<String, Rc<VariableType>> = HashMap::new();

        // Shared keys (i) and left-only keys (j) start from `a`.
        for (key, t1) in &a.bindings {
            let merged: Rc<VariableType> = match b.bindings.get(key) {
                Some(t2) => {
                    // T'_i := refine(schema, meet(T_{i1}, T_{i2}))
                    let met = VariableType::meet(t1, t2);
                    let refined = VariableType::refine(schema, &met);
                    // x_i ↦ T_{i1} ⊔ T'_i
                    Rc::new(VariableType::join((**t1).clone(), refined))
                }
                // x_j ↦ T_j (left-only, kept as-is).
                None => Rc::clone(t1),
            };
            result.insert(key.clone(), merged);
        }

        // Right-only keys (k): T_k ⊔ Null.
        for (key, t2) in &b.bindings {
            if a.bindings.contains_key(key) {
                continue;
            }
            result.insert(
                key.clone(),
                Rc::new(VariableType::join((**t2).clone(), VariableType::Null)),
            );
        }

        TypeEnvironment { bindings: result }
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.values().any(|v| v.is_empty())
    }

    /// Wrap each binding in `VariableType::Group`. Used for repeated/quantified
    /// patterns where variables become groups of values. (Mirrors fppc's
    /// `to_list` — gqlite uses `Group` for the same role.)
    pub fn to_group(&self) -> TypeEnvironment {
        TypeEnvironment {
            bindings: self
                .bindings
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        Rc::new(VariableType::Group(Box::new(v.as_ref().clone()))),
                    )
                })
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

    /// `Γ₁ ⋈ {} = Γ₁`: optional with no new bindings is identity. The
    /// success branch (meet) and unsuccess branch are both Γ₁, so their
    /// join collapses back to Γ₁.
    #[test]
    fn test_outer_join_empty_right_is_identity() {
        let mut a = TypeEnvironment::new();
        a.set("x", nstar());
        let r = TypeEnvironment::outer_join(&Schema::star(), &a, &TypeEnvironment::new());
        assert_eq!(r.get("x"), Some(&nstar()));
        assert_eq!(r.keys().len(), 1);
    }

    /// New variable in the optional side gets `T_k ⊔ Null` per TLEFTJOIN:
    /// the optional's real type when it matches, `Null` when it does not.
    #[test]
    fn test_outer_join_new_var_gains_null() {
        let mut a = TypeEnvironment::new();
        a.set("x", nstar());
        let mut b = TypeEnvironment::new();
        b.set("y", nstar());
        let r = TypeEnvironment::outer_join(&Schema::star(), &a, &b);
        assert_eq!(r.get("x"), Some(&nstar()));
        assert_eq!(
            r.get("y"),
            Some(&VariableType::Union(
                Box::new(nstar()),
                Box::new(VariableType::Null),
            )),
        );
    }

    /// Variable shared between left and optional: unsuccess keeps the left
    /// type unchanged; success refines (meet under schema). Star schema
    /// makes meet a no-op, so the result collapses to Γ₁'s type.
    #[test]
    fn test_outer_join_shared_var_unchanged_under_star_schema() {
        let mut a = TypeEnvironment::new();
        a.set("x", nstar());
        let mut b = TypeEnvironment::new();
        b.set("x", nstar());
        let r = TypeEnvironment::outer_join(&Schema::star(), &a, &b);
        assert_eq!(r.get("x"), Some(&nstar()));
    }

    /// When the meet on a shared variable collapses to Zero (irreconcilable
    /// types), TLEFTJOIN's `T_{i1} ⊔ T'_i` reduces to `T_{i1} ⊔ Zero`. Zero
    /// is the bottom of the join lattice, so the binding stays at the
    /// left-side type. Right-only variables still get `T_k ⊔ Null`.
    #[test]
    fn test_outer_join_irreconcilable_meet_falls_back_to_unsuccess() {
        use crate::typing::descriptor_type::DescriptorType;
        use crate::typing::label_type::LabelType;
        use crate::typing::property_type::PropertyType;

        let person = VariableType::Node(DescriptorType::new(
            LabelType::Label("Person".into()),
            PropertyType::open_empty(),
        ));
        let edge = VariableType::edge_directional(DescriptorType::new(
            LabelType::Label("Transfer".into()),
            PropertyType::open_empty(),
        ));

        let mut a = TypeEnvironment::new();
        a.set("x", person.clone());
        let mut b = TypeEnvironment::new();
        b.set("x", edge);
        b.set("y", nstar());

        let r = TypeEnvironment::outer_join(&Schema::star(), &a, &b);
        assert_eq!(r.get("x"), Some(&person));
        assert_eq!(
            r.get("y"),
            Some(&VariableType::Union(
                Box::new(nstar()),
                Box::new(VariableType::Null),
            )),
        );
    }
}
