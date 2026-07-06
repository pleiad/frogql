pub mod assignment;
pub mod catalog;
pub mod dm;
pub mod engine;
pub mod ltj;
pub mod path_select;
pub mod result;

use crate::model::value::Value;
use crate::syntax::expr::BinOp;

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
            return match op {
                BinOp::Eq => lhs == rhs,
                BinOp::Ne => lhs != rhs,
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
