use std::fmt;

use super::query::{Aggregator, Query};
use crate::model::value::Value;
use crate::typing::simple_type::SimpleType;

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
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
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => (num.clone(), num.clone(), num),
            BinOp::Mod => (SimpleType::Z, SimpleType::Z, SimpleType::Z),
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
            "*" => Ok(BinOp::Mul),
            "/" => Ok(BinOp::Div),
            "MOD" | "mod" => Ok(BinOp::Mod),
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
            BinOp::Mul => write!(f, "*"),
            BinOp::Div => write!(f, "/"),
            BinOp::Mod => write!(f, "MOD"),
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
    /// ISO §20.12 `<binding variable reference>` — a bare variable
    /// name in expression position. Materializes to the variable's
    /// value (a node / edge reference value, or `Null` if unbound at
    /// runtime via OPTIONAL). Distinct from `AttrLookup`, which
    /// requires a `.attr` projection.
    Var(String),
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
    /// SQL-style null test: `expr IS NULL` (negated=false) or
    /// `expr IS NOT NULL` (negated=true). Returns Bool. Distinct from
    /// `expr = null`, which would silently produce false under 3VL.
    IsNull {
        operand: Box<Expr>,
        negated: bool,
    },
    /// ISO §20.7 `<case abbreviation>` — `COALESCE(v1, ..., vN)`,
    /// N ≥ 2 (parser-enforced). First non-null wins.
    Coalesce(Vec<Expr>),
    /// A built-in scalar function call resolved by name. Today: `FLOOR`
    /// (`Call { name: "FLOOR", args: [x] }`) and `CAST`
    /// (`Call { name: "CAST", args: [x, Expr::Type(target)] }`, the
    /// target carried as a type argument). A single generic node so new
    /// ISO numeric/string builtins land without new AST variants.
    Call {
        name: String,
        args: Vec<Expr>,
    },
    /// ISO §17.x `<record constructor>` — `[RECORD] { k: <value expr>, ... }`
    /// where field values are arbitrary expressions. A fully-constant
    /// record is folded to `Expr::Const(Value::Record(..))` by the parser;
    /// this variant carries the dynamic case. Fields keep source order.
    Record {
        fields: Vec<(String, Expr)>,
    },
    /// ISO §16.x `<value query expression>` — `VALUE { <nested query> }`.
    /// Runs a correlated subquery whose body has exactly one RETURN item
    /// plus optional ORDER BY / LIMIT, and projects that item's value for
    /// the single selected row (or `Null` when the body is empty). Outer
    /// variables are visible via correlation; inner bindings do not escape.
    ValueSubquery {
        body: Box<Query>,
    },
    Case {
        branches: Vec<(Expr, Expr)>,
        else_expr: Option<Box<Expr>>,
    },
    /// ISO §16.x `<list value constructor by enumeration>` with a
    /// filter/map — `[<var> IN <source> [WHERE <filter>] | <body>]`. Binds
    /// `var` to each element of the `source` list, keeps elements for which
    /// `filter` holds (when present), and collects `body` evaluated per
    /// element into a new list. `var` is scoped to `filter`/`body` only.
    ListComprehension {
        var: String,
        source: Box<Expr>,
        filter: Option<Box<Expr>>,
        body: Box<Expr>,
    },
    /// Right-hand side of `is`/`as` operators — a type, not a value.
    Type(SimpleType),
    /// `EXISTS { <body> }` — Boolean predicate over a subquery body.
    /// The body is a `Query` without RETURN / GROUP BY / LIMIT / DISTINCT
    /// (the parser rejects those tokens inside the braces). Outer-bound
    /// variables are visible inside the body via correlation; bindings
    /// introduced by the body do not escape to the outer scope.
    Exists {
        body: Box<Query>,
    },
    /// `NOT EXISTS { <body> }` — separate variant from `Exists` because
    /// the optimiser folds it to `true` (vs `false` for `Exists`) when
    /// the body is statically empty.
    NotExists {
        body: Box<Query>,
    },
    /// ISO §20.9 `<aggregate function>` appearing inside a value
    /// expression — e.g. `COUNT(x) + COUNT(y)` in a RETURN item. A bare
    /// aggregate in RETURN is still carried as `ReturnItem::Aggregate`;
    /// this variant exists so an aggregate can be an *operand* of a
    /// `Binop`/`Unop`. The runtime reduces each `Agg` over its group
    /// before evaluating the surrounding arithmetic (see
    /// `Runtime::run_aggregated`).
    Agg(Box<Aggregator>),
}

