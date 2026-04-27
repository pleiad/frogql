use std::path::Path;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use gqlrust::model::csv_loader;
use gqlrust::model::graph::Graph;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::model::value::{PathValue, Value};
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::{IntermediateResult, QueryResult};
use gqlrust::store::lazy::LazyGraphStore;

#[pyclass(unsendable)]
struct Connection {
    store: LazyGraphStore,
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
    fn execute<'py>(
        &self,
        py: Python<'py>,
        query: &str,
        limit: usize,
    ) -> PyResult<Bound<'py, PyList>> {
        let q = gqlrust::compile_query(query)
            .map_err(|e| PyValueError::new_err(format!("parse error: {e}")))?;

        let rt = Runtime::new(&self.store);
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
                            .map(|(i, it)| it.alias.clone().unwrap_or_else(|| format!("col{i}")))
                            .collect()
                    })
                    .unwrap_or_default();

                let out = PyList::empty_bound(py);
                for row in rows {
                    let d = PyDict::new_bound(py);
                    for (i, v) in row.into_iter().enumerate() {
                        let key = headers.get(i).cloned().unwrap_or_else(|| format!("col{i}"));
                        d.set_item(key, value_to_py(py, &v)?)?;
                    }
                    out.append(d)?;
                }
                Ok(out)
            }
            QueryResult::Raw(ir) => raw_to_pylist(py, &self.store, &ir),
        }
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
            "<gqlite.Connection nodes={} edges={}>",
            self.store.node_count(),
            self.store.edge_count()
        )
    }
}

#[pyfunction]
fn open(path: &str) -> PyResult<Connection> {
    let store = LazyGraphStore::open(Path::new(path))
        .map_err(|e| PyRuntimeError::new_err(format!("open failed: {e}")))?;
    Ok(Connection { store })
}

#[pyfunction]
fn import_json(db_path: &str, json_path: &str) -> PyResult<()> {
    let g = Graph::from_file(Path::new(json_path))
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

fn value_to_py<'py>(py: Python<'py>, v: &Value) -> PyResult<PyObject> {
    Ok(match v {
        Value::Int(n) => n.into_py(py),
        Value::Float(x) => x.into_py(py),
        Value::Str(s) => s.into_py(py),
        Value::Bool(b) => b.into_py(py),
        Value::List(items) => {
            let lst = PyList::empty_bound(py);
            for it in items {
                lst.append(value_to_py(py, it)?)?;
            }
            lst.into_py(py)
        }
        Value::Record(fields) => {
            let d = PyDict::new_bound(py);
            for (k, v) in fields {
                d.set_item(k, value_to_py(py, v)?)?;
            }
            d.into_py(py)
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
                props.set_item(k, value_to_py(py, v)?)?;
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
            d.into_py(py)
        }
        PathValue::Nothing => py.None(),
        PathValue::Group(items) => {
            let lst = PyList::empty_bound(py);
            for it in items {
                lst.append(pathvalue_to_py(py, store, it)?)?;
            }
            lst.into_py(py)
        }
    })
}

#[pymodule]
fn gqlite(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Connection>()?;
    m.add_function(wrap_pyfunction!(open, m)?)?;
    m.add_function(wrap_pyfunction!(import_json, m)?)?;
    m.add_function(wrap_pyfunction!(import_csv, m)?)?;
    Ok(())
}
