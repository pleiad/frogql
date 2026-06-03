// PyO3's #[pymethods] / #[pyfunction] macros expand to code that does
// PyErr -> PyErr conversions clippy flags as useless. Allow at crate
// level rather than tagging every function.
#![allow(clippy::useless_conversion)]

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use gqlrust::model::csv_loader;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::model::value::{PathValue, Value};
use gqlrust::parser::parse_statement;
use gqlrust::runtime::catalog::ValidationStatus;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::ltj::triple_index::TripleIndex;
use gqlrust::runtime::result::{IntermediateResult, QueryResult};
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::expr::Expr;
use gqlrust::syntax::query::ReturnItem;
use gqlrust::syntax::statement::{Statement, TypeElement};
use gqlrust::typing::format::format_schema;
use gqlrust::typing::inference::infer_simple_schema;
use gqlrust::typing::validate::{validate_against_data, ElementKind};
use gqlrust::typing::variable_type::{Schema, VariableType};

#[pyclass(unsendable)]
struct Connection {
    store: LazyGraphStore,
    /// Path the connection was opened from. Used by `save()` to write
    /// the merged base+overlay view back to the same file atomically.
    db_path: std::path::PathBuf,
    /// Shared LTJ TripleIndex. Built lazily on the first `execute()` that
    /// needs it; reused across every subsequent call. Without this every
    /// `execute()` would rebuild the six-ordering edge index from scratch
    /// (~700ms on SF0.1, multi-second on SF1) and cap warm-query latency.
    triple_index: RefCell<Option<Arc<TripleIndex>>>,
}

#[pymethods]
impl Connection {
    #[getter]
    fn node_count(&self) -> u32 {
        self.store.node_count()
    }

    #[getter]
    fn edge_count(&self) -> u32 {
        self.store.edge_count()
    }

    #[pyo3(signature = (query, limit = 100))]
    fn execute<'py>(&self, py: Python<'py>, query: &str, limit: usize) -> PyResult<PyObject> {
        // Top-level dispatch: DDL goes through the catalog, queries
        // through the runtime.
        let stmt = parse_statement(query).map_err(PyValueError::new_err)?;
        match stmt {
            Statement::CreateGraphType { name, body } => self.exec_create(py, &name, &body),
            Statement::UseGraphType {
                name,
                refresh_default,
            } => self.exec_use(py, &name, refresh_default),
            Statement::DropGraphType { name } => self.exec_drop(py, &name),
            Statement::ShowGraphTypes => self.exec_show_graph_types(py),
            Statement::ShowGraphType { name } => self.exec_show_graph_type(py, &name),
            Statement::ShowCurrentGraphType => self.exec_show_current(py),
            Statement::ValidateGraphType { name } => self.exec_validate(py, &name),
            Statement::CreateIndex {
                name,
                label,
                prop,
                kind,
            } => self.exec_create_index(py, name, &label, &prop, kind),
            Statement::DropIndex { name } => self.exec_drop_index(py, &name),
            Statement::ShowIndexes => self.exec_show_indexes(py),
            Statement::DataModification(dm) => self.exec_dm(py, dm),
            Statement::Query(_) => self.exec_query(py, query, limit),
        }
    }

    /// Persist the merged base+overlay state back to the file the
    /// connection was opened from. Mirrors the `.save` REPL command and
    /// SQLite's explicit-save model: until you call this, mutations live
    /// only in the in-RAM overlay.
    fn save(&self) -> PyResult<()> {
        self.store
            .save(&self.db_path)
            .map_err(|e| PyRuntimeError::new_err(format!("save: {e}")))
    }

    /// List graph types currently in the catalog with active markers.
    fn graph_types<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let cat = self.store.catalog();
        let out = PyList::empty_bound(py);
        for (name, is_active) in cat.list() {
            let d = PyDict::new_bound(py);
            d.set_item("name", name.as_str())?;
            d.set_item("active", is_active)?;
            if let Some(sch) = cat.types.get(name) {
                d.set_item("nodes", sch.nodes.len())?;
                d.set_item("edges", sch.edges.len())?;
            }
            out.append(d)?;
        }
        Ok(out)
    }

    fn schema<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        use std::collections::BTreeSet;
        let mut node_labels: BTreeSet<String> = BTreeSet::new();
        for nid in 0..self.store.node_count() {
            for l in self.store.node_labels(nid).required_labels() {
                node_labels.insert(l.to_string());
            }
        }
        let mut edge_labels: BTreeSet<String> = BTreeSet::new();
        for eid in 0..self.store.edge_count() {
            for l in self.store.edge_labels(eid).required_labels() {
                edge_labels.insert(l.to_string());
            }
        }
        let d = PyDict::new_bound(py);
        d.set_item("node_labels", node_labels.into_iter().collect::<Vec<_>>())?;
        d.set_item("edge_labels", edge_labels.into_iter().collect::<Vec<_>>())?;
        d.set_item("node_count", self.store.node_count())?;
        d.set_item("edge_count", self.store.edge_count())?;
        Ok(d)
    }

    fn __repr__(&self) -> String {
        format!(
            "<frogql.Connection nodes={} edges={}>",
            self.store.node_count(),
            self.store.edge_count()
        )
    }
}

