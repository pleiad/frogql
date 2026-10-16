//! Embedded API: open a `.gdb` file and run GQL statements against it.
//!
//! This is the Rust counterpart of the Python and Node `Connection`, and
//! the entry point to reach for when embedding froGQL in a Rust program.
//! It owns the store, keeps the LTJ index warm across calls, routes each
//! statement to the query engine, the data-modification engine, or the
//! catalog, and returns plain values.
//!
//! ```no_run
//! let conn = frogql::Connection::open("movies.gdb")?;
//! conn.execute("INSERT (:Person {name: 'Ada'})")?;
//! conn.save()?;
//!
//! let rows = conn.query("MATCH (p:Person) RETURN p.name AS name")?;
//! for row in rows.iter() {
//!     println!("{:?}", row.get("name"));
//! }
//! # Ok::<(), frogql::Error>(())
//! ```
//!
//! Everything below this module (`LazyGraphStore`, `Runtime`,
//! `compile_query_with`, `run_dm_with_index`) stays public for callers who
//! need finer control; [`Connection::store`] hands out the store.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use crate::model::csv_loader;
use crate::model::graph::MemoryGraphStore;
use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, Value};
use crate::parser::parse_statement;
use crate::runtime::catalog::ValidationStatus;
use crate::runtime::engine::{path_value_to_value, Runtime};
use crate::runtime::ltj::triple_index::TripleIndex;
use crate::runtime::result::QueryResult;
use crate::store::lazy::LazyGraphStore;
use crate::store::secondary_index::IndexKind;
use crate::syntax::dm::DmStatement;
use crate::syntax::expr::Expr;
use crate::syntax::query::{Query, ReturnItem};
use crate::syntax::statement::{IndexKindStmt, Statement, TypeElement};
use crate::typing::format::format_schema;
use crate::typing::inference::infer_simple_schema;
use crate::typing::validate::validate_against_data;
use crate::typing::variable_type::{Schema, VariableType};
use crate::CompileError;

/// Why a statement did not run.
#[derive(Debug)]
pub enum Error {
    /// The statement did not parse or did not typecheck.
    Compile(CompileError),
    /// The statement compiled and failed while running (a DML constraint,
    /// an unknown graph type, a missing index, ...).
    Execution(String),
    /// Reading or writing the database file failed.
    Io(std::io::Error),
    /// The timeout set with [`Connection::set_timeout`] expired, or the
    /// flag from [`Connection::set_cancel_flag`] was raised. No rows are
    /// returned: a stopped search holds a partial result, not a shorter one.
    Interrupted,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Compile(e) => write!(f, "{e}"),
            Error::Execution(s) => write!(f, "{s}"),
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Interrupted => f.write_str("query interrupted (timeout or cancel)"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<CompileError> for Error {
    fn from(e: CompileError) -> Self {
        Error::Compile(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What a statement produced.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// A query, or a `SHOW ...` / `VALIDATE ...` statement.
    Rows(Rows),
    /// `INSERT`, `SET`, `REMOVE`, `DELETE`.
    Modified(Modified),
    /// Catalog DDL: `CREATE / USE / DROP GRAPH TYPE`, `CREATE / DROP INDEX`.
    Message(String),
}

impl Outcome {
    /// The rows, if this outcome has any.
    pub fn into_rows(self) -> Option<Rows> {
        match self {
            Outcome::Rows(r) => Some(r),
            _ => None,
        }
    }
}

/// A table of results.
///
/// With `RETURN`, `columns` are the aliases (`RETURN p.name AS name`), the
/// variable name for a bare variable (`RETURN p`), and `col<i>` otherwise.
/// Without `RETURN`, there is one column per pattern variable, sorted by
/// name, holding [`Value::Node`] / [`Value::Edge`] references.
///
/// Nodes and edges come back as ids; [`Connection::node`] and
/// [`Connection::edge`] resolve them to labels and properties.
#[derive(Debug, Clone, Default)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// Typechecker warnings (the query still ran).
    pub warnings: Vec<String>,
}

impl Rows {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Position of a column by name.
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }

    /// Iterate rows with access by column name.
    pub fn iter(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows.iter().map(move |values| Row {
            columns: &self.columns,
            values,
        })
    }
}

/// One row of [`Rows`].
#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    columns: &'a [String],
    values: &'a [Value],
}

impl<'a> Row<'a> {
    /// The value in the named column, or `None` if no column has that name.
    pub fn get(&self, column: &str) -> Option<&'a Value> {
        let i = self.columns.iter().position(|c| c == column)?;
        self.values.get(i)
    }

