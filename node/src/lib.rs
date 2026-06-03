//! Node.js bindings for froGQL via napi-rs. Mirrors the Python surface
//! in `python/src/lib.rs` line-for-line: `open`, `importJson`, `importCsv`,
//! and a `Connection` class with `execute`, `save`, `schema`,
//! `graphTypes`, `nodeCount`, `edgeCount`.
//!
//! Return shape from `execute` is a `serde_json::Value` that napi-rs
//! auto-converts to native JS types (Array / Object / Number / String /
//! Boolean / null). Queries with a RETURN clause produce a list of dicts
//! keyed by alias; queries without RETURN produce a list of dicts with
//! a `_paths` key plus per-variable bindings; DDL produces a single dict
//! with `ok`/`kind` markers; DML produces a counters dict.

#![allow(clippy::useless_conversion)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use napi_derive::napi;
use serde_json::{json, Map, Value as JsonValue};

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

fn err<E: ToString>(e: E) -> napi::Error {
    napi::Error::from_reason(e.to_string())
}

// === Exported TS types ======================================================
//
// `#[napi(object)]` structs surface as TS interfaces in the generated
// `index.d.ts` even when no method returns them directly. They give
// consumers names to cast `execute()` results to, since execute() is
// polymorphic and returns `unknown` (we'd need to split it into
// executeQuery / executeDdl / executeDml to give it a single static
// type, which loses parity with the Python binding).

/// Result of `Connection.schema()`.
#[napi(object)]
pub struct SchemaSummary {
    pub node_labels: Vec<String>,
    pub edge_labels: Vec<String>,
    pub node_count: u32,
    pub edge_count: u32,
}

/// One entry in `Connection.graphTypes()`.
#[napi(object)]
pub struct GraphTypeSummary {
    pub name: String,
    pub active: bool,
    /// Number of node types declared in this graph type, if known.
    pub nodes: Option<u32>,
    /// Number of edge types declared in this graph type, if known.
    pub edges: Option<u32>,
}

/// Shape of a node reference inside an `execute()` row. `kind` is
/// always `"node"`. Cast `execute()` rows to `{ x: NodeRef }` etc.
#[napi(object)]
pub struct NodeRef {
    pub kind: String,
    pub id: u32,
    pub labels: Vec<String>,
    /// Free-form property bag. Values are JSON-compatible (number /
    /// string / boolean / null / nested object / array).
    pub props: serde_json::Value,
}

/// Shape of an edge reference inside an `execute()` row. `kind` is
/// always `"edge"`. Symmetric with `NodeRef`: `props` is always
/// populated, whether the edge arrives via `_paths` or via `RETURN e`.
#[napi(object)]
pub struct EdgeRef {
    pub kind: String,
    pub id: u32,
    pub labels: Vec<String>,
    /// Free-form property bag. Values are JSON-compatible (number /
    /// string / boolean / null / nested object / array).
    pub props: serde_json::Value,
}

/// Counters returned from a successful DML statement (INSERT / SET /
/// REMOVE / DELETE / DETACH DELETE).
#[napi(object)]
pub struct DmCounters {
    pub nodes_inserted: u32,
    pub edges_inserted: u32,
    pub nodes_deleted: u32,
    pub edges_deleted: u32,
    pub nodes_modified: u32,
    pub edges_modified: u32,
    pub rows: u32,
}

/// Result envelope for `CREATE / USE / DROP GRAPH TYPE`.
#[napi(object)]
pub struct DdlOk {
    pub ok: bool,
    /// Always `"ddl"` for catalog statements.
    pub kind: String,
    pub message: String,
}

/// Result envelope for `CREATE / DROP INDEX`.
#[napi(object)]
pub struct IndexResult {
    pub ok: bool,
    /// Always `"index"`.
    pub kind: String,
    pub name: String,
    /// Present on CREATE, absent on DROP.
    pub label: Option<String>,
    /// Present on CREATE, absent on DROP.
    pub prop: Option<String>,
    /// `"HASH"` or `"BTREE"`. Present on CREATE only.
    pub index_kind: Option<String>,
    /// Number of (label, prop) entries indexed. Present on CREATE only.
    pub entries: Option<u32>,
    /// Present when DROP failed (index not found).
    pub error: Option<String>,
}

