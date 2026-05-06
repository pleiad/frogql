//! Serialise a `GraphAccess` view to a `pg_dump`-style JSON document.
//!
//! Mirrors the on-disk format consumed by `Graph::from_json_value`:
//! ```json
//! { "nodes": [{ "id": "...", "labels": [...], "props": {...} }, ...],
//!   "edges": [{ "id": "...", "labels": [...], "endpoints": ["...", "..."],
//!              "directionality": "->" | "~~", "props": {...} }, ...] }
//! ```
//!
//! The dump walks the live (overlay-aware) view. Tombstoned elements are
//! skipped; freshly inserted elements appear with their synthetic
//! `auto-n-<id>` / `auto-e-<id>` names so the reload is deterministic.
//!
//! Round-trip property: `import_json(dump_json(g))` reproduces `g`'s
//! shape (modulo node id reordering — JSON ids are user-facing strings,
//! not internal `u32`s).

use std::io::{self, Write};
use std::path::Path;

use serde_json::{json, Map, Value as Json};

use crate::model::graph::{Graph, Props};
use crate::model::graph_access::GraphAccess;
use crate::model::value::Value;
use crate::typing::label_type::LabelType;

/// Serialise the full graph to a JSON value. Produced shape matches what
/// `Graph::from_json_value` expects (the import path).
pub fn dump_to_json_value<G: GraphAccess>(g: &G) -> Json {
    let mut nodes: Vec<Json> = Vec::new();
    for nid in g.nodes() {
        let mut entry = Map::new();
        entry.insert("id".to_string(), Json::String(g.node_name(nid).to_string()));
        entry.insert(
            "labels".to_string(),
            Json::Array(
                Graph::label_strings(&g.node_labels(nid))
                    .into_iter()
                    .map(Json::String)
                    .collect(),
            ),
        );
        entry.insert("props".to_string(), props_to_json(&g.node_props(nid)));
        nodes.push(Json::Object(entry));
    }

    let mut edges: Vec<Json> = Vec::new();
    for eid in g.edges_directed().into_iter().chain(g.edges_undirected()) {
        let mut entry = Map::new();
        entry.insert("id".to_string(), Json::String(g.edge_name(eid).to_string()));
        entry.insert(
            "labels".to_string(),
            Json::Array(
                Graph::label_strings(&g.edge_labels(eid))
                    .into_iter()
                    .map(Json::String)
                    .collect(),
            ),
        );
        entry.insert(
            "endpoints".to_string(),
            json!([g.node_name(g.src(eid)), g.node_name(g.tgt(eid))]),
        );
        entry.insert(
            "directionality".to_string(),
            Json::String(if g.is_directed(eid) { "->" } else { "~~" }.to_string()),
        );
        entry.insert("props".to_string(), props_to_json(&g.edge_props(eid)));
        edges.push(Json::Object(entry));
    }

    json!({ "nodes": nodes, "edges": edges })
}

/// Serialise the graph to a JSON file at `path`. Pretty-printed (indent 2)
/// to keep human-readable diffs sane in research workflows.
pub fn dump_to_json_file<G: GraphAccess>(g: &G, path: &Path) -> io::Result<()> {
    let v = dump_to_json_value(g);
    let serialized = serde_json::to_string_pretty(&v).map_err(io::Error::other)?;
    let mut f = std::fs::File::create(path)?;
    f.write_all(serialized.as_bytes())?;
    Ok(())
}

fn props_to_json(props: &Props) -> Json {
    let mut m = Map::new();
    for (k, v) in props {
        m.insert(k.clone(), value_to_json(v));
    }
    Json::Object(m)
}

fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Int(n) => Json::Number((*n).into()),
        Value::Float(x) => serde_json::Number::from_f64(*x)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Value::Bool(b) => Json::Bool(*b),
        Value::Str(s) => Json::String(s.clone()),
        Value::List(items) => Json::Array(items.iter().map(value_to_json).collect()),
        Value::Record(fields) => {
            let mut m = Map::new();
            for (k, v) in fields {
                m.insert(k.clone(), value_to_json(v));
            }
            Json::Object(m)
        }
        // Reference values are runtime-only — `node_props` / `edge_props`
        // never produce them in the import/export pipeline. Surface as
        // `null` to keep the dump valid JSON.
        Value::Node(_) | Value::Edge(_) => Json::Null,
    }
}

// MVP-1 will add `dump_to_gql` (the pg_dump-style INSERT script). It
// requires MATCH+INSERT bindings and REMOVE for the temporary `_dump_id`
// cleanup; both ship in MVP-1. Tracking note kept here so the plan
// stays visible in code.
#[allow(dead_code)]
fn _gql_dump_lands_in_mvp1(_lt: &LabelType) {}
