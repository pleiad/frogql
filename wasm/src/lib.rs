//! froGQL compiled to WebAssembly: an in-browser, in-RAM graph engine.
//!
//! Wraps `MemoryGraphStore` (the in-memory backend) plus the shared
//! compiler/runtime, exposing a `Connection` to JavaScript via
//! `wasm-bindgen`. There is no filesystem in the browser, so this binding
//! works entirely with the JSON shape `MemoryGraphStore::from_json_str`
//! consumes and `to_json_string` produces — persist that string in
//! IndexedDB to keep a graph across sessions.
//!
//! Surface mirrors the Python / Node bindings as far as the in-memory
//! backend allows. There is no graph-type catalog and no secondary index,
//! so DDL (`CREATE/USE/DROP GRAPH TYPE`, `CREATE INDEX`) is rejected;
//! queries typecheck against the inferred DEFAULT schema. INSERT / SET /
//! REMOVE / DELETE all work in RAM via the same `MutationOverlay` as the
//! native backends.

use std::cell::RefCell;
use std::sync::Arc;

use serde_json::{json, Map, Value as Json};
use wasm_bindgen::prelude::*;

use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::model::value::{Id, PathValue, Value};
use gqlrust::parser::parse_statement;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::ltj::triple_index::TripleIndex;
use gqlrust::runtime::result::{IntermediateResult, QueryResult};
use gqlrust::syntax::expr::Expr;
use gqlrust::syntax::query::ReturnItem;
use gqlrust::syntax::statement::Statement;
use gqlrust::typing::inference::infer_simple_schema;
use gqlrust::typing::variable_type::Schema;

/// A live in-memory graph plus the caches that keep query latency flat.
#[wasm_bindgen]
pub struct Connection {
    store: MemoryGraphStore,
    /// Shared LTJ TripleIndex, built lazily on first query and reused
    /// across calls. Cleared after every successful DML.
    triple_index: RefCell<Option<Arc<TripleIndex>>>,
    /// Inferred DEFAULT schema for typechecking. Recomputed lazily and
    /// invalidated after every successful DML.
    schema: RefCell<Option<Schema>>,
}

/// Parse a JSON graph document (`{"nodes": [...], "edges": [...]}`) and
/// open a connection over it. Warms the LTJ index eagerly so the first
/// query is as fast as the rest.
#[wasm_bindgen]
pub fn open_json(json: &str) -> Result<Connection, JsError> {
    console_error_panic_hook::set_once();
    let store = MemoryGraphStore::from_json_str(json).map_err(|e| JsError::new(&format!("{e}")))?;
    let conn = Connection {
        store,
        triple_index: RefCell::new(None),
        schema: RefCell::new(None),
    };
    // Warm the index once at open, matching the Python/Node bindings.
    let _ = conn.triple_index_arc();
    Ok(conn)
}

#[wasm_bindgen]
impl Connection {
    #[wasm_bindgen(getter)]
    pub fn node_count(&self) -> u32 {
        self.store.node_count() as u32
    }

    #[wasm_bindgen(getter)]
    pub fn edge_count(&self) -> u32 {
        self.store.edge_count() as u32
    }

    /// Execute one GQL statement. Read queries return an array of row
    /// objects; data-modifying statements return a counters object.
    /// `limit` caps the number of rows (default 100 when omitted on the
    /// JS side).
    pub fn execute(&self, query: &str, limit: Option<u32>) -> Result<JsValue, JsError> {
        let limit = limit.unwrap_or(100) as usize;
        let stmt = parse_statement(query).map_err(|e| JsError::new(&e))?;
        match stmt {
            Statement::Query(_) => self.exec_query(query, limit),
            Statement::DataModification(dm) => self.exec_dm(dm),
            other => Err(JsError::new(&format!(
                "statement not supported by the in-memory wasm backend (no catalog / index): {other:?}"
            ))),
        }
    }

    /// Serialise the live merged view (base + overlay) to a JSON string —
    /// the unit to hand IndexedDB for persistence. Re-open it later with
    /// `open_json`.
    pub fn to_json(&self) -> String {
        self.store.to_json_string()
    }

