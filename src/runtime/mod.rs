pub mod assignment;
pub mod catalog;
pub mod engine;
pub mod ltj;
pub mod result;

use crate::model::value::Value;
use crate::syntax::expr::BinOp;

/// Compare two values under `op` for pushed predicates (scans / LTJ filters).
/// Null on either side yields false. Numeric operands widen mixed int/float.
/// Records and lists: `Eq`/`Ne` only; ordering ops yield false.
pub fn cmp_values(lhs: &Value, op: BinOp, rhs: &Value) -> bool {
    use std::cmp::Ordering;
    if lhs.is_null() || rhs.is_null() {
        return false;
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
