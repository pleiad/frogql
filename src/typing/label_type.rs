use std::fmt;

use serde::{Deserialize, Serialize};

/// Label types form a boolean algebra over graph element labels.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LabelType {
    /// Atomic label (e.g., "Person", "Transfer")
    Label(String),
    /// Wildcard — matches any label
    Star,
    /// Top — "has any known label"
    Top,
    /// Empty — absence of a label, subtype of everything
    Empty,
    /// Conjunction: l1 & l2
    And(Box<LabelType>, Box<LabelType>),
    /// Disjunction: l1 | l2
    Or(Box<LabelType>, Box<LabelType>),
    /// Negation: !l
    Neg(Box<LabelType>),
}

impl LabelType {
    /// Construct a conjunction from a list of label strings.
    /// `["A", "B", "C"]` → `A & B & C`
    pub fn from_list(labels: &[String]) -> LabelType {
        assert!(!labels.is_empty(), "label list must be non-empty");
        if labels.len() == 1 {
            LabelType::Label(labels[0].clone())
        } else {
            LabelType::And(
                Box::new(LabelType::Label(labels[0].clone())),
                Box::new(LabelType::from_list(&labels[1..])),
            )
        }
    }

    /// Greatest lower bound under logical entailment: `ℓ₁ ⊓ ℓ₂ = ℓ₁ & ℓ₂`.
    ///
    /// **There is no special case for `Star`.** The clause `ℓ ⊓ ★ = ℓ`
    /// looks harmless and is not: `★` is the *unknown* label, not a top,
    /// so dropping it throws away the imprecision the program wrote and
    /// the meet stops being monotone in precision. Keeping the conjunct
    /// costs nothing at query time, because `is_subtype` is optimistic on
    /// `★` and `A & ★` therefore admits everything `A` admits.
    ///
    /// The same argument rules out shortcutting on `is_subtype`: that
    /// relation is *consistent* subtyping, so `A|B <: A` holds, and
    /// `(A|B) ⊓ A = A|B` would not be below `A`.
    ///
    /// `Top` keeps its clause. It is the genuine top of label subtyping,
    /// so `ℓ & 1 = ℓ` is a sound simplification rather than a guess.
    pub fn meet(a: &LabelType, b: &LabelType) -> LabelType {
        match (a, b) {
            (LabelType::Top, _) => b.clone(),
            (_, LabelType::Top) => a.clone(),
            _ => {
                let mut parts = Vec::new();
                a.push_conjuncts(&mut parts);
                b.push_conjuncts(&mut parts);
                LabelType::and_of(parts)
            }
        }
    }

    /// Flatten an `&`-chain into its conjuncts, left to right.
    fn push_conjuncts<'a>(&'a self, out: &mut Vec<&'a LabelType>) {
        match self {
            LabelType::And(x, y) => {
                x.push_conjuncts(out);
                y.push_conjuncts(out);
            }
            _ => out.push(self),
        }
    }

    /// Rebuild a right-nested `&`-chain, dropping repeated conjuncts.
    ///
    /// Idempotence (`ℓ & ℓ = ℓ`) is a genuine identity of the boolean
    /// algebra *and* of precision, so collapsing duplicates is a
    /// normalisation rather than a guess — unlike `ℓ & ★ = ℓ`, which
    /// throws away imprecision the program wrote. Without it a long
    /// concat chain keeps re-conjoining the same `★` and the label tree
    /// grows once per meet.
    fn and_of(parts: Vec<&LabelType>) -> LabelType {
        let mut uniq: Vec<&LabelType> = Vec::with_capacity(parts.len());
        for p in parts {
            if !uniq.contains(&p) {
                uniq.push(p);
            }
        }
        let mut iter = uniq.into_iter().rev();
        let last = iter.next().expect("meet operands contribute a conjunct");
        iter.fold(last.clone(), |acc, p| {
            LabelType::And(Box::new(p.clone()), Box::new(acc))
        })
    }

    /// Subtyping under label entailment.
    pub fn is_subtype(l1: &LabelType, l2: &LabelType) -> bool {
        match (l1, l2) {
            (LabelType::Star, _) | (_, LabelType::Star) => true,
            (_, LabelType::Top) => true,
            (LabelType::Label(a), LabelType::Label(b)) => a == b,
            (_, LabelType::And(a, b)) => {
                LabelType::is_subtype(l1, a) && LabelType::is_subtype(l1, b)
            }
            (_, LabelType::Or(a, b)) => {
                LabelType::is_subtype(l1, a) || LabelType::is_subtype(l1, b)
            }
            (LabelType::And(a, b), _) => {
                LabelType::is_subtype(a, l2) || LabelType::is_subtype(b, l2)
            }
            (LabelType::Or(a, b), _) => {
                LabelType::is_subtype(a, l2) || LabelType::is_subtype(b, l2)
            }
            (_, LabelType::Neg(inner)) => !LabelType::is_subtype(l1, inner),
            (LabelType::Empty, _) => true,
            _ => false,
        }
    }

    /// Whether a label type is inconsistent (e.g., A & !A).
    /// Currently unimplemented — always returns false (matches Python).
    pub fn is_empty(&self) -> bool {
        false
    }

    /// If this is a simple atomic label, return its name.
    pub fn as_simple_label(&self) -> Option<&str> {
        match self {
            LabelType::Label(s) => Some(s),
            _ => None,
        }
    }

    /// Collect all positive leaf labels that MUST be present (from And and bare Labels).
    /// Does not include labels under Or or Neg (those can't narrow the search).
    pub fn required_labels(&self) -> Vec<&str> {
        match self {
            LabelType::Label(s) => vec![s.as_str()],
            LabelType::And(a, b) => {
                let mut v = a.required_labels();
                v.extend(b.required_labels());
                v
            }
            _ => vec![],
        }
    }

    /// Or-of-leaves candidate labels — for the btree multi-label merge
    /// path. Returns `Some([X, Y, …])` when the type is a pure
    /// disjunction of bare labels (e.g. `Comment | Post`), so a node
    /// satisfying the type carries at least one of the listed labels.
    /// Returns `None` for shapes that the merge can't safely cover:
    ///   - Anything with `And` / `Neg` / `Star` / `Top` / `Empty`. The
    ///     `required_labels()` path already handles And-of-Labels via a
    ///     single btree pick; mixing And under Or would over-include
    ///     candidates and the LTJ filter chain may not correctly
    ///     re-prune them in this code path.
    ///
    /// The returned slice is unordered and may contain duplicates;
    /// callers should sort + dedup if they rely on uniqueness.
    pub fn or_candidate_labels(&self) -> Option<Vec<&str>> {
        match self {
            LabelType::Label(s) => Some(vec![s.as_str()]),
            LabelType::Or(a, b) => {
                let mut left = a.or_candidate_labels()?;
                left.extend(b.or_candidate_labels()?);
                Some(left)
            }
            _ => None,
        }
    }
}