/// One entry in `SHOW INDEXES`.
#[napi(object)]
pub struct IndexSummary {
    pub name: String,
    pub label: String,
    pub prop: String,
    /// `"HASH"` or `"BTREE"`.
    pub kind: String,
    /// `true` for auto-built indexes (memory-only), `false` for
    /// DDL-declared (persisted in `.gdb`).
    pub auto: bool,
    pub entries: u32,
}

#[napi]
pub struct Connection {
    store: LazyGraphStore,
    db_path: PathBuf,
    /// Shared LTJ TripleIndex. Built eagerly on `open()` so the first
    /// `execute()` runs at warm-cache speed (skips ~700ms cold build
    /// on SF0.1). Invalidated after every successful DML mutation.
    triple_index: RefCell<Option<Arc<TripleIndex>>>,
}

// LazyGraphStore is Send (RefCell<T> is Send when T: Send). napi-rs's
// generated class wrappers don't require Sync because the napi runtime
// is single-threaded per V8 isolate.
unsafe impl Send for Connection {}

#[napi]
impl Connection {
    #[napi(getter)]
    pub fn node_count(&self) -> u32 {
        self.store.node_count()
    }

    #[napi(getter)]
    pub fn edge_count(&self) -> u32 {
        self.store.edge_count()
    }

    /// Run a GQL statement. The return is polymorphic — depends on
    /// statement kind:
    /// - Query with RETURN: `Array<Record<string, unknown>>` keyed by alias
    /// - Query without RETURN: `Array<{ _paths, ...vars }>`
    /// - CREATE / USE / DROP GRAPH TYPE: `DdlOk`
    /// - SHOW GRAPH TYPES: `Array<GraphTypeSummary>`
    /// - SHOW GRAPH TYPE / SHOW CURRENT GRAPH TYPE / VALIDATE GRAPH TYPE: object
    /// - CREATE / DROP INDEX: `IndexResult`
    /// - SHOW INDEXES: `Array<IndexSummary>`
    /// - DML: `DmCounters`
    ///
    /// Cast the return to the expected shape:
    /// `const rows = conn.execute("MATCH ...") as Array<{ x: NodeRef }>`
    #[napi(ts_return_type = "unknown")]
    pub fn execute(&self, query: String, limit: Option<u32>) -> napi::Result<JsonValue> {
        let limit = limit.unwrap_or(100) as usize;
        let stmt = parse_statement(&query).map_err(err)?;
        match stmt {
            Statement::CreateGraphType { name, body } => self.exec_create(&name, &body),
            Statement::UseGraphType {
                name,
                refresh_default,
            } => self.exec_use(&name, refresh_default),
            Statement::DropGraphType { name } => self.exec_drop(&name),
            Statement::ShowGraphTypes => Ok(self.list_graph_types()),
            Statement::ShowGraphType { name } => self.exec_show_graph_type(&name),
            Statement::ShowCurrentGraphType => Ok(self.exec_show_current()),
            Statement::ValidateGraphType { name } => self.exec_validate(&name),
            Statement::CreateIndex {
                name,
                label,
                prop,
                kind,
            } => self.exec_create_index(name, &label, &prop, kind),
            Statement::DropIndex { name } => Ok(self.exec_drop_index(&name)),
            Statement::ShowIndexes => Ok(self.exec_show_indexes()),
            Statement::DataModification(dm) => self.exec_dm(dm),
            Statement::Query(_) => self.exec_query(&query, limit),
        }
    }

    /// Persist the merged base+overlay state back to the file the
    /// connection was opened from. Mirrors `.save` REPL command and
    /// SQLite's explicit-save model.
    #[napi]
    pub fn save(&self) -> napi::Result<()> {
        self.store
            .save(&self.db_path)
            .map_err(|e| err(format!("save: {e}")))
    }

    /// List graph types currently in the catalog with active markers.
    #[napi]
    pub fn graph_types(&self) -> Vec<GraphTypeSummary> {
        self.list_graph_types_typed()
    }