// Connection helpers (no #[pymethods]; called from execute()).
impl Connection {
    /// Build a Runtime that already knows about the cached LTJ TripleIndex.
    /// First call after `open()` builds the index (~700ms on SF0.1) and
    /// stores it on the Connection; subsequent calls hand the same Arc to
    /// every Runtime cheaply (one atomic increment).
    fn runtime(&self) -> Runtime<'_, LazyGraphStore> {
        if self.triple_index.borrow().is_none() {
            let scratch = Runtime::new(&self.store);
            *self.triple_index.borrow_mut() = Some(scratch.warm_triple_index());
        }
        let idx = self
            .triple_index
            .borrow()
            .clone()
            .expect("triple index just built");
        Runtime::with_triple_index(&self.store, idx)
    }

    fn exec_query<'py>(&self, py: Python<'py>, query: &str, limit: usize) -> PyResult<PyObject> {
        let active = self.store.catalog().active_schema();
        let result = gqlrust::compile_query_with_diagnostics_with(&active, query)
            .map_err(|e| PyValueError::new_err(e.message()))?;
        let q = result.query;

        if !result.warnings.is_empty() {
            let warn = py.import_bound("warnings")?.getattr("warn")?;
            for w in &result.warnings {
                warn.call1((w.as_str(),))?;
            }
        }

        if result.guaranteed_empty {
            return Ok(PyList::empty_bound(py).into_py(py));
        }

        let rt = self.runtime();
        let result = rt.run_query(&q, limit);

        match result {
            QueryResult::Projected(rows) => {
                let headers: Vec<String> = q
                    .returns
                    .as_ref()
                    .map(|items| {
                        items
                            .iter()
                            .enumerate()
                            .map(|(i, it)| {
                                if let Some(a) = it.alias() {
                                    return a.to_string();
                                }
                                // ISO §14.11 SR 8a: a bare `<binding
                                // variable reference>` is shorthand for
                                // `n AS n` — the implicit alias is the
                                // variable name. Other expression
                                // shapes (`x.attr`, COALESCE(...), etc.)
                                // keep the positional `colN` default.
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
                    })
                    .unwrap_or_default();

                let out = PyList::empty_bound(py);
                for row in rows {
                    let d = PyDict::new_bound(py);
                    for (i, v) in row.into_iter().enumerate() {
                        let key = headers.get(i).cloned().unwrap_or_else(|| format!("col{i}"));
                        d.set_item(key, value_to_py(py, &self.store, &v)?)?;
                    }
                    out.append(d)?;
                }
                Ok(out.into_py(py))
            }
            QueryResult::Raw(ir) => Ok(raw_to_pylist(py, &self.store, &ir)?.into_py(py)),
        }
    }

    /// ISO §13 data-modification statement: INSERT / DELETE / DETACH
    /// DELETE. Returns a dict with insertion/deletion counts plus the
    /// post-mutation row count so callers can confirm the effect without
    /// having to round-trip through a follow-up MATCH.
    fn exec_dm<'py>(
        &self,
        py: Python<'py>,
        dm: gqlrust::syntax::dm::DmStatement,
    ) -> PyResult<PyObject> {
        let active_name = self.store.catalog().active_name().map(str::to_string);
        let schema_for_validation = match active_name.as_deref() {
            None | Some("DEFAULT") => None,
            _ => Some(self.store.catalog().active_schema()),
        };
        let exec = gqlrust::runtime::dm::run_dm(&self.store, &dm, schema_for_validation.as_ref())
            .map_err(PyValueError::new_err)?;
        // Any successful mutation invalidates the cached LTJ TripleIndex
        // — the next query on this connection rebuilds it from the
        // post-mutation graph.
        *self.triple_index.borrow_mut() = None;
        // Mark DEFAULT dirty so the next read of DEFAULT re-infers it
        // from the live store (Fase 7).
        self.store.catalog_mut().mark_default_dirty();
        let d = PyDict::new_bound(py);
        d.set_item("nodes_inserted", exec.nodes_inserted)?;
        d.set_item("edges_inserted", exec.edges_inserted)?;
        d.set_item("nodes_deleted", exec.nodes_deleted)?;
        d.set_item("edges_deleted", exec.edges_deleted)?;
        d.set_item("nodes_modified", exec.nodes_modified)?;
        d.set_item("edges_modified", exec.edges_modified)?;
        d.set_item("rows", exec.rows.len())?;
        Ok(d.into_py(py))
    }

    fn exec_create<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        body: &[TypeElement],
    ) -> PyResult<PyObject> {
        let schema = build_schema_from_body(body);
        let n_nodes = schema.nodes.len();
        let n_edges = schema.edges.len();
        {
            let mut cat = self.store.catalog_mut();
            cat.register(name.to_string(), schema)
                .map_err(PyValueError::new_err)?;
        }
        self.store
            .save_catalog()
            .map_err(|e| PyRuntimeError::new_err(format!("save catalog: {e}")))?;
        ddl_response(
            py,
            &format!("GRAPH TYPE '{name}' created ({n_nodes} node types, {n_edges} edge types)."),
        )
    }

    fn exec_use<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        refresh_default: bool,
    ) -> PyResult<PyObject> {
        if refresh_default {
            let schema = infer_simple_schema(&self.store);
            let n_nodes = schema.nodes.len();
            let n_edges = schema.edges.len();
            self.store.catalog_mut().install_default(schema);
            self.store
                .save_catalog()
                .map_err(|e| PyRuntimeError::new_err(format!("save catalog: {e}")))?;
            ddl_response(
                py,
                &format!(
                    "GRAPH TYPE 'DEFAULT' refreshed ({n_nodes} node types, {n_edges} edge types) and activated."
                ),
            )
        } else {
            self.store
                .catalog_mut()
                .set_active(name)
                .map_err(PyValueError::new_err)?;
            self.store
                .save_catalog()
                .map_err(|e| PyRuntimeError::new_err(format!("save catalog: {e}")))?;
            ddl_response(py, &format!("Active GRAPH TYPE: {name}."))
        }
    }

    fn exec_drop<'py>(&self, py: Python<'py>, name: &str) -> PyResult<PyObject> {
        {
            let mut cat = self.store.catalog_mut();
            cat.drop(name).map_err(PyValueError::new_err)?;
        }
        self.store
            .save_catalog()
            .map_err(|e| PyRuntimeError::new_err(format!("save catalog: {e}")))?;
        ddl_response(py, &format!("GRAPH TYPE '{name}' dropped."))
    }

    fn exec_show_graph_types<'py>(&self, py: Python<'py>) -> PyResult<PyObject> {
        Ok(self.graph_types(py)?.into_py(py))
    }

    fn exec_show_graph_type<'py>(&self, py: Python<'py>, name: &str) -> PyResult<PyObject> {
        let cat = self.store.catalog();
        let key = if name.eq_ignore_ascii_case("DEFAULT") {
            "DEFAULT"
        } else {
            name
        };
        let schema = cat
            .types
            .get(key)
            .ok_or_else(|| PyValueError::new_err(format!("graph type '{name}' not found")))?;
        let d = PyDict::new_bound(py);
        d.set_item("name", key)?;
        d.set_item("active", cat.active_name() == Some(key))?;
        d.set_item("nodes", schema.nodes.len())?;
        d.set_item("edges", schema.edges.len())?;
        d.set_item("formatted", format_schema(schema))?;
        if let Some(v) = cat.validation_for(key) {
            d.set_item("validation", validation_to_py(py, v)?)?;
        }
        Ok(d.into_py(py))
    }

    fn exec_show_current<'py>(&self, py: Python<'py>) -> PyResult<PyObject> {
        let cat = self.store.catalog();
        let d = PyDict::new_bound(py);
        match cat.active_name() {
            None => {
                d.set_item("active", py.None())?;
                d.set_item("formatted", "(none)")?;
            }
            Some(name) => {
                d.set_item("active", name)?;
                if let Some(schema) = cat.types.get(name) {
                    d.set_item("nodes", schema.nodes.len())?;
                    d.set_item("edges", schema.edges.len())?;
                    d.set_item("formatted", format_schema(schema))?;
                    if let Some(v) = cat.validation_for(name) {
                        d.set_item("validation", validation_to_py(py, v)?)?;
                    }
                }
            }
        }
        Ok(d.into_py(py))
    }

    fn exec_validate<'py>(&self, py: Python<'py>, name: &str) -> PyResult<PyObject> {
        let key = if name.eq_ignore_ascii_case("DEFAULT") {
            "DEFAULT"
        } else {
            name
        };
        let schema = self
            .store
            .catalog()
            .types
            .get(key)
            .cloned()
            .ok_or_else(|| PyValueError::new_err(format!("graph type '{name}' not found")))?;
        let report = validate_against_data(&self.store, &schema);

        let status = ValidationStatus {
            against_node_count: report.nodes_checked,
            against_edge_count: report.edges_checked,
            violations: report.total_violations(),
            validated_at_unix: now_unix(),
        };
        self.store.catalog_mut().record_validation(key, status);
        self.store
            .save_catalog()
            .map_err(|e| PyRuntimeError::new_err(format!("save catalog: {e}")))?;

        let d = PyDict::new_bound(py);
        d.set_item("ok", report.ok())?;
        d.set_item("name", key)?;
        d.set_item("nodes_checked", report.nodes_checked)?;
        d.set_item("edges_checked", report.edges_checked)?;
        d.set_item("node_violations", report.node_violations)?;
        d.set_item("edge_violations", report.edge_violations)?;

        let samples = PyList::empty_bound(py);
        for v in &report.samples {
            let s = PyDict::new_bound(py);
            s.set_item(
                "kind",
                match v.kind {
                    ElementKind::Node => "node",
                    ElementKind::Edge => "edge",
                },
            )?;
            s.set_item("id", v.id)?;
            s.set_item("labels", v.labels.clone())?;
            let props = PyDict::new_bound(py);
            for (k, t) in &v.props {
                props.set_item(k, format!("{t}"))?;
            }
            s.set_item("props", props)?;
            samples.append(s)?;
        }
        d.set_item("samples", samples)?;
        Ok(d.into_py(py))
    }

    fn exec_create_index<'py>(
        &self,
        py: Python<'py>,
        name: Option<String>,
        label: &str,
        prop: &str,
        kind: gqlrust::syntax::statement::IndexKindStmt,
    ) -> PyResult<PyObject> {
        use gqlrust::store::secondary_index::IndexKind;
        let store_kind = match kind {
            gqlrust::syntax::statement::IndexKindStmt::Hash => IndexKind::Hash,
            gqlrust::syntax::statement::IndexKindStmt::BTree => IndexKind::BTree,
        };
        let final_name = name.unwrap_or_else(|| {
            let suffix = match store_kind {
                IndexKind::Hash => "hash",
                IndexKind::BTree => "btree",
            };
            format!("{label}_{prop}_{suffix}")
        });
        let spec = self
            .store
            .secondary_indexes_mut()
            .build_declared(&self.store, final_name, label, prop, store_kind)
            .map_err(PyValueError::new_err)?;
        let d = PyDict::new_bound(py);
        d.set_item("ok", true)?;
        d.set_item("kind", "index")?;
        d.set_item("name", spec.name.as_str())?;
        d.set_item("label", spec.label.as_str())?;
        d.set_item("prop", spec.prop.as_str())?;
        d.set_item(
            "index_kind",
            match spec.kind {
                IndexKind::Hash => "HASH",
                IndexKind::BTree => "BTREE",
            },
        )?;
        d.set_item("entries", spec.entries)?;
        Ok(d.into_py(py))
    }

    fn exec_drop_index<'py>(&self, py: Python<'py>, name: &str) -> PyResult<PyObject> {
        let dropped = self.store.secondary_indexes_mut().drop_named(name);
        let d = PyDict::new_bound(py);
        d.set_item("ok", dropped)?;
        d.set_item("kind", "index")?;
        d.set_item("name", name)?;
        if !dropped {
            d.set_item("error", "index not found")?;
        }
        Ok(d.into_py(py))
    }

    fn exec_show_indexes<'py>(&self, py: Python<'py>) -> PyResult<PyObject> {
        use gqlrust::store::secondary_index::IndexKind;
        let idx = self.store.secondary_indexes();
        let out = PyList::empty_bound(py);
        for spec in idx.list() {
            let d = PyDict::new_bound(py);
            d.set_item("name", spec.name.as_str())?;
            d.set_item("label", spec.label.as_str())?;
            d.set_item("prop", spec.prop.as_str())?;
            d.set_item(
                "kind",
                match spec.kind {
                    IndexKind::Hash => "HASH",
                    IndexKind::BTree => "BTREE",
                },
            )?;
            d.set_item("auto", spec.auto)?;
            d.set_item("entries", spec.entries)?;
            out.append(d)?;
        }
        Ok(out.into_py(py))
    }
}

