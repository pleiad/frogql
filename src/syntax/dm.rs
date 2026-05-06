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
    /// `NODETACH DELETE` (default per §13.5 SR6). Targets restricted to
    /// `<binding variable reference>` in MVP-0 (Feature GD04 not enabled).
    Delete { detach: bool, targets: Vec<String> },
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
    }
}
