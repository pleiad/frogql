//! froGQL compiled to WebAssembly: an in-browser, in-RAM graph engine.
//!
//! Wraps either in-RAM backend plus the shared
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

use frogql_core::model::graph::MemoryGraphStore;
use frogql_core::model::graph_access::GraphAccess;
use frogql_core::model::value::{Id, PathValue, Value};
use frogql_core::parser::parse_statement;
use frogql_core::runtime::engine::Runtime;
use frogql_core::runtime::ltj::triple_index::TripleIndex;
use frogql_core::runtime::result::{IntermediateResult, QueryResult};
use frogql_core::store::lazy::LazyGraphStore;
use frogql_core::syntax::expr::Expr;
use frogql_core::syntax::query::ReturnItem;
use frogql_core::syntax::statement::Statement;
use frogql_core::typing::inference::infer_simple_schema;
use frogql_core::typing::variable_type::Schema;

/// Which backend a `Connection` is reading.
///
/// `Json` is a graph parsed from the JSON document shape; `Bytes` is a
/// real `.gdb` image fetched over HTTP and paged out of RAM. They differ
/// in how the data arrived, not in what the engine does with it — both
/// implement `GraphAccess`, both take DML through the same overlay.
///
/// Why bytes at all: a JSON document has to be parsed and every node and
/// edge rebuilt, and it carries no index. A `.gdb` is already in the
/// engine's own layout, and its `.ltj` sidecar carries the six LTJ trie
/// orderings, which cost `O(E log E)` to rebuild — 252 s measured on a
/// 617 M-edge graph. Not paying that in a browser tab is the point.
enum Backend {
    Json(Box<MemoryGraphStore>),
    Bytes(Box<LazyGraphStore>),
}

/// Run `$body` against whichever backend is live.
///
/// Every use site is generic over `GraphAccess` and identical in both
/// arms; the macro exists so adding a method does not mean writing the
/// two-arm match again and risking the arms drifting apart.
macro_rules! with_store {
    ($conn:expr, $s:ident => $body:expr) => {
        match &$conn.store {
            // Both arms are boxed — `MemoryGraphStore` alone is ~1 KB
            // inline — so the enum stays a pointer and each arm binds
            // through its box, leaving `$s` a plain `&Store` either way.
            Backend::Json(boxed) => {
                let $s = &**boxed;
                $body
            }
            Backend::Bytes(boxed) => {
                let $s = &**boxed;
                $body
            }
        }
    };
}

/// A live graph plus the caches that keep query latency flat.
#[wasm_bindgen]
pub struct Connection {
    store: Backend,
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
        store: Backend::Json(Box::new(store)),
        triple_index: RefCell::new(None),
        schema: RefCell::new(None),
    };
    // Warm the index once at open, matching the Python/Node bindings.
    let _ = conn.triple_index_arc();
    Ok(conn)
}

/// Open a `.gdb` image fetched over the network, with its `.ltj` sidecar
/// when the caller has it.
///
/// ```js
/// const [gdb, ltj] = await Promise.all([
///   fetch("/santiago.gdb").then(r => r.arrayBuffer()),
///   fetch("/santiago.gdb.ltj").then(r => r.arrayBuffer()),
/// ]);
/// const conn = open_bytes(new Uint8Array(gdb), new Uint8Array(ltj));
/// ```
///
/// `ltj` is optional and is the reason to prefer this over `open_json`
/// for anything large: without it the six LTJ trie orderings are rebuilt
/// at open, which is `O(E log E)`. A sidecar that does not describe this
/// database is **refused, not trusted** — the same `(graph_id,
/// node_count, edge_count)` fingerprint a file-backed open checks — and
/// the index is rebuilt instead, so a mismatched pair costs time and
/// never correctness.
///
/// The connection is read-mostly: DML works, through the same overlay as
/// every other backend, but there is nowhere to write pages back to, so
/// the durable copy is whatever the server serves. `to_json()` still
/// gives a snapshot of the merged view.
#[wasm_bindgen]
pub fn open_bytes(gdb: Vec<u8>, ltj: Option<Vec<u8>>) -> Result<Connection, JsError> {
    console_error_panic_hook::set_once();
    let store = LazyGraphStore::from_bytes(gdb, ltj).map_err(|e| JsError::new(&format!("{e}")))?;
    let conn = Connection {
        store: Backend::Bytes(Box::new(store)),
        triple_index: RefCell::new(None),
        schema: RefCell::new(None),
    };
    let _ = conn.triple_index_arc();
    Ok(conn)
}

