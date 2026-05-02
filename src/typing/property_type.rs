use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::simple_type::SimpleType;

/// Property types describe the record structure of node/edge properties.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropertyType {
    /// Open record — allows extra unspecified attributes (returns Star for unknown keys)
    Open(BTreeMap<String, SimpleType>),
    /// Closed record — exact attributes only (returns Zero for unknown keys)
    Closed(BTreeMap<String, SimpleType>),
    /// Bottom — inconsistent property type
    Zero,
}

impl PropertyType {
    pub fn open_empty() -> Self {
        PropertyType::Open(BTreeMap::new())
    }

    pub fn closed_empty() -> Self {
        PropertyType::Closed(BTreeMap::new())
    }

    /// Add or update an attribute.
    pub fn extend(&mut self, attr: String, t: SimpleType) {
        match self {
            PropertyType::Open(m) | PropertyType::Closed(m) => {
                m.insert(attr, t);
            }
            PropertyType::Zero => {}
        }
    }

    /// Get the type of an attribute.
    pub fn get(&self, attr: &str) -> SimpleType {
        match self {
            PropertyType::Open(m) => m.get(attr).cloned().unwrap_or(SimpleType::Star),
            PropertyType::Closed(m) => m.get(attr).cloned().unwrap_or(SimpleType::Zero),
            PropertyType::Zero => SimpleType::Zero,
        }
    }

    /// Greatest lower bound.
    pub fn meet(a: &PropertyType, b: &PropertyType) -> PropertyType {
        match (a, b) {
            // Both closed with identical key-sets: pointwise meet on values.
            (PropertyType::Closed(ma), PropertyType::Closed(mb))
                if ma.len() == mb.len() && ma.keys().eq(mb.keys()) =>
            {
                let m = ma
                    .iter()
                    .map(|(k, va)| (k.clone(), SimpleType::meet(va, &mb[k])))
                    .collect();
                PropertyType::Closed(m)
            }
            // Both open: pointwise meet on shared keys, union of the rest.
            (PropertyType::Open(ma), PropertyType::Open(mb)) => {
                let mut out: BTreeMap<String, SimpleType> = ma
                    .iter()
                    .map(|(k, va)| {
                        let t = match mb.get(k) {
                            Some(vb) => SimpleType::meet(va, vb),
                            None => va.clone(),
                        };
                        (k.clone(), t)
                    })
                    .collect();
                for (k, vb) in mb {
                    out.entry(k.clone()).or_insert_with(|| vb.clone());
                }
                PropertyType::Open(out)
            }
            // Open ⊓ Closed where open keys ⊆ closed keys.
            (PropertyType::Open(ma), PropertyType::Closed(mb)) => {
                if !ma.keys().all(|k| mb.contains_key(k)) {
                    return PropertyType::Zero;
                }
                let m = mb
                    .iter()
                    .map(|(k, vb)| {
                        let t = match ma.get(k) {
                            Some(va) => SimpleType::meet(va, vb),
                            None => vb.clone(),
                        };
                        (k.clone(), t)
                    })
                    .collect();
                PropertyType::Closed(m)
            }
            // Closed ⊓ Open (symmetric)
            (PropertyType::Closed(_), PropertyType::Open(_)) => PropertyType::meet(b, a),
            _ => PropertyType::Zero,
        }
    }

    /// Subtyping.
    pub fn is_subtype(t1: &PropertyType, t2: &PropertyType) -> bool {
        match (t1, t2) {
            (PropertyType::Zero, _) => true,
            // Open <: Open: constraint applies only to keys present in both.
            (PropertyType::Open(m1), PropertyType::Open(m2)) => {
                m1.iter().all(|(k, t1)| match m2.get(k) {
                    Some(t2) => SimpleType::is_subtype(t1, t2),
                    None => true,
                })
            }
            // Closed <: Closed: same key-set, pointwise subtype.
            (PropertyType::Closed(m1), PropertyType::Closed(m2)) => {
                m1.len() == m2.len()
                    && m1.keys().eq(m2.keys())
                    && m1.iter().all(|(k, v1)| SimpleType::is_subtype(v1, &m2[k]))
            }
            // Closed <: Open: Closed must cover every Open key.
            (PropertyType::Closed(m1), PropertyType::Open(m2)) => {
                m2.iter().all(|(k, v2)| match m1.get(k) {
                    Some(v1) => SimpleType::is_subtype(v1, v2),
                    None => false,
                })
            }
            // Open <: Closed: Open's key-set must be ⊆ Closed's.
            (PropertyType::Open(m1), PropertyType::Closed(m2)) => {
                m1.iter().all(|(k, v1)| match m2.get(k) {
                    Some(v2) => SimpleType::is_subtype(v1, v2),
                    None => false,
                })
            }
            _ => false,
        }
    }