    pub fn values(&self) -> &'a [Value] {
        self.values
    }
}

/// Counts reported by a data-modifying statement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modified {
    pub nodes_inserted: usize,
    pub edges_inserted: usize,
    pub nodes_deleted: usize,
    pub edges_deleted: usize,
    pub nodes_modified: usize,
    pub edges_modified: usize,
}

/// A node or edge resolved to its labels and properties.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub id: Id,
    pub labels: Vec<String>,
    pub props: BTreeMap<String, Value>,
}

/// An open database.
///
/// Neither `Send` nor `Sync`: the store keeps its caches and the mutation
/// overlay behind `RefCell`s. Open it on the thread that uses it.
///
/// Mutations stay in memory until [`Connection::save`], like an explicit
/// commit; dropping the connection without saving discards them.
pub struct Connection {
    store: LazyGraphStore,
    path: PathBuf,
    /// Built at open and handed to every `Runtime`; refreshed by a delta
    /// after each DML instead of being rebuilt.
    triple_index: RefCell<Option<Arc<TripleIndex>>>,
    timeout: Cell<Option<Duration>>,
    cancel: RefCell<Option<Arc<AtomicBool>>>,
}

impl Connection {
    /// Open a `.gdb` database, creating an empty one if `path` does not
    /// exist (like `sqlite3_open`). Builds the join index eagerly so the
    /// first query runs at warm speed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let store = LazyGraphStore::open_or_create(path)?;
        let index = Runtime::new(&store).warm_triple_index();
        Ok(Connection {
            store,
            path: path.to_path_buf(),
            triple_index: RefCell::new(Some(index)),
            timeout: Cell::new(None),
            cancel: RefCell::new(None),
        })
    }

    /// Run one GQL statement with no row cap.
    pub fn execute(&self, statement: &str) -> Result<Outcome> {
        self.execute_with_limit(statement, 0)
    }

    /// Run one GQL statement, producing at most `limit` rows (`0` = no
    /// cap). An explicit `LIMIT` in the query also applies; the smaller wins.
    pub fn execute_with_limit(&self, statement: &str, limit: usize) -> Result<Outcome> {
        let stmt =
            parse_statement(statement).map_err(|e| Error::Compile(CompileError::Parse(e)))?;
        match stmt {
            Statement::Query(_) => self.run_query(statement, limit).map(Outcome::Rows),
            Statement::DataModification(dm) => self.run_dm(&dm).map(Outcome::Modified),
            Statement::CreateGraphType { name, body } => self.create_graph_type(&name, &body),
            Statement::UseGraphType {
                name,
                refresh_default,
            } => self.use_graph_type(&name, refresh_default),
            Statement::DropGraphType { name } => {
                self.store
                    .catalog_mut()
                    .drop(&name)
                    .map_err(Error::Execution)?;
                self.store.save_catalog()?;
                Ok(Outcome::Message(format!("GRAPH TYPE '{name}' dropped.")))
            }
            Statement::ShowGraphTypes => Ok(Outcome::Rows(self.show_graph_types())),
            Statement::ShowGraphType { name } => self.show_graph_type(Some(&name)),
            Statement::ShowCurrentGraphType => self.show_graph_type(None),
            Statement::ValidateGraphType { name } => self.validate_graph_type(&name),
            Statement::CreateIndex {
                name,
                label,
                prop,
                kind,
            } => self.create_index(name, &label, &prop, kind),
            Statement::DropIndex { name } => {
                if self.store.secondary_indexes_mut().drop_named(&name) {
                    Ok(Outcome::Message(format!("INDEX '{name}' dropped.")))
                } else {
                    Err(Error::Execution(format!("index '{name}' not found")))
                }
            }
            Statement::ShowIndexes => Ok(Outcome::Rows(self.show_indexes())),
        }
    }

    /// Run a statement that must produce rows (a query or a `SHOW`).
    pub fn query(&self, statement: &str) -> Result<Rows> {
        match self.execute(statement)? {
            Outcome::Rows(r) => Ok(r),
            _ => Err(Error::Execution(
                "statement does not return rows; use execute()".to_string(),
            )),
        }
    }

    /// Write every pending mutation and index declaration to the file the
    /// connection was opened from (atomic: temp file + rename).
    pub fn save(&self) -> Result<()> {
        self.store.save(&self.path)?;
        Ok(())
    }

    /// Stop any query that runs longer than `timeout` (`None` = no limit).
    /// The check is cooperative and covers the main join paths; see
    /// `Runtime::set_query_budget`.
    pub fn set_timeout(&self, timeout: Option<Duration>) {
        self.timeout.set(timeout);
    }

    /// Register a flag another thread can raise to stop the running query.
    pub fn set_cancel_flag(&self, flag: Option<Arc<AtomicBool>>) {
        *self.cancel.borrow_mut() = flag;
    }

    pub fn node_count(&self) -> u32 {
        self.store.node_count()
    }

    pub fn edge_count(&self) -> u32 {
        self.store.edge_count()
    }

    /// Labels and properties of node `id` (from a [`Value::Node`]).
    pub fn node(&self, id: Id) -> Element {
        Element {
            id,
            labels: self
                .store
                .node_labels(id)
                .required_labels()
                .into_iter()
                .map(|s| s.to_string())
                .collect(),
            props: self.store.node_props(id).into_iter().collect(),
        }
    }

    /// Labels and properties of edge `id` (from a [`Value::Edge`]).
    pub fn edge(&self, id: Id) -> Element {
        Element {
            id,
            labels: self
                .store
                .edge_labels(id)
                .required_labels()
                .into_iter()
                .map(|s| s.to_string())
                .collect(),
            props: self.store.edge_props(id).into_iter().collect(),
        }
    }

    /// Source and target node ids of edge `id`.
    pub fn edge_endpoints(&self, id: Id) -> (Id, Id) {
        (self.store.src(id), self.store.tgt(id))
    }

    /// The active graph type, rendered as GQL.
    pub fn schema(&self) -> String {
        format_schema(&self.store.active_schema())
    }

    /// The underlying store, for direct use of the lower-level API.
    pub fn store(&self) -> &LazyGraphStore {
        &self.store
    }

    fn runtime(&self) -> Runtime<'_, LazyGraphStore> {
        if self.triple_index.borrow().is_none() {
            let built = Runtime::new(&self.store).warm_triple_index();
            *self.triple_index.borrow_mut() = Some(built);
        }
        let idx = self
            .triple_index
            .borrow()
            .clone()
            .expect("index just built");
        let rt = Runtime::with_triple_index(&self.store, idx);
        rt.set_query_budget(self.timeout.get());
        rt.set_cancel_flag(self.cancel.borrow().clone());
        rt
    }

    fn run_query(&self, text: &str, limit: usize) -> Result<Rows> {
        let compiled =
            crate::compile_query_with_diagnostics_with(&self.store.active_schema(), text)?;
        let q = compiled.query;
        let mut out = Rows {
            columns: projected_columns(&q),
            rows: Vec::new(),
            warnings: compiled.warnings,
        };
        if compiled.guaranteed_empty {
            return Ok(out);
        }
        let rt = self.runtime();
        let result = rt.run_query(&q, limit);
        if rt.query_timed_out() {
            return Err(Error::Interrupted);
        }
        match result {
            QueryResult::Projected(rows) => out.rows = rows,
            QueryResult::Raw(ir) => {
                let mut vars: Vec<String> = ir
                    .rows
                    .iter()
                    .flat_map(|r| r.assignment.m.keys().cloned())
                    .collect();
                vars.sort();
                vars.dedup();
                out.rows = ir
                    .rows
                    .iter()
                    .map(|r| {
                        vars.iter()
                            .map(|v| {
                                r.assignment
                                    .m
                                    .get(v)
                                    .map(path_value_to_value)
                                    .unwrap_or(Value::Null)
                            })
                            .collect()
                    })
                    .collect();
                out.columns = vars;
            }
        }
        Ok(out)
    }

    fn run_dm(&self, dm: &DmStatement) -> Result<Modified> {
        let validation = match self.store.catalog().active_name() {
            None | Some("DEFAULT") => None,
            _ => Some(self.store.active_schema()),
        };
        let exec = crate::runtime::dm::run_dm_with_index(
            &self.store,
            dm,
            validation.as_ref(),
            self.triple_index.borrow().clone(),
        )
        .map_err(Error::Execution)?;
        // Keep the index and add a delta for the change; `None` means no
        // delta can express it, and the next query rebuilds.
        let refreshed = self
            .triple_index
            .borrow()
            .as_deref()
            .and_then(|idx| crate::runtime::ltj::delta::refresh(idx, &self.store))
            .map(Arc::new);
        *self.triple_index.borrow_mut() = refreshed;
        self.store.catalog_mut().mark_default_dirty();
        Ok(Modified {
            nodes_inserted: exec.nodes_inserted,
            edges_inserted: exec.edges_inserted,
            nodes_deleted: exec.nodes_deleted,
            edges_deleted: exec.edges_deleted,
            nodes_modified: exec.nodes_modified,
            edges_modified: exec.edges_modified,
        })
    }

    fn create_graph_type(&self, name: &str, body: &[TypeElement]) -> Result<Outcome> {
        let schema = schema_from_body(body);
        let (n, e) = (schema.nodes.len(), schema.edges.len());
        self.store
            .catalog_mut()
            .register(name.to_string(), schema)
            .map_err(Error::Execution)?;
        self.store.save_catalog()?;
        Ok(Outcome::Message(format!(
            "GRAPH TYPE '{name}' created ({n} node types, {e} edge types)."
        )))
    }

    fn use_graph_type(&self, name: &str, refresh_default: bool) -> Result<Outcome> {
        let msg = if refresh_default {
            let schema = infer_simple_schema(&self.store);
            let (n, e) = (schema.nodes.len(), schema.edges.len());
            self.store.catalog_mut().install_default(schema);
            format!(
                "GRAPH TYPE 'DEFAULT' refreshed ({n} node types, {e} edge types) and activated."
            )
        } else {
            self.store
                .catalog_mut()
                .set_active(name)
                .map_err(Error::Execution)?;
            format!("Active GRAPH TYPE: {name}.")
        };
        self.store.save_catalog()?;
        Ok(Outcome::Message(msg))
    }

    fn show_graph_types(&self) -> Rows {
        let cat = self.store.catalog();
        let rows = cat
            .list()
            .into_iter()
            .map(|(name, active)| {
                let (n, e) = cat
                    .types
                    .get(name)
                    .map(|s| (s.nodes.len(), s.edges.len()))
                    .unwrap_or((0, 0));
                vec![
                    Value::Str(name.clone()),
                    Value::Bool(active),
                    Value::Int(n as i64),
                    Value::Int(e as i64),
                ]
            })
            .collect();
        Rows {
            columns: cols(&["name", "active", "nodes", "edges"]),
            rows,
            warnings: Vec::new(),
        }
    }

    /// `SHOW GRAPH TYPE <name>`, or the active one when `name` is `None`.
    fn show_graph_type(&self, name: Option<&str>) -> Result<Outcome> {
        let cat = self.store.catalog();
        let key = match name {
            Some(n) => canonical_type_name(n).to_string(),
            None => match cat.active_name() {
                Some(n) => n.to_string(),
                None => return Err(Error::Execution("no active graph type".to_string())),
            },
        };
        let schema = cat
            .types
            .get(&key)
            .ok_or_else(|| Error::Execution(format!("graph type '{key}' not found")))?;
        Ok(Outcome::Rows(Rows {
            columns: cols(&["name", "active", "nodes", "edges", "formatted"]),
            rows: vec![vec![
                Value::Str(key.clone()),
                Value::Bool(cat.active_name() == Some(key.as_str())),
                Value::Int(schema.nodes.len() as i64),
                Value::Int(schema.edges.len() as i64),
                Value::Str(format_schema(schema)),
            ]],
            warnings: Vec::new(),
        }))
    }

    fn validate_graph_type(&self, name: &str) -> Result<Outcome> {
        let key = canonical_type_name(name);
        let schema = self
            .store
            .catalog()
            .types
            .get(key)
            .cloned()
            .ok_or_else(|| Error::Execution(format!("graph type '{name}' not found")))?;
        let report = validate_against_data(&self.store, &schema);
        let status = ValidationStatus {
            against_node_count: report.nodes_checked,
            against_edge_count: report.edges_checked,
            violations: report.total_violations(),
            validated_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        self.store.catalog_mut().record_validation(key, status);
        self.store.save_catalog()?;
        Ok(Outcome::Rows(Rows {
            columns: cols(&[
                "name",
                "ok",
                "nodes_checked",
                "edges_checked",
                "node_violations",
                "edge_violations",
            ]),
            rows: vec![vec![
                Value::Str(key.to_string()),
                Value::Bool(report.ok()),
                Value::Int(report.nodes_checked as i64),
                Value::Int(report.edges_checked as i64),
                Value::Int(report.node_violations as i64),
                Value::Int(report.edge_violations as i64),
            ]],
            warnings: Vec::new(),
        }))
    }

    fn create_index(
        &self,
        name: Option<String>,
        label: &str,
        prop: &str,
        kind: IndexKindStmt,
    ) -> Result<Outcome> {
        let kind = match kind {
            IndexKindStmt::Hash => IndexKind::Hash,
            IndexKindStmt::BTree => IndexKind::BTree,
        };
        let name = name.unwrap_or_else(|| format!("{label}_{prop}_{}", kind_name(kind)));
        let spec = self
            .store
            .secondary_indexes_mut()
            .build_declared(&self.store, name, label, prop, kind)
            .map_err(Error::Execution)?;
        Ok(Outcome::Message(format!(
            "INDEX '{}' created on :{}({}) using {} ({} entries).",
            spec.name,
            spec.label,
            spec.prop,
            kind_name(spec.kind).to_uppercase(),
            spec.entries
        )))
    }

    fn show_indexes(&self) -> Rows {
        let rows = self
            .store
            .secondary_indexes()
            .list()
            .iter()
            .map(|s| {
                vec![
                    Value::Str(s.name.clone()),
                    Value::Str(s.label.clone()),
                    Value::Str(s.prop.clone()),
                    Value::Str(kind_name(s.kind).to_uppercase()),
                    Value::Bool(s.auto),
                    Value::Int(s.entries as i64),
                ]
            })
            .collect();
        Rows {
            columns: cols(&["name", "label", "prop", "kind", "auto", "entries"]),
            rows,
            warnings: Vec::new(),
        }
    }
}