impl Expr {
    /// True when the expression tree contains an aggregate function
    /// anywhere. Drives the grouping/aggregation path: a RETURN item
    /// whose expr contains an aggregate is projected per-group, not used
    /// as a grouping key.
    pub fn contains_agg(&self) -> bool {
        match self {
            Expr::Agg(_) => true,
            Expr::Binop { left, right, .. } => left.contains_agg() || right.contains_agg(),
            Expr::Unop { operand, .. } | Expr::IsNull { operand, .. } => operand.contains_agg(),
            Expr::FieldAccess { base, .. } => base.contains_agg(),
            Expr::Coalesce(args) | Expr::Call { args, .. } => args.iter().any(|a| a.contains_agg()),
            Expr::Case {
                branches,
                else_expr,
            } => {
                branches
                    .iter()
                    .any(|(cond, value)| cond.contains_agg() || value.contains_agg())
                    || else_expr.as_deref().is_some_and(Expr::contains_agg)
            }
            Expr::Record { fields } => fields.iter().any(|(_, e)| e.contains_agg()),
            Expr::ListComprehension {
                source,
                filter,
                body,
                ..
            } => {
                source.contains_agg()
                    || filter.as_deref().is_some_and(Expr::contains_agg)
                    || body.contains_agg()
            }
            Expr::Const(_)
            | Expr::Var(_)
            | Expr::AttrLookup { .. }
            | Expr::Type(_)
            // A subquery is self-contained: any aggregate inside it is
            // reduced within the subquery, not over the outer group.
            | Expr::ValueSubquery { .. }
            | Expr::Exists { .. }
            | Expr::NotExists { .. } => false,
        }
    }

    /// True when the expression contains a subquery (EXISTS / NOT EXISTS /
    /// VALUE). Such an item is evaluated on a group's representative row
    /// rather than acting as a grouping key, so the grouped-query check
    /// exempts it just as it exempts aggregates.
    pub fn contains_subquery(&self) -> bool {
        match self {
            Expr::Exists { .. } | Expr::NotExists { .. } | Expr::ValueSubquery { .. } => true,
            Expr::Binop { left, right, .. } => {
                left.contains_subquery() || right.contains_subquery()
            }
            Expr::Unop { operand, .. } | Expr::IsNull { operand, .. } => {
                operand.contains_subquery()
            }
            Expr::FieldAccess { base, .. } => base.contains_subquery(),
            Expr::Coalesce(args) | Expr::Call { args, .. } => {
                args.iter().any(|a| a.contains_subquery())
            }
            Expr::Case {
                branches,
                else_expr,
            } => {
                branches
                    .iter()
                    .any(|(cond, value)| cond.contains_subquery() || value.contains_subquery())
                    || else_expr.as_deref().is_some_and(Expr::contains_subquery)
            }
            Expr::Record { fields } => fields.iter().any(|(_, e)| e.contains_subquery()),
            Expr::ListComprehension {
                source,
                filter,
                body,
                ..
            } => {
                source.contains_subquery()
                    || filter.as_deref().is_some_and(Expr::contains_subquery)
                    || body.contains_subquery()
            }
            Expr::Const(_)
            | Expr::Var(_)
            | Expr::AttrLookup { .. }
            | Expr::Type(_)
            | Expr::Agg(_) => false,
        }
    }

