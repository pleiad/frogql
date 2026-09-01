pub mod assignment;
pub mod catalog;
pub mod dm;
pub mod engine;
pub mod ltj;
pub mod path_select;
pub mod result;
pub mod vsearch;

use crate::model::value::Value;
use crate::syntax::expr::BinOp;

/// Outcome of an ISO 3VL equality test.
///
/// Three outcomes rather than two, because "definitely not equal" and "these
/// operands have no common type" are different facts that different contexts
/// need to tell apart — see `EqVerdict::Mismatch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EqVerdict {
    /// A definite answer: the operands are comparable and this is the result.
    Definite(bool),
    /// Unknown: a null reached the comparison, so the answer is the null
    /// truth value (ISO has no separate `Unknown` — the null value *is* it).
    Unknown,
    /// The operands share no type, so equality is not defined on them.
    ///
    /// At the **top level** this is a type error: `1 = 'a'` reduces to an
    /// error value that propagates outward, dropping the row in `WHERE` and
    /// producing a null cell in `RETURN` (`ExprResult::Failure`).
    ///
    /// **Nested inside a composite** it degrades to `Definite(false)`, because
    /// there is no domain check down there to report a mismatch against — the
    /// element-wise comparison has to be total. `[1] = ['a']` is `false`,
    /// while the very same pair compared at the top is an error.
    Mismatch,
}

/// ISO 3VL equality, applied structurally through composite values.
///
/// `[1, null] = [1, null]` is `1 = 1 AND null = null`, so it is *unknown*,
/// not true — the same reason SQL gives unknown for a row comparison whose
/// only disagreement is a null.
///
/// Structure is decided before contents: two lists of different lengths, or
/// two records with different key sets, are definitely unequal whatever the
/// nulls inside them say. Within a matching shape the element verdicts are
/// folded with the Kleene conjunction — a definite `false` from any position
/// wins (one disagreeing element settles the whole comparison), and only
/// otherwise does an unknown position make the result unknown.
///
/// Distinct from `eq_value`, which backs GROUP BY / DISTINCT and deliberately
/// treats null as equal to null so grouping keys collapse (ISO §14.6 groups
/// nulls together). Equality-as-a-predicate and equality-as-a-grouping-key are
/// different relations; do not merge them.
pub fn eq_verdict(lhs: &Value, rhs: &Value) -> EqVerdict {
    if lhs.is_null() || rhs.is_null() {
        return EqVerdict::Unknown;
    }
    match (lhs, rhs) {
        (Value::List(x), Value::List(y)) | (Value::Path(x), Value::Path(y)) => {
            if x.len() != y.len() {
                return EqVerdict::Definite(false);
            }
            eq_fold(x.iter().zip(y.iter()))
        }
        (Value::Record(x), Value::Record(y)) => {
            if x.len() != y.len() || x.keys().ne(y.keys()) {
                return EqVerdict::Definite(false);
            }
            eq_fold(x.values().zip(y.values()))
        }
        // Numbers compare across the int/float split: the runtime widens
        // mixed operands everywhere else (`as_num_pair`, `cmp_values`), and
        // leaving equality out made the pushed-down and residual paths
        // disagree on `x.score = 1` for a float-valued `score`.
        (Value::Int(a), Value::Float(b)) => EqVerdict::Definite((*a as f64) == *b),
        (Value::Float(a), Value::Int(b)) => EqVerdict::Definite(*a == (*b as f64)),
        // ISO §4.4.4: reference values are equal iff they denote the same
        // referent. A node and an edge are different referents — definitely
        // unequal, not a type error.
        (Value::Node(_), Value::Edge(_)) | (Value::Edge(_), Value::Node(_)) => {
            EqVerdict::Definite(false)
        }
        _ if same_kind(lhs, rhs) => EqVerdict::Definite(lhs == rhs),
        _ => EqVerdict::Mismatch,
    }
}

/// Whether two values are of the same base type, i.e. whether equality has a
/// common domain to work on at all.
fn same_kind(a: &Value, b: &Value) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// The nested reading of `eq_verdict`: total, because inside a composite
/// there is no domain check to report a mismatch against. `None` is unknown.
pub fn eq_3vl(lhs: &Value, rhs: &Value) -> Option<bool> {
    match eq_verdict(lhs, rhs) {
        EqVerdict::Definite(b) => Some(b),
        EqVerdict::Unknown => None,
        EqVerdict::Mismatch => Some(false),
    }
}

/// Fold pairwise verdicts the way the Kleene `AND` does: a definite `false`
/// anywhere is decisive, an unknown otherwise makes the whole unknown. Uses
/// the nested reading per element, so a mismatched pair counts as `false`.
fn eq_fold<'a>(pairs: impl Iterator<Item = (&'a Value, &'a Value)>) -> EqVerdict {
    let mut unknown = false;
    for (a, b) in pairs {
        match eq_3vl(a, b) {
            Some(false) => return EqVerdict::Definite(false),
            Some(true) => {}
            None => unknown = true,
        }
    }
    if unknown {
        EqVerdict::Unknown
    } else {
        EqVerdict::Definite(true)
    }
}