    /// Lightweight schema summary derived from the live graph: sorted
    /// label sets + node / edge counts. Does NOT consult the catalog.
    #[napi]
    pub fn schema(&self) -> SchemaSummary {
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
        SchemaSummary {
            node_labels: node_labels.into_iter().collect(),
            edge_labels: edge_labels.into_iter().collect(),
            node_count: self.store.node_count(),
            edge_count: self.store.edge_count(),
        }
    }
}

impl Connection {
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

    fn list_graph_types_typed(&self) -> Vec<GraphTypeSummary> {
        let cat = self.store.catalog();
        cat.list()
            .into_iter()
            .map(|(name, is_active)| {
                let (nodes, edges) = cat
                    .types
                    .get(name)
                    .map(|s| (Some(s.nodes.len() as u32), Some(s.edges.len() as u32)))
                    .unwrap_or((None, None));
                GraphTypeSummary {
                    name: name.clone(),
                    active: is_active,
                    nodes,
                    edges,
                }
            })
            .collect()
    }

    /// Same content as `list_graph_types_typed`, hand-converted to
    /// JsonValue for the polymorphic `execute()` return path. Adding a
    /// `serde::Serialize` derive on `GraphTypeSummary` would shave the
    /// boilerplate but napi-rs's `#[napi(object)]` macro doesn't add it.
    fn list_graph_types(&self) -> JsonValue {
        JsonValue::Array(
            self.list_graph_types_typed()
                .into_iter()
                .map(|t| {
                    let mut entry = Map::new();
                    entry.insert("name".into(), JsonValue::String(t.name));
                    entry.insert("active".into(), JsonValue::Bool(t.active));
                    if let Some(n) = t.nodes {
                        entry.insert("nodes".into(), json!(n));
                    }
                    if let Some(e) = t.edges {
                        entry.insert("edges".into(), json!(e));
                    }
                    JsonValue::Object(entry)
                })
                .collect(),
        )
    }