fn validation_to_py(py: Python<'_>, v: &ValidationStatus) -> PyResult<PyObject> {
    let d = PyDict::new_bound(py);
    d.set_item("nodes_checked", v.against_node_count)?;
    d.set_item("edges_checked", v.against_edge_count)?;
    d.set_item("violations", v.violations)?;
    d.set_item("validated_at_unix", v.validated_at_unix)?;
    Ok(d.into_py(py))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn build_schema_from_body(body: &[TypeElement]) -> Schema {
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

fn ddl_response(py: Python<'_>, message: &str) -> PyResult<PyObject> {
    let d = PyDict::new_bound(py);
    d.set_item("ok", true)?;
    d.set_item("kind", "ddl")?;
    d.set_item("message", message)?;
    Ok(d.into_py(py))
}

#[pyfunction]
fn open(path: &str) -> PyResult<Connection> {
    let store = LazyGraphStore::open(Path::new(path))
        .map_err(|e| PyRuntimeError::new_err(format!("open failed: {e}")))?;
    // Eagerly build the LTJ TripleIndex so the first user `execute()` runs
    // at warm-cache speed instead of paying the ~700ms cold build. The
    // total time spent in `gqlite.open()` is unchanged in spirit — we
    // shift the work out of the per-query critical path. Callers that want
    // the lazy behaviour can construct a Connection in Rust and skip the
    // warm step.
    let index = {
        let scratch = Runtime::new(&store);
        scratch.warm_triple_index()
    };
    Ok(Connection {
        store,
        db_path: Path::new(path).to_path_buf(),
        triple_index: RefCell::new(Some(index)),
    })
}

#[pyfunction]
fn import_json(db_path: &str, json_path: &str) -> PyResult<()> {
    let g = MemoryGraphStore::from_file(Path::new(json_path))
        .map_err(|e| PyRuntimeError::new_err(format!("load json: {e}")))?;
    g.save(Path::new(db_path))
        .map_err(|e| PyRuntimeError::new_err(format!("save: {e}")))?;
    Ok(())
}

#[pyfunction]
fn import_csv(db_path: &str, csv_dir: &str) -> PyResult<()> {
    let g = csv_loader::load_from_csv_dir(Path::new(csv_dir))
        .map_err(|e| PyRuntimeError::new_err(format!("load csv: {e}")))?;
    g.save(Path::new(db_path))
        .map_err(|e| PyRuntimeError::new_err(format!("save: {e}")))?;
    Ok(())
}

fn value_to_py<'py>(py: Python<'py>, store: &LazyGraphStore, v: &Value) -> PyResult<PyObject> {
    Ok(match v {
        Value::Null => py.None(),
        Value::Int(n) => n.into_py(py),
        Value::Float(x) => x.into_py(py),
        Value::Str(s) => s.into_py(py),
        Value::Bool(b) => b.into_py(py),
        Value::List(items) => {
            let lst = PyList::empty_bound(py);
            for it in items {
                lst.append(value_to_py(py, store, it)?)?;
            }
            lst.into_py(py)
        }
        Value::Record(fields) => {
            let d = PyDict::new_bound(py);
            for (k, v) in fields {
                d.set_item(k, value_to_py(py, store, v)?)?;
            }
            d.into_py(py)
        }
        // ISO §4.4.4 reference values. Materialize the same dict
        // shape the Raw path already uses for `pathvalue_to_py`:
        // `{kind, id, labels, props}`. Round-trippable to Python and
        // identifiable by the `kind` field.
        Value::Node(id) => {
            let d = PyDict::new_bound(py);
            d.set_item("kind", "node")?;
            d.set_item("id", *id)?;
            let labels: Vec<String> = store
                .node_labels(*id)
                .required_labels()
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            d.set_item("labels", labels)?;
            let props = PyDict::new_bound(py);
            for (k, vv) in store.node_props(*id).iter() {
                props.set_item(k, value_to_py(py, store, vv)?)?;
            }
            d.set_item("props", props)?;
            d.into_py(py)
        }
        Value::Edge(id) => {
            let d = PyDict::new_bound(py);
            d.set_item("kind", "edge")?;
            d.set_item("id", *id)?;
            let labels: Vec<String> = store
                .edge_labels(*id)
                .required_labels()
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            d.set_item("labels", labels)?;
            let props = PyDict::new_bound(py);
            for (k, vv) in store.edge_props(*id).iter() {
                props.set_item(k, value_to_py(py, store, vv)?)?;
            }
            d.set_item("props", props)?;
            d.into_py(py)
        }
        // A named path projects to a list of its element dicts (nodes and
        // edges in match order), the same `{kind, id, labels, props}`
        // shape used everywhere else.
        Value::Path(items) => {
            let lst = PyList::empty_bound(py);
            for it in items {
                lst.append(value_to_py(py, store, it)?)?;
            }
            lst.into_py(py)
        }
    })
}

fn raw_to_pylist<'py>(
    py: Python<'py>,
    store: &LazyGraphStore,
    ir: &IntermediateResult,
) -> PyResult<Bound<'py, PyList>> {
    let out = PyList::empty_bound(py);
    for row in &ir.rows {
        let d = PyDict::new_bound(py);
        // _paths: list of paths (one per pattern in a comma-join), each
        // path a list of node/edge dicts in match order. Underscore
        // prefix avoids collisions with user-defined variable names.
        let paths = PyList::empty_bound(py);
        for path in &row.paths {
            let p = PyList::empty_bound(py);
            for pv in &path.0 {
                p.append(pathvalue_to_py(py, store, pv)?)?;
            }
            paths.append(p)?;
        }
        d.set_item("_paths", paths)?;
        for (var, pv) in row.assignment.m.iter() {
            d.set_item(var, pathvalue_to_py(py, store, pv)?)?;
        }
        out.append(d)?;
    }
    Ok(out)
}