    /// True if any attribute is bottom.
    pub fn is_empty(&self) -> bool {
        match self {
            PropertyType::Open(m) | PropertyType::Closed(m) => {
                !m.is_empty() && m.values().any(|t| t.is_empty())
            }
            PropertyType::Zero => true,
        }
    }
}

impl fmt::Display for PropertyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PropertyType::Open(m) if m.is_empty() => write!(f, "{{*}}"),
            PropertyType::Open(m) => {
                let entries: Vec<String> = m.iter().map(|(k, v)| format!("{k}:{v}")).collect();
                write!(f, "{{{},*}}", entries.join(","))
            }
            PropertyType::Closed(m) if m.is_empty() => write!(f, "{{}}"),
            PropertyType::Closed(m) => {
                let entries: Vec<String> = m.iter().map(|(k, v)| format!("{k}:{v}")).collect();
                write!(f, "{{{}}}", entries.join(","))
            }
            PropertyType::Zero => write!(f, "⊥"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed(fields: &[(&str, SimpleType)]) -> PropertyType {
        let m: BTreeMap<String, SimpleType> = fields
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect();
        PropertyType::Closed(m)
    }
    fn open(fields: &[(&str, SimpleType)]) -> PropertyType {
        let m: BTreeMap<String, SimpleType> = fields
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect();
        PropertyType::Open(m)
    }

    // is_empty — quirk: empty record is NOT empty, but a non-empty
    // record with any empty field IS.
    #[test]
    fn test_zero_is_empty() {
        assert!(PropertyType::Zero.is_empty());
    }
    #[test]
    fn test_empty_record_is_not_empty() {
        // Per impl: `!m.is_empty() && m.values().any(...)`. Empty maps
        // fail the first conjunct, so are NOT empty.
        assert!(!PropertyType::open_empty().is_empty());
        assert!(!PropertyType::closed_empty().is_empty());
    }
    #[test]
    fn test_record_with_empty_field_is_empty() {
        let r_open = open(&[("a", SimpleType::Zero)]);
        let r_closed = closed(&[("a", SimpleType::Zero)]);
        assert!(r_open.is_empty());
        assert!(r_closed.is_empty());
    }
    #[test]
    fn test_record_with_only_full_fields_is_not_empty() {
        let r_open = open(&[("a", SimpleType::Z)]);
        let r_closed = closed(&[("a", SimpleType::Z)]);
        assert!(!r_open.is_empty());
        assert!(!r_closed.is_empty());
    }

    // meet — record preservation + Zero handling.
    #[test]
    fn test_meet_same_closed_returns_same() {
        let r = closed(&[("a", SimpleType::Z)]);
        assert_eq!(PropertyType::meet(&r, &r), r);
    }
    #[test]
    fn test_meet_different_closed_keys_yields_zero() {
        // gqlite treats records-with-different-keys as incompatible.
        let r1 = closed(&[("a", SimpleType::Z)]);
        let r2 = closed(&[("b", SimpleType::Z)]);
        assert_eq!(PropertyType::meet(&r1, &r2), PropertyType::Zero);
    }

    // ----- subtyping -----

    #[test]
    fn test_subtype_zero_is_bottom() {
        let any = open(&[("a", SimpleType::Z)]);
        assert!(PropertyType::is_subtype(&PropertyType::Zero, &any));
    }

    #[test]
    fn test_subtype_open_open_constrains_only_shared_keys() {
        // Open <: Open ignores keys not present in both.
        let m1 = open(&[("a", SimpleType::Z), ("b", SimpleType::S)]);
        let m2 = open(&[("a", SimpleType::Z), ("c", SimpleType::B)]);
        assert!(PropertyType::is_subtype(&m1, &m2));
        assert!(PropertyType::is_subtype(&m2, &m1));
    }

    #[test]
    fn test_subtype_open_open_fails_on_shared_key_mismatch() {
        let m1 = open(&[("a", SimpleType::Z)]);
        let m2 = open(&[("a", SimpleType::S)]); // same key, incompatible type
        assert!(!PropertyType::is_subtype(&m1, &m2));
    }

    #[test]
    fn test_subtype_closed_closed_requires_equal_keysets() {
        let m1 = closed(&[("a", SimpleType::Z)]);
        let m2 = closed(&[("a", SimpleType::Z), ("b", SimpleType::S)]);
        assert!(!PropertyType::is_subtype(&m1, &m2));
        assert!(!PropertyType::is_subtype(&m2, &m1));
    }

    #[test]
    fn test_subtype_closed_under_open_requires_open_keys_subset_of_closed() {
        // Closed <: Open: every key of Open must exist in Closed (and types match).
        let m_closed = closed(&[("a", SimpleType::Z), ("b", SimpleType::S)]);
        let m_open_sub = open(&[("a", SimpleType::Z)]);
        assert!(PropertyType::is_subtype(&m_closed, &m_open_sub));
        let m_open_extra = open(&[("a", SimpleType::Z), ("c", SimpleType::B)]);
        assert!(!PropertyType::is_subtype(&m_closed, &m_open_extra));
    }

    #[test]
    fn test_subtype_open_under_closed_requires_open_keys_subset_of_closed() {
        let m_closed = closed(&[("a", SimpleType::Z), ("b", SimpleType::S)]);
        let m_open_sub = open(&[("a", SimpleType::Z)]);
        assert!(PropertyType::is_subtype(&m_open_sub, &m_closed));
        let m_open_extra = open(&[("a", SimpleType::Z), ("c", SimpleType::B)]);
        assert!(!PropertyType::is_subtype(&m_open_extra, &m_closed));
    }

    // ----- meet -----

    #[test]
    fn test_meet_open_open_unions_keys_pointwise() {
        let m1 = open(&[("a", SimpleType::Z)]);
        let m2 = open(&[("b", SimpleType::S)]);
        let met = PropertyType::meet(&m1, &m2);
        let expected = open(&[("a", SimpleType::Z), ("b", SimpleType::S)]);
        assert_eq!(met, expected);
    }

    #[test]
    fn test_meet_open_open_meets_shared_simple_types() {
        // Shared key with compatible types meets to their SimpleType meet.
        let m1 = open(&[("a", SimpleType::Z)]);
        let m2 = open(&[("a", SimpleType::Z)]);
        assert_eq!(PropertyType::meet(&m1, &m2), m1);
    }

    #[test]
    fn test_meet_open_closed_subset_yields_closed() {
        let m_open = open(&[("a", SimpleType::Z)]);
        let m_closed = closed(&[("a", SimpleType::Z), ("b", SimpleType::S)]);
        let met = PropertyType::meet(&m_open, &m_closed);
        assert_eq!(met, m_closed);
    }

    #[test]
    fn test_meet_open_closed_extra_keys_yields_zero() {
        let m_open = open(&[("a", SimpleType::Z), ("c", SimpleType::B)]);
        let m_closed = closed(&[("a", SimpleType::Z), ("b", SimpleType::S)]);
        assert_eq!(PropertyType::meet(&m_open, &m_closed), PropertyType::Zero);
    }

    #[test]
    fn test_meet_closed_open_symmetric_with_open_closed() {
        let m_open = open(&[("a", SimpleType::Z)]);
        let m_closed = closed(&[("a", SimpleType::Z), ("b", SimpleType::S)]);
        assert_eq!(
            PropertyType::meet(&m_open, &m_closed),
            PropertyType::meet(&m_closed, &m_open),
        );
    }
}
