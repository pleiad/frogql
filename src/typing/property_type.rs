use std::collections::{BTreeMap, BTreeSet};
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
            // Both closed with identical keys
            (PropertyType::Closed(ma), PropertyType::Closed(mb))
                if ma.keys().collect::<BTreeSet<_>>() == mb.keys().collect::<BTreeSet<_>>() =>
            {
                let m = ma
                    .keys()
                    .map(|k| (k.clone(), SimpleType::meet(&ma[k], &mb[k])))
                    .collect();
                PropertyType::Closed(m)
            }
            // Both open
            (PropertyType::Open(ma), PropertyType::Open(mb)) => {
                let all_keys: BTreeSet<_> = ma.keys().chain(mb.keys()).cloned().collect();
                let m = all_keys
                    .into_iter()
                    .map(|k| {
                        let t = match (ma.get(&k), mb.get(&k)) {
                            (Some(ta), Some(tb)) => SimpleType::meet(ta, tb),
                            (Some(ta), None) => ta.clone(),
                            (None, Some(tb)) => tb.clone(),
                            (None, None) => unreachable!(),
                        };
                        (k, t)
                    })
                    .collect();
                PropertyType::Open(m)
            }
            // Open ⊓ Closed where open keys ⊆ closed keys
            (PropertyType::Open(ma), PropertyType::Closed(mb)) => {
                let ak: BTreeSet<_> = ma.keys().cloned().collect();
                let bk: BTreeSet<_> = mb.keys().cloned().collect();
                if ak.is_subset(&bk) {
                    let m = bk
                        .into_iter()
                        .map(|k| {
                            let t = if let Some(ta) = ma.get(&k) {
                                SimpleType::meet(ta, &mb[&k])
                            } else {
                                mb[&k].clone()
                            };
                            (k, t)
                        })
                        .collect();
                    PropertyType::Closed(m)
                } else {
                    PropertyType::Zero
                }
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
            (PropertyType::Open(m1), PropertyType::Open(m2)) => {
                let shared: BTreeSet<_> =
                    m1.keys().filter(|k| m2.contains_key(*k)).cloned().collect();
                shared
                    .iter()
                    .all(|k| SimpleType::is_subtype(&m1[k], &m2[k]))
            }
            (PropertyType::Closed(m1), PropertyType::Closed(m2)) => {
                let k1: BTreeSet<_> = m1.keys().cloned().collect();
                let k2: BTreeSet<_> = m2.keys().cloned().collect();
                k1 == k2 && k2.iter().all(|k| SimpleType::is_subtype(&m1[k], &m2[k]))
            }
            (PropertyType::Closed(m1), PropertyType::Open(m2)) => {
                let k1: BTreeSet<_> = m1.keys().cloned().collect();
                let k2: BTreeSet<_> = m2.keys().cloned().collect();
                k2.is_subset(&k1) && k2.iter().all(|k| SimpleType::is_subtype(&m1[k], &m2[k]))
            }
            (PropertyType::Open(m1), PropertyType::Closed(m2)) => {
                let k1: BTreeSet<_> = m1.keys().cloned().collect();
                let k2: BTreeSet<_> = m2.keys().cloned().collect();
                k1.is_subset(&k2) && k1.iter().all(|k| SimpleType::is_subtype(&m1[k], &m2[k]))
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
}
