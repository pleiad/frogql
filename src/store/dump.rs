//! Serialise a `GraphAccess` view to a `pg_dump`-style JSON document.
//!
//! Mirrors the on-disk format consumed by `MemoryGraphStore::from_json_value`:
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

use crate::model::graph::{MemoryGraphStore, Props};
use crate::model::graph_access::GraphAccess;
use crate::model::value::Value;

/// Serialise the full graph to a JSON value. Produced shape matches what
/// `MemoryGraphStore::from_json_value` expects (the import path).
pub fn dump_to_json_value<G: GraphAccess>(g: &G) -> Json {
    let mut nodes: Vec<Json> = Vec::new();
    for nid in g.nodes() {
        let mut entry = Map::new();
        entry.insert("id".to_string(), Json::String(g.node_name(nid).to_string()));
        entry.insert(
            "labels".to_string(),
            Json::Array(
                MemoryGraphStore::label_strings(&g.node_labels(nid))
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
                MemoryGraphStore::label_strings(&g.edge_labels(eid))
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
        // Reference values and paths are runtime-only — `node_props` /
        // `edge_props` never produce them in the import/export pipeline.
        // Surface as `null` to keep the dump valid JSON.
        Value::Node(_) | Value::Edge(_) | Value::Path(_) => Json::Null,
    }
}

// ---------------------------------------------------------------------
// MVP-1.F: pg_dump-style GQL dump
// ---------------------------------------------------------------------

/// Build a GQL script that, when executed against an empty database,
/// reproduces `g`'s shape (nodes + edges + label sets + property maps).
/// Mirrors the strategy `pg_dump` uses for foreign keys: every node
/// receives a synthetic `_dump_id` property keyed by its internal id;
/// edges are produced by `MATCH ... INSERT ...` pairs that look those
/// `_dump_id`s up; a final `REMOVE` strips the synthetic key.
///
/// The synthetic property name is `_dump_id` by default; if any live
/// element already carries a property with that name, the dump falls
/// back to `__dump_id_v1`, `__dump_id_v2`, ... until it finds an unused
/// slot. Returns `Err` only on inputs the lexer cannot round-trip, e.g.
/// a string literal containing a single quote.
pub fn dump_to_gql_string<G: GraphAccess>(g: &G) -> Result<String, String> {
    let dump_id_prop = pick_dump_id_prop(g);
    let mut out = String::new();
    out.push_str("-- froGQL dump (.dump-gql)\n");
    out.push_str("-- nodes ----\n");
    for nid in g.nodes() {
        out.push_str(&format_node_insert(g, nid, &dump_id_prop)?);
        out.push('\n');
    }
    out.push_str("-- edges ----\n");
    for eid in g.edges_directed().into_iter().chain(g.edges_undirected()) {
        out.push_str(&format_edge_insert(g, eid, &dump_id_prop)?);
        out.push('\n');
    }
    out.push_str("-- cleanup --\n");
    out.push_str(&format!("MATCH (n) REMOVE n.{dump_id_prop};\n"));
    Ok(out)
}

/// Convenience wrapper that writes the dump to `path`.
pub fn dump_to_gql_file<G: GraphAccess>(g: &G, path: &Path) -> io::Result<()> {
    let script = dump_to_gql_string(g).map_err(io::Error::other)?;
    let mut f = std::fs::File::create(path)?;
    f.write_all(script.as_bytes())
}

/// Pick a property name for the temporary `_dump_id` key that does not
/// collide with anything the graph already stores. Linear scan over
/// every live element's property map; bounded by total prop count.
fn pick_dump_id_prop<G: GraphAccess>(g: &G) -> String {
    let candidates: Vec<String> = std::iter::once("_dump_id".to_string())
        .chain((1..).map(|i| format!("__dump_id_v{i}")))
        .take(64)
        .collect();
    let prop_used = |name: &str| -> bool {
        for nid in g.nodes() {
            if g.node_props(nid).contains_key(name) {
                return true;
            }
        }
        for eid in g.edges_directed().into_iter().chain(g.edges_undirected()) {
            if g.edge_props(eid).contains_key(name) {
                return true;
            }
        }
        false
    };
    for name in &candidates {
        if !prop_used(name) {
            return name.clone();
        }
    }
    "__frogql_dump_id__".to_string()
}

fn format_node_insert<G: GraphAccess>(
    g: &G,
    nid: u32,
    dump_id_prop: &str,
) -> Result<String, String> {
    let labels = MemoryGraphStore::label_strings(&g.node_labels(nid));
    let mut props = g.node_props(nid);
    props.insert(dump_id_prop.to_string(), Value::Str(format!("n_{nid}")));
    let descriptor = format_descriptor(&labels, &props)?;
    Ok(format!("INSERT ({descriptor});"))
}

fn format_edge_insert<G: GraphAccess>(
    g: &G,
    eid: u32,
    dump_id_prop: &str,
) -> Result<String, String> {
    let src = g.src(eid);
    let tgt = g.tgt(eid);
    let directed = g.is_directed(eid);
    let labels = MemoryGraphStore::label_strings(&g.edge_labels(eid));
    let props = g.edge_props(eid);
    let edge_descriptor = format_descriptor(&labels, &props)?;
    let arrow = if directed {
        format!("-[{edge_descriptor}]->")
    } else {
        format!("~[{edge_descriptor}]~")
    };
    // The parser does not accept `(a {prop: val})` (variable + property
    // dictionary without a label or `IS`); the `:*` wildcard label
    // satisfies the `<label and property set specification>` shape and
    // matches every node, so we can pin the row by `_dump_id` while
    // keeping the lookup label-agnostic.
    Ok(format!(
        "MATCH (a:* {{{dump_id_prop}: 'n_{src}'}}), (b:* {{{dump_id_prop}: 'n_{tgt}'}}) \
         INSERT (a){arrow}(b);"
    ))
}

/// Render `:L1 & L2 ... { k1: v1, k2: v2, ... }`. Either side may be
/// empty; an element with neither labels nor props collapses to "". The
/// parser models multi-label as a `LabelType::And` chain joined by `&`,
/// so the render uses `:L1 & L2` rather than `:L1:L2`.
fn format_descriptor(labels: &[String], props: &Props) -> Result<String, String> {
    let mut out = String::new();
    if !labels.is_empty() {
        out.push(':');
        out.push_str(&labels.join(" & "));
    }
    if !props.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push('{');
        let mut keys: Vec<&String> = props.keys().collect();
        keys.sort();
        let mut first = true;
        for k in keys {
            if let Some(v) = props.get(k) {
                let formatted = format_gql_value(v)?;
                if formatted.is_empty() {
                    continue;
                }
                if !first {
                    out.push_str(", ");
                }
                first = false;
                out.push_str(k);
                out.push_str(": ");
                out.push_str(&formatted);
            }
        }
        out.push('}');
    }
    Ok(out)
}

fn format_gql_value(v: &Value) -> Result<String, String> {
    Ok(match v {
        Value::Null => "NULL".to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(x) => {
            if x.is_finite() && x.fract() == 0.0 {
                format!("{x:.1}")
            } else {
                format!("{x}")
            }
        }
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Str(s) => {
            if s.contains('\'') {
                return Err(format!(
                    "dump-gql: string value contains a single quote, which the lexer \
                     cannot round-trip (value: {s:?})"
                ));
            }
            format!("'{s}'")
        }
        Value::List(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(format_gql_value)
                .collect::<Result<_, _>>()?;
            format!("[{}]", parts.join(", "))
        }
        Value::Record(fields) => {
            let mut parts: Vec<String> = Vec::new();
            for (k, v) in fields {
                let fv = format_gql_value(v)?;
                parts.push(format!("{k}: {fv}"));
            }
            format!("{{{}}}", parts.join(", "))
        }
        Value::Node(_) | Value::Edge(_) | Value::Path(_) => String::new(),
    })
}