    /// `{ node_labels, edge_labels, node_count, edge_count }`, mirroring
    /// the Python/Node `schema()` summary.
    pub fn schema(&self) -> Result<JsValue, JsError> {
        use std::collections::BTreeSet;
        let mut node_labels: BTreeSet<String> = BTreeSet::new();
        for nid in 0..self.store.node_count() as u32 {
            for l in self.store.node_labels(nid).required_labels() {
                node_labels.insert(l.to_string());
            }
        }
        let mut edge_labels: BTreeSet<String> = BTreeSet::new();
        for eid in 0..self.store.edge_count() as u32 {
            for l in self.store.edge_labels(eid).required_labels() {
                edge_labels.insert(l.to_string());
            }
        }
        let v = json!({
            "node_labels": node_labels.into_iter().collect::<Vec<_>>(),
            "edge_labels": edge_labels.into_iter().collect::<Vec<_>>(),
            "node_count": self.store.node_count(),
            "edge_count": self.store.edge_count(),
        });
        to_js(&v)
    }
}

// Internal helpers (no #[wasm_bindgen]).
impl Connection {
    /// Build (once) and return the shared TripleIndex Arc.
    fn triple_index_arc(&self) -> Arc<TripleIndex> {
        if self.triple_index.borrow().is_none() {
            let scratch = Runtime::new(&self.store);
            *self.triple_index.borrow_mut() = Some(scratch.warm_triple_index());
        }
        self.triple_index
            .borrow()
            .clone()
            .expect("triple index just built")
    }

    fn runtime(&self) -> Runtime<'_, MemoryGraphStore> {
        Runtime::with_triple_index(&self.store, self.triple_index_arc())
    }

    /// The inferred DEFAULT schema, cached until the next mutation.
    fn active_schema(&self) -> Schema {
        if self.schema.borrow().is_none() {
            *self.schema.borrow_mut() = Some(infer_simple_schema(&self.store));
        }
        self.schema.borrow().clone().expect("schema just inferred")
    }

    fn invalidate_caches(&self) {
        *self.triple_index.borrow_mut() = None;
        *self.schema.borrow_mut() = None;
    }

    fn exec_query(&self, query: &str, limit: usize) -> Result<JsValue, JsError> {
        let v = self
            .query_json(query, limit)
            .map_err(|e| JsError::new(&e))?;
        to_js(&v)
    }

    fn exec_dm(&self, dm: gqlrust::syntax::dm::DmStatement) -> Result<JsValue, JsError> {
        let v = self.dm_json(dm).map_err(|e| JsError::new(&e))?;
        to_js(&v)
    }

    /// Engine core for read queries, returning a `serde_json` array of row
    /// objects. Split out from `exec_query` so it is testable on the host
    /// target (the `JsValue` marshaling needs a JS runtime; this does not).
    fn query_json(&self, query: &str, limit: usize) -> Result<Json, String> {
        let schema = self.active_schema();
        let compiled = gqlrust::compile_query_with_diagnostics_with(&schema, query)
            .map_err(|e| e.message())?;
        let q = compiled.query;

        if compiled.guaranteed_empty {
            return Ok(Json::Array(vec![]));
        }

        let rt = self.runtime();
        let rows = match rt.run_query(&q, limit) {
            QueryResult::Projected(rows) => {
                let headers = projection_headers(&q);
                let mut out: Vec<Json> = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut obj = Map::new();
                    for (i, v) in row.into_iter().enumerate() {
                        let key = headers.get(i).cloned().unwrap_or_else(|| format!("col{i}"));
                        obj.insert(key, value_to_json(&self.store, &v));
                    }
                    out.push(Json::Object(obj));
                }
                Json::Array(out)
            }
            QueryResult::Raw(ir) => raw_to_json(&self.store, &ir),
        };
        Ok(rows)
    }

    /// Engine core for data-modifying statements; returns the counters
    /// object. Host-testable, same split rationale as `query_json`.
    fn dm_json(&self, dm: gqlrust::syntax::dm::DmStatement) -> Result<Json, String> {
        // No catalog in the in-memory backend, so DEFAULT semantics: no
        // G2000 validation schema.
        let exec = gqlrust::runtime::dm::run_dm(&self.store, &dm, None)?;
        self.invalidate_caches();
        Ok(json!({
            "nodes_inserted": exec.nodes_inserted,
            "edges_inserted": exec.edges_inserted,
            "nodes_deleted": exec.nodes_deleted,
            "edges_deleted": exec.edges_deleted,
            "nodes_modified": exec.nodes_modified,
            "edges_modified": exec.edges_modified,
            "rows": exec.rows.len(),
        }))
    }
}

