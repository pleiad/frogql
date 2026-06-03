use std::fmt;

use serde::{Deserialize, Serialize};

/// Atomic and composite types for property values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SimpleType {
    /// Integer type
    Z,
    /// Floating-point type (f64)
    F,
    /// Boolean type
    B,
    /// String type
    S,
    /// Imprecise/unknown type (wildcard) — matches any type
    Star,
    /// Bottom type — contradiction/conflict
    Zero,
    /// Union of two types
    Union(Box<SimpleType>, Box<SimpleType>),
    /// Repetition-grouping type (from `{n,m}` quantifiers). Name chosen to reserve
    /// the word `List` for user-facing list values (ISO GQL list type).
    Group(Box<SimpleType>),
    /// User-facing list type: `[T]` in surface syntax.
    List(Box<SimpleType>),
    /// Record (nested) type. Closed: must have exactly these fields. Syntax:
    /// `{k is T, k2 is T2}`. Distinct from a descriptor's PropertyType — this
    /// appears at value positions, and can nest (a field's type can itself be Record).
    Record(std::collections::BTreeMap<String, SimpleType>),
    /// ISO §4.4.4 reference value type for nodes. Carries identity, not
    /// structure. Two values of this type are equal iff their ids match
    /// (same referent); they are not orderable without Feature GA04.
    Node,
    /// ISO §4.4.4 reference value type for edges. Same identity-based
    /// equality and the same non-orderable restriction as `Node`.
    Edge,
    /// ISO §4.4 PATH value type — the static type of a named path
    /// variable (`MATCH p = ...`) and the argument type of the §20.16
    /// path functions. Terminal in the lattice: it only meets/subtypes
    /// itself, never carries structure, and never appears in a persisted
    /// schema (a path cannot be a property value).
    Path,
}

impl SimpleType {
    /// Least upper bound (gradual union).
    pub fn union(a: &SimpleType, b: &SimpleType) -> SimpleType {
        if *a == SimpleType::Zero {
            return b.clone();
        }
        if *b == SimpleType::Zero {
            return a.clone();
        }
        if a == b {
            return a.clone();
        }
        SimpleType::Union(Box::new(a.clone()), Box::new(b.clone()))
    }

    /// Greatest lower bound. Composite types meet recursively on inner
    /// types (e.g. `meet(List[int], List[int|str]) = List[int]`).
    /// Records with different key sets are distinct types → Zero.
    pub fn meet(a: &SimpleType, b: &SimpleType) -> SimpleType {
        match (a, b) {
            (SimpleType::Star, _) => b.clone(),
            (_, SimpleType::Star) => a.clone(),
            (SimpleType::Union(t1, t2), _) => {
                SimpleType::union(&SimpleType::meet(t1, b), &SimpleType::meet(t2, b))
            }
            (_, SimpleType::Union(t1, t2)) => {
                SimpleType::union(&SimpleType::meet(a, t1), &SimpleType::meet(a, t2))
            }
            (SimpleType::List(a), SimpleType::List(b)) => {
                SimpleType::List(Box::new(SimpleType::meet(a, b)))
            }
            (SimpleType::Group(a), SimpleType::Group(b)) => {
                SimpleType::Group(Box::new(SimpleType::meet(a, b)))
            }
            (SimpleType::Record(a), SimpleType::Record(b)) => {
                // BTreeMap keys are ordered, so iterator comparison is O(n).
                if a.len() != b.len() || !a.keys().zip(b.keys()).all(|(ka, kb)| ka == kb) {
                    SimpleType::Zero
                } else {
                    let met: std::collections::BTreeMap<String, SimpleType> = a
                        .iter()
                        .map(|(k, va)| {
                            let vb = b.get(k).expect("keys verified equal above");
                            (k.clone(), SimpleType::meet(va, vb))
                        })
                        .collect();
                    SimpleType::Record(met)
                }
            }
            _ if a == b => a.clone(),
            _ => SimpleType::Zero,
        }
    }

    /// Gradual subtyping.
    pub fn is_subtype(t1: &SimpleType, t2: &SimpleType) -> bool {
        match (t1, t2) {
            (SimpleType::Star, _) | (_, SimpleType::Star) => true,
            (SimpleType::Zero, _) => true,
            (SimpleType::Union(a, b), _) => {
                SimpleType::is_subtype(a, t2) || SimpleType::is_subtype(b, t2)
            }
            (_, SimpleType::Union(a, b)) => {
                SimpleType::is_subtype(t1, a) || SimpleType::is_subtype(t1, b)
            }
            // Covariance on constructed types: `List(A) <: List(B)` iff `A <: B`.
            // Same for Group — internal repetition grouping.
            (SimpleType::List(a), SimpleType::List(b)) => SimpleType::is_subtype(a, b),
            (SimpleType::Group(a), SimpleType::Group(b)) => SimpleType::is_subtype(a, b),
            // Records: same field set, each field covariant.
            (SimpleType::Record(a), SimpleType::Record(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, v)| b.get(k).is_some_and(|bv| SimpleType::is_subtype(v, bv)))
            }
            _ => t1 == t2,
        }
    }

    /// True if the type is empty (bottom or composed of bottoms).
    pub fn is_empty(&self) -> bool {
        match self {
            SimpleType::Zero => true,
            SimpleType::Union(a, b) => a.is_empty() && b.is_empty(),
            SimpleType::Group(t) => t.is_empty(),
            SimpleType::List(t) => t.is_empty(),
            SimpleType::Record(fields) => fields.values().any(|t| t.is_empty()),
            _ => false,
        }
    }
}

