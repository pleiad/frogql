use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::syntax::descriptor::Descriptor;

use super::variable_type::{Schema, VariableType};

/// One environment binding: the shared type plus a lazily-computed
/// interned id (`Schema::intern_vt_rc`). The id exists so the lattice
/// memos (`meet_refined`/`join_interned`) can key by integer pair; it is
/// a cache, not part of the value — equality compares the type only.
#[derive(Debug, Clone)]
struct Binding {
    ty: Rc<VariableType>,
    id: std::cell::Cell<Option<u32>>,
}

impl Binding {
    fn new(ty: Rc<VariableType>) -> Self {
        Binding {
            ty,
            id: std::cell::Cell::new(None),
        }
    }

    fn with_id(ty: Rc<VariableType>, id: u32) -> Self {
        Binding {
            ty,
            id: std::cell::Cell::new(Some(id)),
        }
    }

    fn id(&self, schema: &Schema) -> u32 {
        match self.id.get() {
            Some(i) => i,
            None => {
                let i = schema.intern_vt_rc(&self.ty);
                self.id.set(Some(i));
                i
            }
        }
    }
}

impl PartialEq for Binding {
    fn eq(&self, other: &Self) -> bool {
        self.ty == other.ty
    }
}
impl Eq for Binding {}

thread_local! {
    /// Shared `Null` value for the one-sided join arms.
    static NULL_VT: Rc<VariableType> = Rc::new(VariableType::Null);
}