#[wasm_bindgen]
impl Connection {
    #[wasm_bindgen(getter)]
    pub fn node_count(&self) -> u32 {
        // The merged view, so this agrees with `COUNT(n)` on the same
        // connection after an INSERT. See `live_node_count`.
        with_store!(self, s => s.live_node_count()) as u32
    }

    #[wasm_bindgen(getter)]
    pub fn edge_count(&self) -> u32 {
        with_store!(self, s => s.live_edge_count()) as u32
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
        match &self.store {
            Backend::Json(s) => s.to_json_string(),
            // A paged database has no JSON writer; materialising the
            // merged view into an in-RAM graph is what `.save` and the
            // dump utilities already do, and it compacts ids on the way.
            Backend::Bytes(s) => s.materialize_to_graph().to_json_string(),
        }
    }

    /// `{ node_labels, edge_labels, node_count, edge_count }`, mirroring
    /// the Python/Node `schema()` summary.
    /// What the typechecker knows about a query, without running it.
    ///
    /// This is the half of froGQL a REPL shows and a bare "0 rows" hides.
    /// `(n:Calle)-[e]->(m)` against a schema where every `EN_CALLE` points
    /// *into* `Calle` is not an empty answer, it is a **provably** empty
    /// one — the checker settles it before the runtime is asked, and
    /// saying "0 filas" instead sends the reader looking for missing data
    /// that was never missing.
    ///
    /// ```json
    /// { "ok": true, "empty": true, "errors": [], "warnings": [...],
    ///   "vars": [{ "name": "n", "type": "(:Calle {...})" }] }
    /// ```
    ///
    /// The pipeline is run here rather than through
    /// `compile_query_with_diagnostics_with` because that returns the
    /// compiled query and drops the `TypeEnvironment` — and the
    /// environment is the interesting part: it is what the checker
    /// *inferred*, per variable, which no amount of reading the query
    /// back tells you.
    ///
    /// Cheap enough to run on every keystroke: parse, elaborate and check,
    /// with no optimizer pass and no graph access at all.
    pub fn check(&self, query: &str) -> Result<JsValue, JsError> {
        use frogql_core::typing::checker::Typechecker;

        let ast = match frogql_core::parser::parse_query(query) {
            Ok(a) => a,
            Err(e) => {
                return to_js(&json!({
                    "ok": false, "empty": false, "kind": "parse",
                    "errors": [e], "warnings": [], "vars": [],
                }))
            }
        };
        let q = frogql_core::elaborate::elaborate_query(ast);
        let mut tc = Typechecker::new(self.active_schema());
        let r = tc.check_query(&q);

        let mut vars: Vec<Json> = r
            .env
            .keys()
            .into_iter()
            .map(|k| {
                json!({
                    "name": k,
                    "type": r.env.get(k).map(|t| format!("{t}")).unwrap_or_default(),
                })
            })
            .collect();
        // Stable order: a panel that reshuffles on every keystroke is
        // harder to read than one that grows.
        vars.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

        to_js(&json!({
            "ok": r.ok,
            "empty": r.empty,
            "kind": if r.ok { "ok" } else { "type" },
            "errors": tc.errors,
            "warnings": tc.warnings,
            "vars": vars,
        }))
    }

    /// The active GRAPH TYPE, rendered the way `SHOW GRAPH TYPE DEFAULT`
    /// renders it in the REPL: one line per node type and one per edge
    /// type, with the properties each carries.
    ///
    /// `schema()` answers "which labels exist", which is what a sidebar
    /// needs and all it needs. This answers "what is in them" — the
    /// question anyone writing a query against an unfamiliar database
    /// actually has, and the one that was unanswerable in the browser
    /// because the formatter was never exposed.
    ///
    /// On a `.gdb` this reads the catalog; on a JSON graph it is inferred
    /// from the data, which is the same split `active_schema` makes.
    pub fn graph_type(&self) -> String {
        frogql_core::typing::format::format_schema(&self.active_schema())
    }