    fn exec_query(&self, query: &str, limit: usize) -> napi::Result<JsonValue> {
        let active = self.store.catalog().active_schema();
        let result = gqlrust::compile_query_with_diagnostics_with(&active, query)
            .map_err(|e| err(e.message()))?;
        let q = result.query;

        for w in &result.warnings {
            eprintln!("frogql warning: {w}");
        }

        if result.guaranteed_empty {
            return Ok(JsonValue::Array(vec![]));
        }

        let rt = self.runtime();
        let exec = rt.run_query(&q, limit);

        match exec {
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

                let mut out: Vec<JsonValue> = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut obj = Map::new();
                    for (i, v) in row.into_iter().enumerate() {
                        let key = headers.get(i).cloned().unwrap_or_else(|| format!("col{i}"));
                        obj.insert(key, value_to_json(&self.store, &v));
                    }
                    out.push(JsonValue::Object(obj));
                }
                Ok(JsonValue::Array(out))
            }
            QueryResult::Raw(ir) => Ok(raw_to_json(&self.store, &ir)),
        }
    }

    fn exec_dm(&self, dm: gqlrust::syntax::dm::DmStatement) -> napi::Result<JsonValue> {
        let active_name = self.store.catalog().active_name().map(str::to_string);
        let schema_for_validation = match active_name.as_deref() {
            None | Some("DEFAULT") => None,
            _ => Some(self.store.catalog().active_schema()),
        };
        let exec = gqlrust::runtime::dm::run_dm(&self.store, &dm, schema_for_validation.as_ref())
            .map_err(err)?;
        *self.triple_index.borrow_mut() = None;
        self.store.catalog_mut().mark_default_dirty();
        Ok(json!({
            "nodesInserted": exec.nodes_inserted,
            "edgesInserted": exec.edges_inserted,
            "nodesDeleted": exec.nodes_deleted,
            "edgesDeleted": exec.edges_deleted,
            "nodesModified": exec.nodes_modified,
            "edgesModified": exec.edges_modified,
            "rows": exec.rows.len(),
        }))
    }

    fn exec_create(&self, name: &str, body: &[TypeElement]) -> napi::Result<JsonValue> {
        let schema = build_schema_from_body(body);
        let n_nodes = schema.nodes.len();
        let n_edges = schema.edges.len();
        {
            let mut cat = self.store.catalog_mut();
            cat.register(name.to_string(), schema).map_err(err)?;
        }
        self.store
            .save_catalog()
            .map_err(|e| err(format!("save catalog: {e}")))?;
        Ok(ddl_response(&format!(
            "GRAPH TYPE '{name}' created ({n_nodes} node types, {n_edges} edge types)."
        )))
    }

    fn exec_use(&self, name: &str, refresh_default: bool) -> napi::Result<JsonValue> {
        if refresh_default {
            let schema = infer_simple_schema(&self.store);
            let n_nodes = schema.nodes.len();
            let n_edges = schema.edges.len();
            self.store.catalog_mut().install_default(schema);
            self.store
                .save_catalog()
                .map_err(|e| err(format!("save catalog: {e}")))?;
            Ok(ddl_response(&format!(
                "GRAPH TYPE 'DEFAULT' refreshed ({n_nodes} node types, {n_edges} edge types) and activated."
            )))
        } else {
            self.store.catalog_mut().set_active(name).map_err(err)?;
            self.store
                .save_catalog()
                .map_err(|e| err(format!("save catalog: {e}")))?;
            Ok(ddl_response(&format!("Active GRAPH TYPE: {name}.")))
        }
    }

    fn exec_drop(&self, name: &str) -> napi::Result<JsonValue> {
        {
            let mut cat = self.store.catalog_mut();
            cat.drop(name).map_err(err)?;
        }
        self.store
            .save_catalog()
            .map_err(|e| err(format!("save catalog: {e}")))?;
        Ok(ddl_response(&format!("GRAPH TYPE '{name}' dropped.")))
    }

    fn exec_show_graph_type(&self, name: &str) -> napi::Result<JsonValue> {
        let cat = self.store.catalog();
        let key = if name.eq_ignore_ascii_case("DEFAULT") {
            "DEFAULT"
        } else {
            name
        };
        let schema = cat
            .types
            .get(key)
            .ok_or_else(|| err(format!("graph type '{name}' not found")))?;
        let mut out = Map::new();
        out.insert("name".into(), JsonValue::String(key.into()));
        out.insert(
            "active".into(),
            JsonValue::Bool(cat.active_name() == Some(key)),
        );
        out.insert("nodes".into(), json!(schema.nodes.len()));
        out.insert("edges".into(), json!(schema.edges.len()));
        out.insert("formatted".into(), JsonValue::String(format_schema(schema)));
        if let Some(v) = cat.validation_for(key) {
            out.insert("validation".into(), validation_to_json(v));
        }
        Ok(JsonValue::Object(out))
    }

    fn exec_show_current(&self) -> JsonValue {
        let cat = self.store.catalog();
        let mut out = Map::new();
        match cat.active_name() {
            None => {
                out.insert("active".into(), JsonValue::Null);
                out.insert("formatted".into(), JsonValue::String("(none)".into()));
            }
            Some(name) => {
                out.insert("active".into(), JsonValue::String(name.into()));
                if let Some(schema) = cat.types.get(name) {
                    out.insert("nodes".into(), json!(schema.nodes.len()));
                    out.insert("edges".into(), json!(schema.edges.len()));
                    out.insert("formatted".into(), JsonValue::String(format_schema(schema)));
                    if let Some(v) = cat.validation_for(name) {
                        out.insert("validation".into(), validation_to_json(v));
                    }
                }
            }
        }
        JsonValue::Object(out)
    }

    fn exec_validate(&self, name: &str) -> napi::Result<JsonValue> {
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
            .ok_or_else(|| err(format!("graph type '{name}' not found")))?;
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
            .map_err(|e| err(format!("save catalog: {e}")))?;

        let mut samples: Vec<JsonValue> = Vec::new();
        for v in &report.samples {
            let mut s = Map::new();
            s.insert(
                "kind".into(),
                JsonValue::String(
                    match v.kind {
                        ElementKind::Node => "node",
                        ElementKind::Edge => "edge",
                    }
                    .into(),
                ),
            );
            s.insert("id".into(), json!(v.id));
            s.insert(
                "labels".into(),
                JsonValue::Array(
                    v.labels
                        .iter()
                        .map(|l| JsonValue::String(l.clone()))
                        .collect(),
                ),
            );
            let mut props = Map::new();
            for (k, t) in &v.props {
                props.insert(k.clone(), JsonValue::String(format!("{t}")));
            }
            s.insert("props".into(), JsonValue::Object(props));
            samples.push(JsonValue::Object(s));
        }

        Ok(json!({
            "ok": report.ok(),
            "name": key,
            "nodesChecked": report.nodes_checked,
            "edgesChecked": report.edges_checked,
            "nodeViolations": report.node_violations,
            "edgeViolations": report.edge_violations,
            "samples": samples,
        }))
    }

    fn exec_create_index(
        &self,
        name: Option<String>,
        label: &str,
        prop: &str,
        kind: gqlrust::syntax::statement::IndexKindStmt,
    ) -> napi::Result<JsonValue> {
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
            .map_err(err)?;
        Ok(json!({
            "ok": true,
            "kind": "index",
            "name": spec.name.as_str(),
            "label": spec.label.as_str(),
            "prop": spec.prop.as_str(),
            "indexKind": match spec.kind { IndexKind::Hash => "HASH", IndexKind::BTree => "BTREE" },
            "entries": spec.entries,
        }))
    }

    fn exec_drop_index(&self, name: &str) -> JsonValue {
        let dropped = self.store.secondary_indexes_mut().drop_named(name);
        let mut out = Map::new();
        out.insert("ok".into(), JsonValue::Bool(dropped));
        out.insert("kind".into(), JsonValue::String("index".into()));
        out.insert("name".into(), JsonValue::String(name.into()));
        if !dropped {
            out.insert("error".into(), JsonValue::String("index not found".into()));
        }
        JsonValue::Object(out)
    }

    fn exec_show_indexes(&self) -> JsonValue {
        use gqlrust::store::secondary_index::IndexKind;
        let idx = self.store.secondary_indexes();
        let mut out: Vec<JsonValue> = Vec::new();
        for spec in idx.list() {
            out.push(json!({
                "name": spec.name.as_str(),
                "label": spec.label.as_str(),
                "prop": spec.prop.as_str(),
                "kind": match spec.kind { IndexKind::Hash => "HASH", IndexKind::BTree => "BTREE" },
                "auto": spec.auto,
                "entries": spec.entries,
            }));
        }
        JsonValue::Array(out)
    }
}

