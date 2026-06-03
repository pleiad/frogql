//! ISO/IEC 39075:2024 §13 data-modification AST.
//!
//! A `<linear data-modifying statement>` is a chain of zero or more
//! `<simple match statement>`s followed by a single primitive DML
//! (INSERT / SET / REMOVE / DELETE), optionally followed by a RETURN.
//!
//! MVP-0 surface (see plan in `~/.claude/plans/que-dice-el-gql-encapsulated-pumpkin.md`):
//!   - `INSERT <pattern>` standalone, or
//!   - `MATCH ... [DETACH | NODETACH] DELETE x [, y, ...]` with var refs only
//!   - optional trailing `RETURN ...`
//!
//! `SET` and `REMOVE` are reserved as tokens but the grammar rejects them
//! with "not implemented in this version" so users get a compile error
//! instead of a confusing parse error against a property name.

use crate::syntax::expr::Expr;
use crate::syntax::path_pattern::PathPattern;
use crate::syntax::query::{MatchStatement, ReturnItem};

/// Top-level data-modifying statement: zero or more MATCH clauses, one
/// DML op, optional RETURN. ISO §13.1 `<linear data-modifying statement>`.
#[derive(Debug, Clone, PartialEq)]
pub struct DmStatement {
    /// MATCH chain that produces the incoming working table for the op.
    /// Empty for standalone INSERT (no bindings to iterate over — the op
    /// runs once with an empty assignment).
    pub matches: Vec<MatchStatement>,
    pub op: DmOp,
    /// ISO §14.10 `<primitive result statement>`. None means "no RETURN
    /// clause" — the statement executes for side effects only and produces
    /// an empty working table.
    pub returns: Option<Vec<ReturnItem>>,
    pub limit: Option<u32>,
}

/// The DML op itself.
#[derive(Debug, Clone, PartialEq)]
pub enum DmOp {
    /// ISO §13.2. The `Vec<PathPattern>` is the comma-separated
    /// `<insert path pattern list>`. Each path pattern must satisfy
    /// `is_valid_insert_pattern` (no Filter, Repeat, Union, Questioned,
    /// EdgeAnyDirection, Join). Validated at parse time.
    Insert(Vec<PathPattern>),
    /// ISO §13.5. `detach=true` → `DETACH DELETE`; `detach=false` →
    /// `NODETACH DELETE` (default per §13.5 SR6). Each target is an
    /// arbitrary `<value expression>` evaluated against the binding row
    /// at execution time (MVP-1.E, ISO Feature GD04 enabled). The
    /// expression must yield a `Value::Node` or `Value::Edge`; `Null`
    /// is treated as a no-op (§13.5 GR4 a).
    Delete { detach: bool, targets: Vec<Expr> },
    /// ISO §13.3 SET. MVP-1.B handles property-value SET; label SET
    /// (`<set label item>`) lands in MVP-1.D.
    Set(Vec<SetItem>),
    /// ISO §13.4 REMOVE. MVP-1.C handles `REMOVE x.prop`; label REMOVE
    /// (`REMOVE x:Label`) lands in MVP-1.D.
    Remove(Vec<RemoveItem>),
}

/// One element of `<remove item list>` (ISO §13.4).
#[derive(Debug, Clone, PartialEq)]
pub enum RemoveItem {
    /// `<remove property item> ::= var <period> prop`.
    Property { var: String, prop: String },
    /// `<remove label item> ::= var <colon> label` (or `var IS label`).
    /// MVP-1.D, ISO Feature GD02.
    Label { var: String, label: String },
}

/// One element of `<set item list>` (ISO §13.3).
#[derive(Debug, Clone, PartialEq)]
pub enum SetItem {
    /// `<set property item> ::= var <period> prop <equals> <value expr>`.
    Property {
        var: String,
        prop: String,
        value: Expr,
    },
    /// `<set all properties item> ::= var <equals> <left brace> [pkv list] <right brace>`.
    /// ISO §13.3 GR8 b.i: every existing property of the target is
    /// removed first, then the new set is applied (clear+set, not merge).
    AllProperties {
        var: String,
        props: Vec<(String, Expr)>,
    },
    /// `<set label item> ::= var <colon> label` (or `var IS label`).
    /// MVP-1.D, ISO Feature GD02. Idempotent: setting a label the
    /// element already carries is a no-op (§13.3 GR8 c.iii).
    Label { var: String, label: String },
}

/// Validate that a `PathPattern` is legal as an `<insert path pattern>`
/// per ISO §13.2 + §16.5: only nodes, directed/undirected edges, and
/// concatenation. No filters, repetitions, unions, optional, any-direction
/// edges, or comma-joins.
pub fn validate_insert_pattern(p: &PathPattern) -> Result<(), String> {
    match p {
        PathPattern::Node(_) => Ok(()),
        PathPattern::EdgeRight(_) | PathPattern::EdgeLeft(_) | PathPattern::EdgeUndirected(_) => {
            Ok(())
        }
        PathPattern::Concat(a, b) => {
            validate_insert_pattern(a)?;
            validate_insert_pattern(b)
        }
        PathPattern::EdgeAnyDirection(_) => Err(
            "INSERT patterns cannot use any-direction edges (-[]-); pick a direction (§16.5)"
                .into(),
        ),
        PathPattern::Filter(_, _) => {
            Err("INSERT patterns cannot have WHERE filters (§16.5)".into())
        }
        PathPattern::Repeat { .. } => {
            Err("INSERT patterns cannot use {n,m} repetition (§16.5)".into())
        }
        PathPattern::Union(_, _) => Err("INSERT patterns cannot use | union (§16.5)".into()),
        PathPattern::Questioned(_) => Err("INSERT patterns cannot use ? optional (§16.5)".into()),
        PathPattern::Join(_, _) => Err(
            "INSERT path patterns inside one statement use comma at the top level only (§13.2)"
                .into(),
        ),
        PathPattern::Selected { .. } => Err(
            "INSERT patterns cannot carry a path pattern prefix (WALK/TRAIL/SIMPLE/ACYCLIC/\
             ALL/ANY/SHORTEST) (§16.5)"
                .into(),
        ),
        PathPattern::Named { .. } => {
            Err("INSERT patterns cannot bind a path variable (`p = ...`) (§16.5)".into())
        }
    }
}