impl fmt::Display for SimpleType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SimpleType::Z => write!(f, "int"),
            SimpleType::F => write!(f, "float"),
            SimpleType::B => write!(f, "bool"),
            SimpleType::S => write!(f, "str"),
            SimpleType::Star => write!(f, "*"),
            SimpleType::Zero => write!(f, "⊥"),
            SimpleType::Union(a, b) => write!(f, "{a} | {b}"),
            SimpleType::Group(t) => write!(f, "group<{t}>"),
            SimpleType::List(t) => write!(f, "[{t}]"),
            SimpleType::Record(fields) => {
                let parts: Vec<String> = fields.iter().map(|(k, v)| format!("{k} {v}")).collect();
                write!(f, "{{{}}}", parts.join(", "))
            }
            SimpleType::Node => write!(f, "node"),
            SimpleType::Edge => write!(f, "edge"),
            SimpleType::Path => write!(f, "path"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_meet_same() {
        assert_eq!(
            SimpleType::meet(&SimpleType::Z, &SimpleType::Z),
            SimpleType::Z
        );
    }

    #[test]
    fn test_meet_different() {
        assert_eq!(
            SimpleType::meet(&SimpleType::Z, &SimpleType::S),
            SimpleType::Zero
        );
    }

    #[test]
    fn test_meet_star() {
        assert_eq!(
            SimpleType::meet(&SimpleType::Star, &SimpleType::Z),
            SimpleType::Z
        );
        assert_eq!(
            SimpleType::meet(&SimpleType::Z, &SimpleType::Star),
            SimpleType::Z
        );
    }

    #[test]
    fn test_subtype_star() {
        assert!(SimpleType::is_subtype(&SimpleType::Star, &SimpleType::Z));
        assert!(SimpleType::is_subtype(&SimpleType::Z, &SimpleType::Star));
    }

    #[test]
    fn test_subtype_same() {
        assert!(SimpleType::is_subtype(&SimpleType::Z, &SimpleType::Z));
        assert!(!SimpleType::is_subtype(&SimpleType::Z, &SimpleType::S));
    }

    #[test]
    fn test_union_with_zero() {
        assert_eq!(
            SimpleType::union(&SimpleType::Zero, &SimpleType::Z),
            SimpleType::Z
        );
        assert_eq!(
            SimpleType::union(&SimpleType::Z, &SimpleType::Zero),
            SimpleType::Z
        );
    }

    #[test]
    fn test_is_empty() {
        assert!(SimpleType::Zero.is_empty());
        assert!(!SimpleType::Z.is_empty());
        let u = SimpleType::Union(Box::new(SimpleType::Zero), Box::new(SimpleType::Zero));
        assert!(u.is_empty());
    }

    #[test]
    fn test_is_empty_atoms() {
        // Each non-bottom atom is non-empty.
        assert!(!SimpleType::F.is_empty());
        assert!(!SimpleType::B.is_empty());
        assert!(!SimpleType::S.is_empty());
        assert!(!SimpleType::Star.is_empty());
    }

    #[test]
    fn test_is_empty_union_short_circuits() {
        // Union(a, b) iff both empty.
        let z = || SimpleType::Zero;
        let one = || SimpleType::Z;
        assert!(SimpleType::Union(Box::new(z()), Box::new(z())).is_empty());
        assert!(!SimpleType::Union(Box::new(z()), Box::new(one())).is_empty());
        assert!(!SimpleType::Union(Box::new(one()), Box::new(z())).is_empty());
    }

    #[test]
    fn test_is_empty_group_and_list_propagate() {
        assert!(SimpleType::Group(Box::new(SimpleType::Zero)).is_empty());
        assert!(!SimpleType::Group(Box::new(SimpleType::Z)).is_empty());
        assert!(SimpleType::List(Box::new(SimpleType::Zero)).is_empty());
        assert!(!SimpleType::List(Box::new(SimpleType::Z)).is_empty());
    }

    #[test]
    fn test_is_empty_record_uses_any_semantics() {
        // Record(fields) iff fields.values().any(is_empty) — different
        // from Union which uses AND.
        let m_with_zero = {
            let mut m = std::collections::BTreeMap::new();
            m.insert("a".to_string(), SimpleType::Z);
            m.insert("b".to_string(), SimpleType::Zero);
            m
        };
        assert!(
            SimpleType::Record(m_with_zero).is_empty(),
            "record with one Zero field should be empty (any-semantics)"
        );

        // Empty record (no fields) is not empty per the impl.
        assert!(!SimpleType::Record(std::collections::BTreeMap::new()).is_empty());
    }

    #[test]
    fn test_union_collapses_equal_operands() {
        // Beyond test_union_with_zero — `union(X, X) = X`.
        assert_eq!(
            SimpleType::union(&SimpleType::Z, &SimpleType::Z),
            SimpleType::Z
        );
    }

    // meet on composite types — covariant recursion.

    fn list(t: SimpleType) -> SimpleType {
        SimpleType::List(Box::new(t))
    }
    fn group(t: SimpleType) -> SimpleType {
        SimpleType::Group(Box::new(t))
    }
    fn record(fields: &[(&str, SimpleType)]) -> SimpleType {
        let m: std::collections::BTreeMap<String, SimpleType> = fields
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect();
        SimpleType::Record(m)
    }
    fn union(a: SimpleType, b: SimpleType) -> SimpleType {
        SimpleType::Union(Box::new(a), Box::new(b))
    }

    #[test]
    fn test_meet_list_same_inner() {
        assert_eq!(
            SimpleType::meet(&list(SimpleType::Z), &list(SimpleType::Z)),
            list(SimpleType::Z)
        );
    }

    #[test]
    fn test_meet_list_covariant_with_union() {
        // meet(List[int], List[int|str]) = List[meet(int, int|str)] = List[int]
        let lhs = list(SimpleType::Z);
        let rhs = list(union(SimpleType::Z, SimpleType::S));
        assert_eq!(SimpleType::meet(&lhs, &rhs), list(SimpleType::Z));
    }

    #[test]
    fn test_meet_list_incompatible_inner_propagates_zero() {
        // meet(List[int], List[str]) = List[Zero]. The List wrapper survives;
        // is_empty on the result is true (a list whose element type is
        // impossible is itself impossible).
        let result = SimpleType::meet(&list(SimpleType::Z), &list(SimpleType::S));
        assert_eq!(result, list(SimpleType::Zero));
        assert!(result.is_empty());
    }

    #[test]
    fn test_meet_record_same_keys_recursive() {
        let lhs = record(&[("name", SimpleType::S), ("age", SimpleType::Z)]);
        let rhs = record(&[("name", SimpleType::S), ("age", SimpleType::Z)]);
        let expected = record(&[("name", SimpleType::S), ("age", SimpleType::Z)]);
        assert_eq!(SimpleType::meet(&lhs, &rhs), expected);
    }

    #[test]
    fn test_meet_record_covariant_field() {
        // meet({age: int}, {age: int|str}) = {age: int}
        let lhs = record(&[("age", SimpleType::Z)]);
        let rhs = record(&[("age", union(SimpleType::Z, SimpleType::S))]);
        let expected = record(&[("age", SimpleType::Z)]);
        assert_eq!(SimpleType::meet(&lhs, &rhs), expected);
    }

    #[test]
    fn test_meet_record_distinct_keys_is_zero() {
        let lhs = record(&[("a", SimpleType::Z)]);
        let rhs = record(&[("b", SimpleType::Z)]);
        assert_eq!(SimpleType::meet(&lhs, &rhs), SimpleType::Zero);
    }

    #[test]
    fn test_meet_record_different_arity_is_zero() {
        let lhs = record(&[("a", SimpleType::Z)]);
        let rhs = record(&[("a", SimpleType::Z), ("b", SimpleType::Z)]);
        assert_eq!(SimpleType::meet(&lhs, &rhs), SimpleType::Zero);
    }

    #[test]
    fn test_meet_record_incompatible_field_propagates() {
        // meet({a: int}, {a: str}) = {a: Zero}. Record survives, is_empty true.
        let lhs = record(&[("a", SimpleType::Z)]);
        let rhs = record(&[("a", SimpleType::S)]);
        let result = SimpleType::meet(&lhs, &rhs);
        assert_eq!(result, record(&[("a", SimpleType::Zero)]));
        assert!(result.is_empty());
    }

    #[test]
    fn test_meet_group_covariant() {
        // Group is the moral equivalent of List for repetition grouping;
        // same recursive treatment.
        let lhs = group(SimpleType::Z);
        let rhs = group(union(SimpleType::Z, SimpleType::S));
        assert_eq!(SimpleType::meet(&lhs, &rhs), group(SimpleType::Z));
    }

    #[test]
    fn test_meet_nested_list_of_records() {
        // meet(List[{a: int}], List[{a: int|str}]) = List[{a: int}]
        let lhs = list(record(&[("a", SimpleType::Z)]));
        let rhs = list(record(&[("a", union(SimpleType::Z, SimpleType::S))]));
        let expected = list(record(&[("a", SimpleType::Z)]));
        assert_eq!(SimpleType::meet(&lhs, &rhs), expected);
    }
}