fn validation_to_json(v: &ValidationStatus) -> JsonValue {
    json!({
        "nodesChecked": v.against_node_count,
        "edgesChecked": v.against_edge_count,
        "violations": v.violations,
        "validatedAtUnix": v.validated_at_unix,
    })
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

fn ddl_response(message: &str) -> JsonValue {
    json!({
        "ok": true,
        "kind": "ddl",
        "message": message,
    })
}

fn value_to_json(store: &LazyGraphStore, v: &Value) -> JsonValue {
    match v {
        Value::Null => JsonValue::Null,
        Value::Int(n) => json!(*n),
        Value::Float(x) => json!(*x),
        Value::Str(s) => JsonValue::String(s.clone()),
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::List(items) => {
            JsonValue::Array(items.iter().map(|v| value_to_json(store, v)).collect())
        }
        Value::Record(fields) => {
            let mut obj = Map::new();
            for (k, v) in fields {
                obj.insert(k.clone(), value_to_json(store, v));
            }
            JsonValue::Object(obj)
        }
        Value::Node(id) => node_to_json(store, *id),
        Value::Edge(id) => edge_to_json(store, *id),
        // A named path projects to a JSON array of its element objects
        // (nodes and edges in match order).
        Value::Path(items) => {
            JsonValue::Array(items.iter().map(|v| value_to_json(store, v)).collect())
        }
    }
}

fn node_to_json(store: &LazyGraphStore, id: u32) -> JsonValue {
    let labels: Vec<JsonValue> = store
        .node_labels(id)
        .required_labels()
        .into_iter()
        .map(|s| JsonValue::String(s.to_string()))
        .collect();
    let mut props = Map::new();
    for (k, vv) in store.node_props(id).iter() {
        props.insert(k.clone(), value_to_json(store, vv));
    }
    json!({
        "kind": "node",
        "id": id,
        "labels": labels,
        "props": JsonValue::Object(props),
    })
}

fn edge_to_json(store: &LazyGraphStore, id: u32) -> JsonValue {
    let labels: Vec<JsonValue> = store
        .edge_labels(id)
        .required_labels()
        .into_iter()
        .map(|s| JsonValue::String(s.to_string()))
        .collect();
    let mut props = Map::new();
    for (k, vv) in store.edge_props(id).iter() {
        props.insert(k.clone(), value_to_json(store, vv));
    }
    json!({
        "kind": "edge",
        "id": id,
        "labels": labels,
        "props": JsonValue::Object(props),
    })
}

fn raw_to_json(store: &LazyGraphStore, ir: &IntermediateResult) -> JsonValue {
    let mut out: Vec<JsonValue> = Vec::with_capacity(ir.rows.len());
    for row in &ir.rows {
        let mut obj = Map::new();
        let mut paths: Vec<JsonValue> = Vec::new();
        for path in &row.paths {
            let p: Vec<JsonValue> = path
                .0
                .iter()
                .map(|pv| pathvalue_to_json(store, pv))
                .collect();
            paths.push(JsonValue::Array(p));
        }
        obj.insert("_paths".into(), JsonValue::Array(paths));
        for (var, pv) in row.assignment.m.iter() {
            obj.insert(var.clone(), pathvalue_to_json(store, pv));
        }
        out.push(JsonValue::Object(obj));
    }
    JsonValue::Array(out)
}

fn pathvalue_to_json(store: &LazyGraphStore, pv: &PathValue) -> JsonValue {
    match pv {
        PathValue::Node(id) => node_to_json(store, *id),
        PathValue::EdgeDirectional(id) | PathValue::EdgeUndirectional(id) => {
            edge_to_json(store, *id)
        }
        PathValue::Nothing => JsonValue::Null,
        PathValue::Group(items) | PathValue::Path(items) => JsonValue::Array(
            items
                .iter()
                .map(|it| pathvalue_to_json(store, it))
                .collect(),
        ),
    }
}

/// Open or create a `.gdb` database. Eagerly warms the LTJ TripleIndex
/// so the first `execute()` on the returned Connection runs at warm
/// cache speed.
#[napi]
pub fn open(path: String) -> napi::Result<Connection> {
    let store =
        LazyGraphStore::open(Path::new(&path)).map_err(|e| err(format!("open failed: {e}")))?;
    let index = {
        let scratch = Runtime::new(&store);
        scratch.warm_triple_index()
    };
    Ok(Connection {
        store,
        db_path: Path::new(&path).to_path_buf(),
        triple_index: RefCell::new(Some(index)),
    })
}

/// Import a JSON graph (`{nodes: [...], edges: [...]}`) into a fresh
/// `.gdb` at `dbPath`. Overwrites the destination.
#[napi]
pub fn import_json(db_path: String, json_path: String) -> napi::Result<()> {
    let g = MemoryGraphStore::from_file(Path::new(&json_path))
        .map_err(|e| err(format!("load json: {e}")))?;
    g.save(Path::new(&db_path))
        .map_err(|e| err(format!("save: {e}")))?;
    Ok(())
}

/// Import a directory of CSVs (configured via `spanner_import_config.json`)
/// into a fresh `.gdb` at `dbPath`. Overwrites the destination.
#[napi]
pub fn import_csv(db_path: String, csv_dir: String) -> napi::Result<()> {
    let g = csv_loader::load_from_csv_dir(Path::new(&csv_dir))
        .map_err(|e| err(format!("load csv: {e}")))?;
    g.save(Path::new(&db_path))
        .map_err(|e| err(format!("save: {e}")))?;
    Ok(())
}
