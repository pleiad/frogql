use std::fmt;

/// Atomic and composite types for property values.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

    /// Greatest lower bound (meet).
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
                    && a.iter().all(|(k, v)| b.get(k).map_or(false, |bv| SimpleType::is_subtype(v, bv)))
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
                let parts: Vec<String> = fields.iter().map(|(k, v)| format!("{k} is {v}")).collect();
                write!(f, "{{{}}}", parts.join(", "))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_meet_same() {
        assert_eq!(SimpleType::meet(&SimpleType::Z, &SimpleType::Z), SimpleType::Z);
    }

    #[test]
    fn test_meet_different() {
        assert_eq!(SimpleType::meet(&SimpleType::Z, &SimpleType::S), SimpleType::Zero);
    }

    #[test]
    fn test_meet_star() {
        assert_eq!(SimpleType::meet(&SimpleType::Star, &SimpleType::Z), SimpleType::Z);
        assert_eq!(SimpleType::meet(&SimpleType::Z, &SimpleType::Star), SimpleType::Z);
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
        assert_eq!(SimpleType::union(&SimpleType::Zero, &SimpleType::Z), SimpleType::Z);
        assert_eq!(SimpleType::union(&SimpleType::Z, &SimpleType::Zero), SimpleType::Z);
    }

    #[test]
    fn test_is_empty() {
        assert!(SimpleType::Zero.is_empty());
        assert!(!SimpleType::Z.is_empty());
        let u = SimpleType::Union(Box::new(SimpleType::Zero), Box::new(SimpleType::Zero));
        assert!(u.is_empty());
    }
}