    /// The active GRAPH TYPE as data, so it can be **drawn**.
    ///
    /// `graph_type()` renders the same thing as text, which is what the
    /// REPL shows and what a reader skims. A schema is a graph, though —
    /// node types joined by edge types — and the shape of it is the part
    /// a text listing makes you reconstruct in your head. This returns
    /// the pieces a diagram needs and lets the page draw them.
    ///
    /// ```json
    /// { "nodes": [{ "name": "fpl", "labels": ["Fpl"],
    ///               "props": [{ "key": "fplId", "type": "STRING" }] }],
    ///   "edges": [{ "label": "SALE_DE", "from": "fpl", "to": "aerodromo",
    ///               "directed": true, "props": [] }] }
    /// ```
    ///
    /// Endpoints are the *names* `typing::format::NodeTypeNames` derives,
    /// so the diagram and the text call a type the same thing. An edge
    /// whose endpoint matches no declared node type gets `null` there
    /// rather than a name that would misdescribe it — the same care
    /// `format_schema` takes when it declines to borrow a name.
    pub fn graph_type_json(&self) -> Result<JsValue, JsError> {
        use frogql_core::typing::descriptor_type::DescriptorType;
        use frogql_core::typing::format::NodeTypeNames;
        use frogql_core::typing::property_type::PropertyType;
        use frogql_core::typing::variable_type::VariableType;

        let schema = self.active_schema();
        let names = NodeTypeNames::of(&schema);

        let props_of = |p: &PropertyType| -> Json {
            let map = match p {
                PropertyType::Open(m) | PropertyType::Closed(m) => m,
                PropertyType::Zero => return Json::Array(vec![]),
            };
            Json::Array(
                map.iter()
                    .map(|(k, t)| json!({ "key": k, "type": format!("{t}") }))
                    .collect(),
            )
        };
        let name_of = |d: &DescriptorType| -> Json {
            match names.get(d) {
                Some(n) => Json::String(n.to_string()),
                None => Json::Null,
            }
        };

        let mut nodes: Vec<Json> = Vec::new();
        for vt in schema.nodes.iter() {
            let VariableType::Node(d) = vt else { continue };
            nodes.push(json!({
                "name": name_of(d),
                "labels": d.label.required_labels().iter().map(|l| l.to_string())
                           .collect::<Vec<_>>(),
                "props": props_of(&d.props),
            }));
        }

        let mut edges: Vec<Json> = Vec::new();
        for vt in schema.edges.iter() {
            let (desc, left, right, directed) = match vt {
                VariableType::EdgeDirectional { desc, left, right } => (desc, left, right, true),
                VariableType::EdgeNonDirectional { desc, left, right } => {
                    (desc, left, right, false)
                }
                _ => continue,
            };
            let endpoint = |v: &VariableType| match v {
                VariableType::Node(d) => name_of(d),
                _ => Json::Null,
            };
            edges.push(json!({
                "label": desc.label.required_labels().first().map(|l| l.to_string()),
                "from": endpoint(left),
                "to": endpoint(right),
                "directed": directed,
                "props": props_of(&desc.props),
            }));
        }
        to_js(&json!({ "nodes": nodes, "edges": edges }))
    }

    // `node_count` returns `usize` on one backend and `u32` on the other,
    // and `with_store!` expands both arms, so whichever cast is needed for
    // one is redundant for the other. Narrowing before the match would
    // just move the same problem.
    #[allow(clippy::unnecessary_cast)]
    pub fn schema(&self) -> Result<JsValue, JsError> {
        use std::collections::BTreeSet;
        let mut node_labels: BTreeSet<String> = BTreeSet::new();
        with_store!(self, s => {
            for nid in 0..s.node_count() as u32 {
                for l in s.node_labels(nid).required_labels() {
                    node_labels.insert(l.to_string());
                }
            }
        });
        let mut edge_labels: BTreeSet<String> = BTreeSet::new();
        with_store!(self, s => {
            for eid in 0..s.edge_count() as u32 {
                for l in s.edge_labels(eid).required_labels() {
                    edge_labels.insert(l.to_string());
                }
            }
        });
        let v = json!({
            "node_labels": node_labels.into_iter().collect::<Vec<_>>(),
            "edge_labels": edge_labels.into_iter().collect::<Vec<_>>(),
            "node_count": with_store!(self, s => s.node_count() as u64),
            "edge_count": with_store!(self, s => s.edge_count() as u64),
        });
        to_js(&v)
    }
}

// Internal helpers (no #[wasm_bindgen]).
impl Connection {
    /// Build (once) and return the shared TripleIndex Arc.
    fn triple_index_arc(&self) -> Arc<TripleIndex> {
        if self.triple_index.borrow().is_none() {
            let idx = with_store!(self, s => Runtime::new(s).warm_triple_index());
            *self.triple_index.borrow_mut() = Some(idx);
        }
        self.triple_index
            .borrow()
            .clone()
            .expect("triple index just built")
    }