/// Evaluate `lhs <op> rhs` with GQL-leaning semantics for pushed value
/// predicates. Null on either side yields false (3VL: predicate is null →
/// row drops). Numeric comparison promotes Int→Float; comparison across
/// incompatible kinds yields false. Records and lists support structural
/// `Eq`/`Ne` only — ordering on composite values is not defined here and
/// returns false. Shared between the LTJ filter loop and the standard
/// node/edge scan.
///
/// This returns a bare keep/drop `bool`, NOT the 3VL value the interpreter's
/// `eval_binop` returns (`Null` for a null operand). That is sound because the
/// value-predicate pushdown only lifts top-level positive `attr op literal`
/// AND-conjuncts, where a null operand means "drop" under both (`false` here;
/// `Null` → `get_bool` false there) — so they never disagree on which rows
/// survive. Pushing a conjunct out of an `OR` / under a `NOT` is already
/// forbidden (wrong for non-null rows too), and those keep-despite-null shapes
/// stay on the residual 3VL path. Pinned by `tests/pushdown_null_test.rs`.
///
/// UNIFY TRIGGER: if a pushed comparison's *value* is ever consumed as
/// something other than keep/drop (e.g. a future computed-column pushdown that
/// feeds a projection / `CASE`), replace this with a shared 3VL core —
/// `cmp_3vl(..) -> Value` — and make this a `matches!(.., Bool(true))` wrapper.
pub fn cmp_values(lhs: &Value, op: BinOp, rhs: &Value) -> bool {
    use std::cmp::Ordering;
    if lhs.is_null() || rhs.is_null() {
        return false;
    }
    // Temporal values (§4.16.6) are totally ordered within their own
    // type; a DATE and a LOCAL DATETIME are distinct types and never
    // compare (false for every operator, like any cross-type pair).
    match (lhs, rhs) {
        (Value::Date(x), Value::Date(y)) => {
            return match op {
                BinOp::Eq => x == y,
                BinOp::Ne => x != y,
                BinOp::Lt => x < y,
                BinOp::Le => x <= y,
                BinOp::Gt => x > y,
                BinOp::Ge => x >= y,
                _ => false,
            };
        }
        (Value::LocalDatetime(x), Value::LocalDatetime(y)) => {
            return match op {
                BinOp::Eq => x == y,
                BinOp::Ne => x != y,
                BinOp::Lt => x < y,
                BinOp::Le => x <= y,
                BinOp::Gt => x > y,
                BinOp::Ge => x >= y,
                _ => false,
            };
        }
        _ => {}
    }
    // Composite values: structural equality via the derived `PartialEq`.
    // Ordering is undefined and yields false for `<`, `<=`, `>`, `>=`.
    match (lhs, rhs) {
        (Value::Record(_), Value::Record(_)) | (Value::List(_), Value::List(_)) => {
            // `eq_3vl` so a null *inside* the composite makes the comparison
            // unknown, exactly as the residual path computes it; unknown maps
            // to `false` here because this function is a keep/drop verdict and
            // the residual path drops an unknown row too.
            return match (op, eq_3vl(lhs, rhs)) {
                (BinOp::Eq, Some(b)) => b,
                (BinOp::Ne, Some(b)) => !b,
                _ => false,
            };
        }
        // ISO §4.4.4: reference values are equal iff they refer to
        // the same referent (same id). They are not orderable
        // without Feature GA04. Cross-kind (node vs edge) is never
        // equal regardless of id. The PartialEq on `Value` already
        // implements the per-id identity, so `==` does the right
        // thing here.
        (Value::Node(_), Value::Node(_))
        | (Value::Edge(_), Value::Edge(_))
        | (Value::Node(_), Value::Edge(_))
        | (Value::Edge(_), Value::Node(_)) => {
            return match op {
                BinOp::Eq => lhs == rhs,
                BinOp::Ne => lhs != rhs,
                _ => false,
            };
        }
        _ => {}
    }
    let ord = match (lhs, rhs) {
        (Value::Int(a), Value::Int(b)) => a.cmp(b),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
        (Value::Int(a), Value::Float(b)) => (*a as f64).partial_cmp(b).unwrap_or(Ordering::Equal),
        (Value::Float(a), Value::Int(b)) => a.partial_cmp(&(*b as f64)).unwrap_or(Ordering::Equal),
        (Value::Str(a), Value::Str(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        _ => return false,
    };
    match op {
        BinOp::Eq => ord == Ordering::Equal,
        BinOp::Ne => ord != Ordering::Equal,
        BinOp::Lt => ord == Ordering::Less,
        BinOp::Le => ord != Ordering::Greater,
        BinOp::Gt => ord == Ordering::Greater,
        BinOp::Ge => ord != Ordering::Less,
        _ => false,
    }
}