/// ISO §14.11 SR 8a header derivation, mirroring the Python binding: an
/// aliased item uses its alias; a bare variable reference uses the
/// variable name; everything else falls back to a positional `colN`.
fn projection_headers(q: &gqlrust::syntax::query::Query) -> Vec<String> {
    q.returns
        .as_ref()
        .map(|items| {
            items
                .iter()
                .enumerate()
                .map(|(i, it)| {
                    if let Some(a) = it.alias() {
                        return a.to_string();
                    }
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
        .unwrap_or_default()
}

fn to_js(v: &Json) -> Result<JsValue, JsError> {
    // `serde_wasm_bindgen::to_value` serializes maps as JS `Map` by default,
    // which `serde_json` objects hit — yielding `{}` under `JSON.stringify`
    // and bracket access. `json_compatible()` serializes maps as plain
    // objects (and numbers/strings JSON-style), which is what JS callers
    // expect.
    use serde::Serialize;
    let serializer = serde_wasm_bindgen::Serializer::json_compatible();
    v.serialize(&serializer)
        .map_err(|e| JsError::new(&e.to_string()))
}

fn value_to_json(store: &MemoryGraphStore, v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Int(n) => json!(n),
        Value::Float(x) => serde_json::Number::from_f64(*x)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Value::Str(s) => json!(s),
        Value::Bool(b) => json!(b),
        Value::List(items) => {
            Json::Array(items.iter().map(|it| value_to_json(store, it)).collect())
        }
        Value::Record(fields) => {
            let mut m = Map::new();
            for (k, vv) in fields {
                m.insert(k.clone(), value_to_json(store, vv));
            }
            Json::Object(m)
        }
        Value::Node(id) => node_ref_json(store, *id),
        Value::Edge(id) => edge_ref_json(store, *id),
        // A named path projects to a JSON array of its element objects.
        Value::Path(items) => {
            Json::Array(items.iter().map(|it| value_to_json(store, it)).collect())
        }
    }
}

fn node_ref_json(store: &MemoryGraphStore, id: Id) -> Json {
    let labels: Vec<String> = store
        .node_labels(id)
        .required_labels()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    let mut props = Map::new();
    for (k, vv) in store.node_props(id).iter() {
        props.insert(k.clone(), value_to_json(store, vv));
    }
    json!({ "kind": "node", "id": id, "labels": labels, "props": Json::Object(props) })
}

fn edge_ref_json(store: &MemoryGraphStore, id: Id) -> Json {
    let labels: Vec<String> = store
        .edge_labels(id)
        .required_labels()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    let mut props = Map::new();
    for (k, vv) in store.edge_props(id).iter() {
        props.insert(k.clone(), value_to_json(store, vv));
    }
    json!({ "kind": "edge", "id": id, "labels": labels, "props": Json::Object(props) })
}

fn pathvalue_to_json(store: &MemoryGraphStore, pv: &PathValue) -> Json {
    match pv {
        PathValue::Node(id) => node_ref_json(store, *id),
        PathValue::EdgeDirectional(id) | PathValue::EdgeUndirectional(id) => {
            edge_ref_json(store, *id)
        }
        PathValue::Nothing => Json::Null,
        PathValue::Group(items) | PathValue::Path(items) => Json::Array(
            items
                .iter()
                .map(|it| pathvalue_to_json(store, it))
                .collect(),
        ),
    }
}

fn raw_to_json(store: &MemoryGraphStore, ir: &IntermediateResult) -> Json {
    let mut out: Vec<Json> = Vec::with_capacity(ir.rows.len());
    for row in &ir.rows {
        let mut obj = Map::new();
        let paths: Vec<Json> = row
            .paths
            .iter()
            .map(|path| {
                Json::Array(
                    path.0
                        .iter()
                        .map(|pv| pathvalue_to_json(store, pv))
                        .collect(),
                )
            })
            .collect();
        obj.insert("_paths".to_string(), Json::Array(paths));
        for (var, pv) in row.assignment.m.iter() {
            obj.insert(var.clone(), pathvalue_to_json(store, pv));
        }
        out.push(Json::Object(obj));
    }
    Json::Array(out)
}

#[cfg(test)]
mod tests {
    //! Host-target tests for the engine core. They exercise `query_json` /
    //! `dm_json` (the `serde_json` half), which carry all the real logic;
    //! the `JsValue` marshaling on top is a thin wrapper that only runs in
    //! a JS environment.
    use super::*;

    const GRAPH: &str = r#"{
        "nodes": [
            {"id": "a", "labels": ["Person"], "props": {"name": "Alice"}},
            {"id": "b", "labels": ["Person"], "props": {"name": "Bob"}}
        ],
        "edges": [
            {"id": "e1", "labels": ["KNOWS"], "endpoints": ["a", "b"],
             "directionality": "->", "props": {}}
        ]
    }"#;

    // `JsError` is not `Debug` on the host target, so unwrap via `match`
    // rather than `.ok().expect()` / `.unwrap()`.
    fn open(json: &str) -> Connection {
        match open_json(json) {
            Ok(c) => c,
            Err(_) => panic!("open_json failed"),
        }
    }

    fn conn() -> Connection {
        open(GRAPH)
    }

    fn dm(conn: &Connection, input: &str) -> Json {
        match parse_statement(input).expect("parse") {
            Statement::DataModification(d) => conn.dm_json(d).expect("dm"),
            other => panic!("expected DM, got {other:?}"),
        }
    }

    #[test]
    fn open_reports_counts() {
        let c = conn();
        assert_eq!(c.node_count(), 2);
        assert_eq!(c.edge_count(), 1);
    }

    #[test]
    fn projected_query_returns_aliased_rows() {
        let c = conn();
        let rows = c
            .query_json("MATCH (n:Person) RETURN n.name AS name", 100)
            .expect("query");
        let arr = rows.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let names: Vec<&str> = arr.iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"Alice"));
        assert!(names.contains(&"Bob"));
    }

    #[test]
    fn projected_node_reference_is_expanded() {
        let c = conn();
        let rows = c
            .query_json("MATCH (n:Person) RETURN n", 100)
            .expect("query");
        let first = &rows.as_array().unwrap()[0]["n"];
        assert_eq!(first["kind"], "node");
        assert_eq!(first["labels"][0], "Person");
        assert!(first["props"]["name"].is_string());
    }

    #[test]
    fn raw_query_exposes_paths_and_vars() {
        let c = conn();
        let rows = c.query_json("MATCH (n:Person)", 100).expect("query");
        let arr = rows.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr[0]["_paths"].is_array());
        assert_eq!(arr[0]["n"]["kind"], "node");
    }

    #[test]
    fn dm_insert_then_query_sees_new_node() {
        let c = conn();
        let counters = dm(&c, "INSERT (cara:Person {name: 'Cara'})");
        assert_eq!(counters["nodes_inserted"], 1);
        // Cache invalidation must let the next query see the insert.
        let rows = c
            .query_json("MATCH (n:Person) RETURN n.name AS name", 100)
            .expect("query");
        assert_eq!(rows.as_array().unwrap().len(), 3);
    }

    #[test]
    fn to_json_round_trips_through_open() {
        let c = conn();
        dm(&c, "INSERT (cara:Person {name: 'Cara'})");
        let snapshot = c.to_json();
        let reopened = open(&snapshot);
        assert_eq!(reopened.node_count(), 3);
        assert_eq!(reopened.edge_count(), 1);
    }
}