/// A type environment mapping variable names to their inferred `VariableType`.
///
/// Bindings are stored as `Rc<VariableType>` so cloning the environment —
/// which happens on every `Concat`/`Join` via `meet` — only bumps refcounts
/// instead of deep-cloning each binding's descriptor tree.
#[derive(PartialEq, Eq, Clone, Debug, Default)]
pub struct TypeEnvironment {
    bindings: HashMap<String, Binding>,
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
        TypeEnvironment::create_context_shared(descriptor, Rc::new(t))
    }

    /// `create_context` for an already-shared binding (refine-cache hits
    /// arrive as `Rc`; re-wrapping would deep-clone for nothing).
    pub fn create_context_shared(descriptor: &Descriptor, t: Rc<VariableType>) -> Self {
        let mut env = TypeEnvironment::new();
        if let Some(var) = &descriptor.var {
            env.set_shared(var, t);
        }
        env
    }

    pub fn set(&mut self, key: &str, value: VariableType) {
        self.bindings
            .insert(key.to_string(), Binding::new(Rc::new(value)));
    }

    pub fn get(&self, key: &str) -> Option<&VariableType> {
        self.bindings.get(key).map(|b| b.ty.as_ref())
    }

    /// Like `get`, but exposes the shared binding for callers that need
    /// to retain it without deep-cloning.
    pub fn get_shared(&self, key: &str) -> Option<&Rc<VariableType>> {
        self.bindings.get(key).map(|b| &b.ty)
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.bindings.keys()
    }

    /// Iterate `(name, binding)` pairs. The `Rc` is exposed so callers
    /// merging environments can share bindings instead of deep-cloning
    /// the descriptor trees.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Rc<VariableType>)> {
        self.bindings.iter().map(|(k, b)| (k, &b.ty))
    }

    /// Insert an already-shared binding without cloning the inner type.
    pub fn set_shared(&mut self, key: &str, value: Rc<VariableType>) {
        self.bindings.insert(key.to_string(), Binding::new(value));
    }

    /// Pointwise join (least upper bound) of two environments.
    ///
    /// For keys present in both sides, the result binds the join of the two
    /// types. For keys present in only one side, the result binds the type
    /// joined with `Null` — the variable may be absent in the other branch.
    /// This matches the rule `Γ₁ ⊔ Γ₂` in the paper.
    pub fn union(schema: &Schema, a: &TypeEnvironment, b: &TypeEnvironment) -> TypeEnvironment {
        super::stats::record_env_union();
        let null_rc = NULL_VT.with(Rc::clone);
        let null_id = schema.intern_vt_rc(&null_rc);
        let keys: HashSet<&String> = a.bindings.keys().chain(b.bindings.keys()).collect();
        let mut result = HashMap::with_capacity(keys.len());
        for key in keys {
            let merged = match (a.bindings.get(key), b.bindings.get(key)) {
                // Same shared binding on both sides (common: both arms
                // hold the same refine-cache Rc): `join(v, v)` collapses
                // to `v`, so share it without cloning or walking.
                (Some(ta), Some(tb)) if Rc::ptr_eq(&ta.ty, &tb.ty) => ta.clone(),
                (Some(ta), Some(tb)) => {
                    let (id, ty) =
                        schema.join_interned(ta.id(schema), &ta.ty, tb.id(schema), &tb.ty);
                    Binding::with_id(ty, id)
                }
                (Some(ta), None) => {
                    let (id, ty) = schema.join_interned(ta.id(schema), &ta.ty, null_id, &null_rc);
                    Binding::with_id(ty, id)
                }
                (None, Some(tb)) => {
                    let (id, ty) = schema.join_interned(null_id, &null_rc, tb.id(schema), &tb.ty);
                    Binding::with_id(ty, id)
                }
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
        super::stats::record_env_bindings_copied(a.bindings.len());
        TypeEnvironment::meet_owned(schema, a.clone(), b).map_err(|(_, msg)| msg)
    }

    /// `meet` that consumes the left environment instead of cloning it —
    /// the checker's Concat/Join/match-chain folds own their accumulator
    /// and were paying O(vars) String-key clones per operator (quadratic
    /// along a chain). Merged bindings are staged in a scratch `Vec` and
    /// applied only on success, so the `Err` case hands `a` back
    /// untouched (callers keep the pre-meet environment on error, exactly
    /// as the cloning version behaved).
    pub fn meet_owned(
        schema: &Schema,
        mut a: TypeEnvironment,
        b: &TypeEnvironment,
    ) -> Result<TypeEnvironment, (TypeEnvironment, String)> {
        super::stats::record_env_meet();
        let mut merged: Vec<(&String, Binding)> = Vec::new();
        for (key, other) in &b.bindings {
            match a.bindings.get(key) {
                Some(self_b) => {
                    match schema.meet_refined(
                        self_b.id(schema),
                        &self_b.ty,
                        other.id(schema),
                        &other.ty,
                    ) {
                        Some((rid, refined)) => merged.push((key, Binding::with_id(refined, rid))),
                        // Collapse marker: met to Zero with both sides
                        // non-empty — same message the uncached path built.
                        None => {
                            let msg = format!(
                                "Cannot reconcile types for variable {}: {} and {}",
                                key, self_b.ty, other.ty
                            );
                            return Err((a, msg));
                        }
                    }
                }
                // Right-only key: keep the binding as-is.
                None => merged.push((key, other.clone())),
            }
        }
        for (k, v) in merged {
            a.bindings.insert(k.clone(), v);
        }
        Ok(a)
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
        super::stats::record_env_outer_join();
        let null_rc = NULL_VT.with(Rc::clone);
        let null_id = schema.intern_vt_rc(&null_rc);
        let mut result: HashMap<String, Binding> = HashMap::new();

        // Shared keys (i) and left-only keys (j) start from `a`.
        for (key, t1) in &a.bindings {
            let merged: Binding = match b.bindings.get(key) {
                Some(t2) => {
                    // T'_i := refine(schema, meet(T_{i1}, T_{i2}))
                    match schema.meet_refined(t1.id(schema), &t1.ty, t2.id(schema), &t2.ty) {
                        // x_i ↦ T_{i1} ⊔ T'_i
                        Some((rid, refined)) => {
                            let (jid, joined) =
                                schema.join_interned(t1.id(schema), &t1.ty, rid, &refined);
                            Binding::with_id(joined, jid)
                        }
                        // Collapse marker ⇒ T'_i = Zero and
                        // join(T_{i1}, Zero) = T_{i1}: keep the left
                        // binding, as the uncached path did.
                        None => t1.clone(),
                    }
                }
                // x_j ↦ T_j (left-only, kept as-is).
                None => t1.clone(),
            };
            result.insert(key.clone(), merged);
        }

        // Right-only keys (k): T_k ⊔ Null.
        for (key, t2) in &b.bindings {
            if a.bindings.contains_key(key) {
                continue;
            }
            let (jid, joined) = schema.join_interned(t2.id(schema), &t2.ty, null_id, &null_rc);
            result.insert(key.clone(), Binding::with_id(joined, jid));
        }

        TypeEnvironment { bindings: result }
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.values().any(|b| b.ty.is_empty())
    }

    /// Wrap each binding in `VariableType::Group`. Used for repeated/quantified
    /// patterns where variables become groups of values. (Mirrors fppc's
    /// `to_list` — gqlite uses `Group` for the same role.)
    pub fn to_group(&self) -> TypeEnvironment {
        super::stats::record_env_to_group();
        TypeEnvironment {
            bindings: self
                .bindings
                .iter()
                .map(|(k, b)| {
                    (
                        k.clone(),
                        Binding::new(Rc::new(VariableType::Group(Box::new(
                            b.ty.as_ref().clone(),
                        )))),
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
        let u = TypeEnvironment::union(&Schema::star(), &a, &b);
        // Equal types collapse under join.
        assert_eq!(u.get("x"), Some(&nstar()));
    }

    #[test]
    fn test_union_key_only_in_left_joins_with_null() {
        let mut a = TypeEnvironment::new();
        let b = TypeEnvironment::new();
        a.set("x", nstar());
        let u = TypeEnvironment::union(&Schema::star(), &a, &b);
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
        let u = TypeEnvironment::union(&Schema::star(), &a, &b);
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
        assert_eq!(r.keys().count(), 1);
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