    /// The inferred DEFAULT schema, cached until the next mutation.
    fn active_schema(&self) -> Schema {
        if self.schema.borrow().is_none() {
            let sch = match &self.store {
                // The JSON backend has no catalog, so the schema is
                // derived from the data every time it is invalidated.
                Backend::Json(s) => infer_simple_schema(&**s),
                // A `.gdb` carries its own: `active_schema` reads the
                // catalog and only re-infers when DML marked DEFAULT
                // dirty. Inferring unconditionally instead walks every
                // node and edge decoding properties — on a 459 127-node /
                // 2 244 154-edge graph that is **8.4 s of every first
                // query** in wasm, against 0.000 s for the same query
                // natively, where the REPL reads the catalog. The
                // difference is not wasm being slow; it is O(N + E) work
                // the file had already done.
                Backend::Bytes(s) => s.active_schema(),
            };
            *self.schema.borrow_mut() = Some(sch);
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

    fn exec_dm(&self, dm: frogql_core::syntax::dm::DmStatement) -> Result<JsValue, JsError> {
        let v = self.dm_json(dm).map_err(|e| JsError::new(&e))?;
        to_js(&v)
    }

    /// Engine core for read queries, returning a `serde_json` array of row
    /// objects. Split out from `exec_query` so it is testable on the host
    /// target (the `JsValue` marshaling needs a JS runtime; this does not).
    fn query_json(&self, query: &str, limit: usize) -> Result<Json, String> {
        with_store!(self, s => self.query_json_on(s, query, limit))
    }

    /// The same body against whichever backend is live. Generic rather
    /// than duplicated, so the two arms cannot drift.
    fn query_json_on<G: GraphAccess>(
        &self,
        store: &G,
        query: &str,
        limit: usize,
    ) -> Result<Json, String> {
        let schema = self.active_schema();
        let compiled = frogql_core::compile_query_with_diagnostics_with(&schema, query)
            .map_err(|e| e.message())?;
        let q = compiled.query;

        if compiled.guaranteed_empty {
            return Ok(Json::Array(vec![]));
        }

        let rt = Runtime::with_triple_index(store, self.triple_index_arc());
        let rows = match rt.run_query(&q, limit) {
            QueryResult::Projected(rows) => {
                let headers = projection_headers(&q);
                let mut out: Vec<Json> = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut obj = Map::new();
                    for (i, v) in row.into_iter().enumerate() {
                        let key = headers.get(i).cloned().unwrap_or_else(|| format!("col{i}"));
                        obj.insert(key, value_to_json(store, &v));
                    }
                    out.push(Json::Object(obj));
                }
                Json::Array(out)
            }
            QueryResult::Raw(ir) => raw_to_json(store, &ir),
        };
        Ok(rows)
    }

    /// Engine core for data-modifying statements; returns the counters
    /// object. Host-testable, same split rationale as `query_json`.
    fn dm_json(&self, dm: frogql_core::syntax::dm::DmStatement) -> Result<Json, String> {
        // No catalog in the in-memory backend, so DEFAULT semantics: no
        // G2000 validation schema.
        let exec = with_store!(self, s => frogql_core::runtime::dm::run_dm(s, &dm, None))?;
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
fn projection_headers(q: &frogql_core::syntax::query::Query) -> Vec<String> {
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

fn value_to_json<G: GraphAccess>(store: &G, v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Int(n) => json!(n),
        Value::Float(x) => serde_json::Number::from_f64(*x)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Value::Str(s) => json!(s),
        Value::Date(d) => json!(frogql_core::model::value::format_date(*d)),
        Value::LocalDatetime(ms) => json!(frogql_core::model::value::format_datetime(*ms)),
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

fn node_ref_json<G: GraphAccess>(store: &G, id: Id) -> Json {
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

fn edge_ref_json<G: GraphAccess>(store: &G, id: Id) -> Json {
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
    // Endpoints and directedness travel with the edge because the caller
    // cannot recover them. A path lists elements in *traversal* order, so
    // a reverse pattern (`(a)<-[e]-(b)`) yields the same sequence as the
    // forward one and the arrow would point the wrong way half the time;
    // and `PathValue::EdgeDirectional` / `EdgeUndirectional` collapse
    // into one `kind` here, so an undirected edge would be drawn with an
    // arrowhead it does not have.
    json!({
        "kind": "edge", "id": id, "labels": labels,
        "src": store.src(id), "tgt": store.tgt(id),
        "directed": store.is_directed(id),
        "props": Json::Object(props),
    })
}

fn pathvalue_to_json<G: GraphAccess>(store: &G, pv: &PathValue) -> Json {
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

fn raw_to_json<G: GraphAccess>(store: &G, ir: &IntermediateResult) -> Json {
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