/// Create (or overwrite) `db_path` from a JSON graph file.
pub fn import_json(db_path: impl AsRef<Path>, json_path: impl AsRef<Path>) -> Result<()> {
    let g = MemoryGraphStore::from_file(json_path.as_ref())
        .map_err(|e| Error::Execution(e.to_string()))?;
    save_with_default(&g, db_path.as_ref())
}

/// Create (or overwrite) `db_path` from a JSON graph held in memory.
pub fn import_json_str(db_path: impl AsRef<Path>, json: &str) -> Result<()> {
    let g = MemoryGraphStore::from_json_str(json).map_err(|e| Error::Execution(e.to_string()))?;
    save_with_default(&g, db_path.as_ref())
}

/// Create (or overwrite) `db_path` from a directory of CSV files.
pub fn import_csv(db_path: impl AsRef<Path>, csv_dir: impl AsRef<Path>) -> Result<()> {
    let g = csv_loader::load_from_csv_dir(csv_dir.as_ref())
        .map_err(|e| Error::Execution(e.to_string()))?;
    save_with_default(&g, db_path.as_ref())
}

/// Save `g` with DEFAULT inferred and active, as the REPL import does;
/// without a catalog the database would open with a permissive checker.
fn save_with_default(g: &MemoryGraphStore, db_path: &Path) -> Result<()> {
    g.save(db_path)
        .map_err(|e| Error::Execution(e.to_string()))?;
    let store = LazyGraphStore::open(db_path)?;
    let schema = infer_simple_schema(&store);
    store.catalog_mut().install_default(schema);
    store.save_catalog()?;
    Ok(())
}