fn pathvalue_to_py<'py>(
    py: Python<'py>,
    store: &LazyGraphStore,
    pv: &PathValue,
) -> PyResult<PyObject> {
    Ok(match pv {
        PathValue::Node(id) => {
            let d = PyDict::new_bound(py);
            d.set_item("kind", "node")?;
            d.set_item("id", *id)?;
            let labels: Vec<String> = store
                .node_labels(*id)
                .required_labels()
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            d.set_item("labels", labels)?;
            let props = PyDict::new_bound(py);
            for (k, v) in store.node_props(*id).iter() {
                props.set_item(k, value_to_py(py, store, v)?)?;
            }
            d.set_item("props", props)?;
            d.into_py(py)
        }
        PathValue::EdgeDirectional(id) | PathValue::EdgeUndirectional(id) => {
            let d = PyDict::new_bound(py);
            d.set_item("kind", "edge")?;
            d.set_item("id", *id)?;
            let labels: Vec<String> = store
                .edge_labels(*id)
                .required_labels()
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            d.set_item("labels", labels)?;
            let props = PyDict::new_bound(py);
            for (k, vv) in store.edge_props(*id).iter() {
                props.set_item(k, value_to_py(py, store, vv)?)?;
            }
            d.set_item("props", props)?;
            d.into_py(py)
        }
        PathValue::Nothing => py.None(),
        PathValue::Group(items) | PathValue::Path(items) => {
            let lst = PyList::empty_bound(py);
            for it in items {
                lst.append(pathvalue_to_py(py, store, it)?)?;
            }
            lst.into_py(py)
        }
    })
}

#[pymodule]
fn frogql(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Connection>()?;
    m.add_function(wrap_pyfunction!(open, m)?)?;
    m.add_function(wrap_pyfunction!(import_json, m)?)?;
    m.add_function(wrap_pyfunction!(import_csv, m)?)?;
    Ok(())
}
