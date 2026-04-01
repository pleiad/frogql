use std::fmt;

/// Atomic and composite types for property values.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SimpleType {
    /// Integer type
    Z,
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
    /// List type (used for repetition grouping)
    List(Box<SimpleType>),
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
            (_, SimpleType::Zero) => true,
            (SimpleType::Union(a, b), _) => {
                SimpleType::is_subtype(a, t2) || SimpleType::is_subtype(b, t2)
            }
            (_, SimpleType::Union(a, b)) => {
                SimpleType::is_subtype(t1, a) || SimpleType::is_subtype(t1, b)
            }
            _ => t1 == t2,
        }
    }

    /// True if the type is empty (bottom or composed of bottoms).
    pub fn is_empty(&self) -> bool {
        match self {
            SimpleType::Zero => true,
            SimpleType::Union(a, b) => a.is_empty() && b.is_empty(),
            SimpleType::List(t) => t.is_empty(),
            _ => false,
        }
    }
}

impl fmt::Display for SimpleType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SimpleType::Z => write!(f, "int"),
            SimpleType::B => write!(f, "bool"),
            SimpleType::S => write!(f, "str"),
            SimpleType::Star => write!(f, "*"),
            SimpleType::Zero => write!(f, "⊥"),
            SimpleType::Union(a, b) => write!(f, "{a} | {b}"),
            SimpleType::List(t) => write!(f, "[{t}]"),
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