/// Column names for a projected query; the same rule as the bindings.
fn projected_columns(q: &Query) -> Vec<String> {
    let Some(items) = q.returns.as_ref() else {
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            if let Some(a) = it.alias() {
                return a.to_string();
            }
            // ISO §14.11 SR 8a: a bare variable reference is `n AS n`.
            if let ReturnItem::Expr {
                expr: Expr::Var(name),
                ..
            } = it
            {
                return name.clone();
            }
            format!("col{i}")
        })
        .collect()
}

fn schema_from_body(body: &[TypeElement]) -> Schema {
    let mut nodes: Vec<VariableType> = Vec::new();
    let mut edges: Vec<VariableType> = Vec::new();
    for el in body {
        match el {
            TypeElement::Node(vt) => nodes.push(vt.clone()),
            TypeElement::Edge(vt) => edges.push(vt.clone()),
        }
    }
    Schema::from_parts(nodes, edges)
}

fn canonical_type_name(name: &str) -> &str {
    if name.eq_ignore_ascii_case("DEFAULT") {
        "DEFAULT"
    } else {
        name
    }
}

fn kind_name(kind: IndexKind) -> &'static str {
    match kind {
        IndexKind::Hash => "hash",
        IndexKind::BTree => "btree",
    }
}

fn cols(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}