impl fmt::Display for LabelType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LabelType::Label(s) => write!(f, "{s}"),
            LabelType::Star => write!(f, "*"),
            LabelType::Top => write!(f, "⊤"),
            LabelType::Empty => write!(f, "ε"),
            LabelType::And(a, b) => write!(f, "({a} & {b})"),
            LabelType::Or(a, b) => write!(f, "({a} | {b})"),
            LabelType::Neg(a) => write!(f, "!({a})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_list_single() {
        assert_eq!(
            LabelType::from_list(&["Person".into()]),
            LabelType::Label("Person".into())
        );
    }

    #[test]
    fn test_from_list_multi() {
        let lt = LabelType::from_list(&["Dummy".into(), "Person".into()]);
        assert_eq!(
            lt,
            LabelType::And(
                Box::new(LabelType::Label("Dummy".into())),
                Box::new(LabelType::Label("Person".into())),
            )
        );
    }

    #[test]
    fn test_subtype_star() {
        assert!(LabelType::is_subtype(
            &LabelType::Star,
            &LabelType::Label("X".into())
        ));
        assert!(LabelType::is_subtype(
            &LabelType::Label("X".into()),
            &LabelType::Star
        ));
    }

    #[test]
    fn test_subtype_and() {
        // A & B <: A
        let ab = LabelType::And(
            Box::new(LabelType::Label("A".into())),
            Box::new(LabelType::Label("B".into())),
        );
        assert!(LabelType::is_subtype(&ab, &LabelType::Label("A".into())));
        assert!(LabelType::is_subtype(&ab, &LabelType::Label("B".into())));
    }

    #[test]
    fn test_subtype_label_eq() {
        assert!(LabelType::is_subtype(
            &LabelType::Label("X".into()),
            &LabelType::Label("X".into())
        ));
        assert!(!LabelType::is_subtype(
            &LabelType::Label("X".into()),
            &LabelType::Label("Y".into())
        ));
    }

    // meet — Star identity, distinct atoms produce And.
    #[test]
    fn test_meet_same_atom_returns_atom() {
        let a = LabelType::Label("A".into());
        assert_eq!(LabelType::meet(&a, &a), a);
    }
    #[test]
    fn test_meet_keeps_left_star() {
        // The paper prints `★ ⊓ ℓ = ℓ`; that clause is what makes the
        // static gradual guarantee false, and the Lean development drops
        // it. `★` is the *unknown* label, not a top, so the meet keeps it
        // as a conjunct and the result stays as imprecise as written.
        let a = LabelType::Label("A".into());
        assert_eq!(
            LabelType::meet(&LabelType::Star, &a),
            LabelType::And(Box::new(LabelType::Star), Box::new(a))
        );
    }
    #[test]
    fn test_meet_keeps_right_star() {
        let a = LabelType::Label("A".into());
        assert_eq!(
            LabelType::meet(&a, &LabelType::Star),
            LabelType::And(Box::new(a), Box::new(LabelType::Star))
        );
    }
    #[test]
    fn test_meet_with_star_still_admits_what_the_operand_admits() {
        // Keeping the conjunct costs no matching power: subtyping is
        // optimistic on `★`, so `A & ★` accepts everything `A` accepts.
        let a = LabelType::Label("A".into());
        let m = LabelType::meet(&LabelType::Star, &a);
        assert!(LabelType::is_subtype(&m, &a));
        assert!(LabelType::is_subtype(&a, &m));
    }
    #[test]
    fn test_meet_distinct_atoms_yields_and() {
        // §4.1: 1₁ ⊓ 1₂ = 1₁ & 1₂. A regression returning Star, or Or,
        // or just one operand, would all fail.
        let a = LabelType::Label("A".into());
        let b = LabelType::Label("B".into());
        let m = LabelType::meet(&a, &b);
        assert!(
            matches!(&m, LabelType::And(_, _)),
            "meet of distinct atoms should be And, got {m:?}"
        );
    }
}
