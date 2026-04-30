use std::fmt;

use crate::model::value::Value;
use crate::typing::simple_type::SimpleType;

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    Is,
    As,
    In,
}

impl BinOp {
    /// Returns (expected_left_type, expected_right_type, result_type) for the operator.
    pub fn delta(
        &self,
        ty1: &SimpleType,
        ty2: &SimpleType,
    ) -> (SimpleType, SimpleType, SimpleType) {
        // Arithmetic and ordering accept either int or float; the runtime widens mixed
        // Int/Float operands to f64 in `eval_binop`.
        let num = SimpleType::Union(Box::new(SimpleType::Z), Box::new(SimpleType::F));
        match self {
            BinOp::Add | BinOp::Sub => (num.clone(), num.clone(), num),
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => (num.clone(), num, SimpleType::B),
            BinOp::Eq | BinOp::Ne => {
                let m = SimpleType::meet(ty1, ty2);
                (m.clone(), m, SimpleType::B)
            }
            BinOp::And | BinOp::Or => (SimpleType::B, SimpleType::B, SimpleType::B),
            BinOp::Is | BinOp::As => (SimpleType::Star, SimpleType::Star, SimpleType::B),
            BinOp::In => (
                SimpleType::Star,
                SimpleType::List(Box::new(SimpleType::Star)),
                SimpleType::B,
            ),
        }
    }
}

impl std::str::FromStr for BinOp {
    type Err = ();

    fn from_str(s: &str) -> Result<BinOp, ()> {
        match s {
            "+" => Ok(BinOp::Add),
            "-" => Ok(BinOp::Sub),
            "<" => Ok(BinOp::Lt),
            ">" => Ok(BinOp::Gt),
            "<=" => Ok(BinOp::Le),
            ">=" => Ok(BinOp::Ge),
            "=" => Ok(BinOp::Eq),
            "!=" => Ok(BinOp::Ne),
            "and" => Ok(BinOp::And),
            "or" => Ok(BinOp::Or),
            "TYPED" | "typed" => Ok(BinOp::Is),
            "as" => Ok(BinOp::As),
            "in" => Ok(BinOp::In),
            _ => Err(()),
        }
    }
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinOp::Add => write!(f, "+"),
            BinOp::Sub => write!(f, "-"),
            BinOp::Lt => write!(f, "<"),
            BinOp::Gt => write!(f, ">"),
            BinOp::Le => write!(f, "<="),
            BinOp::Ge => write!(f, ">="),
            BinOp::Eq => write!(f, "="),
            BinOp::Ne => write!(f, "!="),
            BinOp::And => write!(f, "and"),
            BinOp::Or => write!(f, "or"),
            BinOp::Is => write!(f, "TYPED"),
            BinOp::As => write!(f, "as"),
            BinOp::In => write!(f, "in"),
        }
    }
}

/// Unary operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

impl UnOp {
    /// Returns (expected_operand_type, result_type).
    pub fn delta(&self) -> (SimpleType, SimpleType) {
        match self {
            UnOp::Neg => {
                let num = SimpleType::Union(Box::new(SimpleType::Z), Box::new(SimpleType::F));
                (num.clone(), num)
            }
            UnOp::Not => (SimpleType::B, SimpleType::B),
        }
    }
}

impl fmt::Display for UnOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnOp::Neg => write!(f, "-"),
            UnOp::Not => write!(f, "not"),
        }
    }
}

/// Expressions used in WHERE clauses.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Const(Value),
    AttrLookup {
        var: String,
        attr: String,
    },
    /// Field access on a value (e.g. after an attribute lookup returns a Record):
    /// `x.addr.street` parses as `FieldAccess { base: AttrLookup { x, addr }, field: street }`.
    FieldAccess {
        base: Box<Expr>,
        field: String,
    },
    Binop {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Unop {
        op: UnOp,
        operand: Box<Expr>,
    },
    /// Right-hand side of `is`/`as` operators — a type, not a value.
    Type(SimpleType),
}

impl Expr {
    /// Check if a Value conforms to a SimpleType.
    pub fn value_is_type(val: &Value, ty: &SimpleType) -> bool {
        match (val, ty) {
            (Value::Str(_), SimpleType::S) => true,
            (Value::Int(_), SimpleType::Z) => true,
            (Value::Float(_), SimpleType::F) => true,
            (Value::Bool(_), SimpleType::B) => true,
            (Value::List(items), SimpleType::List(elem_ty)) => {
                items.iter().all(|v| Expr::value_is_type(v, elem_ty))
            }
            (Value::Record(v_fields), SimpleType::Record(t_fields)) => {
                v_fields.len() == t_fields.len()
                    && v_fields
                        .iter()
                        .all(|(k, v)| t_fields.get(k).is_some_and(|ty| Expr::value_is_type(v, ty)))
            }
            (_, SimpleType::Star) => true,
            (_, SimpleType::Union(a, b)) => {
                Expr::value_is_type(val, a) || Expr::value_is_type(val, b)
            }
            _ => false,
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Const(v) => write!(f, "{v}"),
            Expr::AttrLookup { var, attr } => write!(f, "{var}.{attr}"),
            Expr::FieldAccess { base, field } => write!(f, "{base}.{field}"),
            Expr::Binop { op, left, right } => write!(f, "({left} {op} {right})"),
            Expr::Unop { op, operand } => write!(f, "{op} {operand}"),
            Expr::Type(t) => write!(f, "{t}"),
        }
    }
}
