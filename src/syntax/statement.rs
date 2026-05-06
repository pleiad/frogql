//! Top-level statement AST. Distinguishes a query from the catalog DDL
//! (CREATE / USE / DROP GRAPH TYPE) so the REPL and Python bindings can
//! route to the right handler in a single call.

use crate::syntax::dm::DmStatement;
use crate::syntax::query::Query;
use crate::typing::variable_type::VariableType;

/// A parsed top-level statement.
#[derive(Debug, Clone)]
pub enum Statement {
    /// A regular GQL query (MATCH/WHERE/RETURN).
    Query(Query),
    /// ISO 39075 §13 `<linear data-modifying statement>` — INSERT,
    /// SET, REMOVE, DELETE, DETACH DELETE, optionally preceded by
    /// MATCH clauses and followed by RETURN.
    DataModification(DmStatement),
    /// `CREATE GRAPH TYPE <name> AS { <body> };`
    CreateGraphType {
        name: String,
        body: Vec<TypeElement>,
    },
    /// `USE GRAPH TYPE <name>;`
    ///
    /// `refresh_default` is true when `<name>` is the reserved `DEFAULT`
    /// (case-insensitive). The handler must re-run schema inference and
    /// overwrite the catalog entry before activating it.
    UseGraphType { name: String, refresh_default: bool },
    /// `DROP GRAPH TYPE <name>;`
    DropGraphType { name: String },
    /// `SHOW GRAPH TYPES;` — list every catalog entry with its active flag.
    ShowGraphTypes,
    /// `SHOW GRAPH TYPE <name>;` — describe one catalog entry.
    ShowGraphType { name: String },
    /// `SHOW CURRENT GRAPH TYPE;` — name + content of the active type
    /// (or "(none)" if nothing is active).
    ShowCurrentGraphType,
    /// `VALIDATE GRAPH TYPE <name>;` — walk the data and check that
    /// every node and edge satisfies a type in the named schema.
    /// Caches the verdict on the catalog entry.
    ValidateGraphType { name: String },
    /// `CREATE [HASH|BTREE] INDEX [<name>] ON :Label(prop) [USING HASH|BTREE];`
    /// Declares a secondary index on a node property. Auto-named
    /// `<label>_<prop>_<kind>` when `name` is omitted. `kind` defaults to
    /// `Hash` when neither `BTREE` nor `USING BTREE` is supplied.
    CreateIndex {
        name: Option<String>,
        label: String,
        prop: String,
        kind: IndexKindStmt,
    },
    /// `DROP INDEX <name>;` — remove a declared index. Auto-inferred
    /// indexes cannot be dropped (re-built every open).
    DropIndex { name: String },
    /// `SHOW INDEXES;` — list all indexes (auto + declared) with kind +
    /// entry counts.
    ShowIndexes,
}

/// Index flavour used in DDL. Stays separate from
/// `store::secondary_index::IndexKind` so the syntax layer doesn't depend
/// on the store internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexKindStmt {
    Hash,
    BTree,
}

/// A single element inside a `CREATE GRAPH TYPE` body.
#[derive(Debug, Clone)]
pub enum TypeElement {
    /// `(:Label {name STRING})` — produces a `VariableType::Node(...)`.
    Node(VariableType),
    /// `(:A)-[:E]->(:B)` or `(:A)~[:E]~(:B)` — produces a directional
    /// or non-directional edge `VariableType` carrying its endpoints.
    Edge(VariableType),
}
