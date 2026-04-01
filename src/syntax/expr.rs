use std::fmt;

use crate::model::value::Value;
use crate::typing::simple_type::SimpleType;

/// Binary operators.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl BinOp {
    /// Returns (expected_left_type, expected_right_type, result_type) for the operator.
    pub fn delta(&self, ty1: &SimpleType, ty2: &SimpleType) -> (SimpleType, SimpleType, SimpleType) {
        match self {
            BinOp::Add | BinOp::Sub => (SimpleType::Z, SimpleType::Z, SimpleType::Z),
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                (SimpleType::Z, SimpleType::Z, SimpleType::B)
            }
            BinOp::Eq | BinOp::Ne => {
                let m = SimpleType::meet(ty1, ty2);
                (m.clone(), m, SimpleType::B)
            }
            BinOp::And | BinOp::Or => (SimpleType::B, SimpleType::B, SimpleType::B),
            BinOp::Is | BinOp::As => (SimpleType::Star, SimpleType::Star, SimpleType::B),
        }
    }

    pub fn from_str(s: &str) -> Option<BinOp> {
        match s {
            "+" => Some(BinOp::Add),
            "-" => Some(BinOp::Sub),
            "<" => Some(BinOp::Lt),
            ">" => Some(BinOp::Gt),
            "<=" => Some(BinOp::Le),
            ">=" => Some(BinOp::Ge),
            "=" => Some(BinOp::Eq),
            "!=" => Some(BinOp::Ne),
            "and" => Some(BinOp::And),
            "or" => Some(BinOp::Or),
            "is" => Some(BinOp::Is),
            "as" => Some(BinOp::As),
            _ => None,
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
            BinOp::Is => write!(f, "is"),
            BinOp::As => write!(f, "as"),
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
            UnOp::Neg => (SimpleType::Z, SimpleType::Z),
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
    AttrLookup { var: String, attr: String },
    Binop { op: BinOp, left: Box<Expr>, right: Box<Expr> },
    Unop { op: UnOp, operand: Box<Expr> },
    /// Right-hand side of `is`/`as` operators — a type, not a value.
    Type(SimpleType),
}

impl Expr {
    /// Check if a Value conforms to a SimpleType.
    pub fn value_is_type(val: &Value, ty: &SimpleType) -> bool {
        match (val, ty) {
            (Value::Str(_), SimpleType::S) => true,
            (Value::Int(_), SimpleType::Z) => true,
            (Value::Bool(_), SimpleType::B) => true,
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
            Expr::Binop { op, left, right } => write!(f, "({left} {op} {right})"),
            Expr::Unop { op, operand } => write!(f, "{op} {operand}"),
            Expr::Type(t) => write!(f, "{t}"),
        }
    }
}