    /// Binding variables this expression references in the *outer* scope.
    /// A subquery contributes only its correlation surface, which from the
    /// outer side equals the binding variables it mentions — but those are
    /// resolved inside the subquery, so for the grouped-query
    /// functional-dependency check we conservatively treat a subquery as
    /// referencing no outer grouping key (it is exempted via
    /// `contains_subquery`). Drives `GROUP BY <binding variable>` validation
    /// (ISO: a non-aggregate projection must depend only on grouping keys).
    pub fn referenced_vars(&self, acc: &mut std::collections::BTreeSet<String>) {
        match self {
            Expr::Var(name) => {
                acc.insert(name.clone());
            }
            Expr::AttrLookup { var, .. } => {
                acc.insert(var.clone());
            }
            Expr::Binop { left, right, .. } => {
                left.referenced_vars(acc);
                right.referenced_vars(acc);
            }
            Expr::Unop { operand, .. } | Expr::IsNull { operand, .. } => {
                operand.referenced_vars(acc)
            }
            Expr::FieldAccess { base, .. } => base.referenced_vars(acc),
            Expr::Coalesce(args) | Expr::Call { args, .. } => {
                for a in args {
                    a.referenced_vars(acc);
                }
            }
            Expr::Case {
                branches,
                else_expr,
            } => {
                for (cond, value) in branches {
                    cond.referenced_vars(acc);
                    value.referenced_vars(acc);
                }
                if let Some(value) = else_expr {
                    value.referenced_vars(acc);
                }
            }
            Expr::Record { fields } => {
                for (_, e) in fields {
                    e.referenced_vars(acc);
                }
            }
            Expr::ListComprehension {
                var,
                source,
                filter,
                body,
            } => {
                // `source` is evaluated in the outer scope; `filter`/`body`
                // see `var` locally, so its references don't escape.
                source.referenced_vars(acc);
                let mut inner = std::collections::BTreeSet::new();
                if let Some(f) = filter {
                    f.referenced_vars(&mut inner);
                }
                body.referenced_vars(&mut inner);
                inner.remove(var);
                acc.extend(inner);
            }
            // Subqueries are handled via `contains_subquery`; their
            // internal references are out of scope here.
            Expr::Const(_)
            | Expr::Type(_)
            | Expr::Agg(_)
            | Expr::Exists { .. }
            | Expr::NotExists { .. }
            | Expr::ValueSubquery { .. } => {}
        }
    }
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
            (Value::Node(_), SimpleType::Node) => true,
            (Value::Edge(_), SimpleType::Edge) => true,
            (Value::Path(_), SimpleType::Path) => true,
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
            Expr::Var(name) => write!(f, "{name}"),
            Expr::AttrLookup { var, attr } => write!(f, "{var}.{attr}"),
            Expr::FieldAccess { base, field } => write!(f, "{base}.{field}"),
            Expr::Binop { op, left, right } if *op == BinOp::Mod => {
                write!(f, "MOD({left}, {right})")
            }
            Expr::Binop { op, left, right } => write!(f, "({left} {op} {right})"),
            Expr::Unop { op, operand } => write!(f, "{op} {operand}"),
            Expr::IsNull { operand, negated } => {
                if *negated {
                    write!(f, "({operand} IS NOT NULL)")
                } else {
                    write!(f, "({operand} IS NULL)")
                }
            }
            Expr::Coalesce(args) => {
                let parts: Vec<String> = args.iter().map(|e| e.to_string()).collect();
                write!(f, "COALESCE({})", parts.join(", "))
            }
            Expr::Call { name, args } => {
                // CAST renders with its `AS <type>` surface form.
                if name == "CAST" && args.len() == 2 {
                    write!(f, "CAST({} AS {})", args[0], args[1])
                } else {
                    let parts: Vec<String> = args.iter().map(|e| e.to_string()).collect();
                    write!(f, "{name}({})", parts.join(", "))
                }
            }
            Expr::Record { fields } => {
                let parts: Vec<String> = fields.iter().map(|(k, e)| format!("{k}: {e}")).collect();
                write!(f, "RECORD {{ {} }}", parts.join(", "))
            }
            Expr::ValueSubquery { body } => {
                write!(f, "VALUE {{ {} }}", display_subquery(body))
            }
            Expr::Case {
                branches,
                else_expr,
            } => {
                write!(f, "CASE")?;
                for (cond, value) in branches {
                    write!(f, " WHEN {cond} THEN {value}")?;
                }
                if let Some(value) = else_expr {
                    write!(f, " ELSE {value}")?;
                }
                write!(f, " END")
            }
            Expr::ListComprehension {
                var,
                source,
                filter,
                body,
            } => match filter {
                Some(cond) => write!(f, "[{var} IN {source} WHERE {cond} | {body}]"),
                None => write!(f, "[{var} IN {source} | {body}]"),
            },
            Expr::Type(t) => write!(f, "{t}"),
            Expr::Exists { body } => write!(f, "EXISTS {{ {} }}", display_subquery(body)),
            Expr::NotExists { body } => write!(f, "NOT EXISTS {{ {} }}", display_subquery(body)),
            Expr::Agg(agg) => write!(f, "{agg}"),
        }
    }
}

/// Compact textual rendering of a subquery body for the Display
/// instance. Mirrors the parser's surface form: optional MATCH on the
/// first clause, explicit MATCH/OPTIONAL MATCH on subsequent ones, no
/// RETURN / GROUP BY / LIMIT (the parser rejects those tokens inside
/// EXISTS, so the body never carries them).
fn display_subquery(q: &Query) -> String {
    use super::query::MatchStatement;
    let mut out = String::new();
    for (i, m) in q.matches.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match m {
            MatchStatement::Simple { pattern } => {
                if i > 0 {
                    out.push_str("MATCH ");
                }
                out.push_str(&format!("{pattern}"));
            }
            MatchStatement::Optional { pattern } => {
                out.push_str("OPTIONAL MATCH ");
                out.push_str(&format!("{pattern}"));
            }
        }
    }
    out
}
