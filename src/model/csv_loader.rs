//! Load a property graph from CSV files using a spanner_import_config.json.
//!
//! The config describes node and edge CSV files with their columns and types.
//! Node files have a `vid` column (ID) + property columns.
//! Edge files have `SRC_ID`, `DST_ID`, `vid` columns + optional property columns.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use crate::model::graph::{Graph, Props};
use crate::model::value::{Id, Value};
use crate::typing::label_type::LabelType;

/// Load a graph from a directory containing spanner_import_config.json and CSV files.
pub fn load_from_csv_dir(dir: &Path) -> io::Result<Graph> {
    let config_path = dir.join("spanner_import_config.json");
    let config_str = fs::read_to_string(&config_path)?;
    let config: serde_json::Value = serde_json::from_str(&config_str)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let files = config["files"].as_array()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing 'files' array"))?;

    // First pass: identify node vs edge files
    let mut node_files = Vec::new();
    let mut edge_files = Vec::new();

    for file_config in files {
        let columns = file_config["columns"].as_object()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing 'columns'"))?;
        let is_edge = columns.keys().any(|k| k.eq_ignore_ascii_case("SRC_ID"))
            && columns.keys().any(|k| k.eq_ignore_ascii_case("DST_ID"));
        if is_edge {
            edge_files.push(file_config);
        } else {
            node_files.push(file_config);
        }
    }

    // Load nodes
    let mut node_names = Vec::new();
    let mut node_labels = Vec::new();
    let mut node_props = Vec::new();
    let mut node_name_to_id: HashMap<String, Id> = HashMap::new();

    for file_config in &node_files {
        let csv_name = file_config["path"].as_str().unwrap();
        let label = infer_node_label(csv_name);
        let csv_path = dir.join(csv_name);
        let columns = file_config["columns"].as_object().unwrap();
        let id_col = find_id_column(columns, &label);
        let prop_cols: Vec<(&str, &str)> = columns.iter()
            .filter(|(k, _)| k.as_str() != id_col)
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap_or("STRING")))
            .collect();

        let rows = read_csv(&csv_path)?;
        for row in &rows {
            let vid = get_ci(row, &id_col).unwrap_or_default();
            if vid.is_empty() { continue; }

            if let Some(&existing_id) = node_name_to_id.get(&vid) {
                // Merge label
                let existing: &mut LabelType = &mut node_labels[existing_id as usize];
                *existing = LabelType::And(
                    Box::new(existing.clone()),
                    Box::new(LabelType::Label(label.clone())),
                );
                continue;
            }

            let nid = node_names.len() as Id;
            node_name_to_id.insert(vid.clone(), nid);

            let props = extract_props(row, &prop_cols);
            node_names.push(vid);
            node_labels.push(LabelType::Label(label.clone()));
            node_props.push(props);
        }
    }

    // Load edges
    let mut edge_names = Vec::new();
    let mut edge_labels_vec = Vec::new();
    let mut edge_props_vec = Vec::new();
    let mut edge_src = Vec::new();
    let mut edge_tgt = Vec::new();
    let mut edge_directed = Vec::new();

    // Collect node type names (labels) for stripping from edge filenames
    let node_type_names: Vec<String> = node_files.iter()
        .map(|fc| infer_node_label(fc["path"].as_str().unwrap()))
        .collect();

    for file_config in &edge_files {
        let csv_name = file_config["path"].as_str().unwrap();
        // Use the config's label field if available (strip node types from it),
        // otherwise infer from filename
        let raw_label = file_config["label"].as_str()
            .unwrap_or(Path::new(csv_name).file_stem().unwrap().to_str().unwrap());
        let label = strip_node_types(raw_label, &node_type_names);
        let csv_path = dir.join(csv_name);
        let columns = file_config["columns"].as_object().unwrap();
        let prop_cols: Vec<(&str, &str)> = columns.iter()
            .filter(|(k, _)| {
                let kl = k.to_lowercase();
                kl != "src_id" && kl != "dst_id" && kl != "vid"
            })
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap_or("STRING")))
            .collect();

        let rows = read_csv(&csv_path)?;
        for row in &rows {
            let src_name = get_ci(row, "SRC_ID").unwrap_or_default();
            let dst_name = get_ci(row, "DST_ID").unwrap_or_default();
            let eid_name = get_ci(row, "vid")
                .unwrap_or_else(|| format!("e{}", edge_names.len()));

            let src_id = match node_name_to_id.get(&src_name) {
                Some(&id) => id,
                None => continue, // skip edges with unknown endpoints
            };
            let tgt_id = match node_name_to_id.get(&dst_name) {
                Some(&id) => id,
                None => continue,
            };

            let props = extract_props(row, &prop_cols);
            edge_names.push(eid_name);
            edge_labels_vec.push(LabelType::Label(label.clone()));
            edge_props_vec.push(props);
            edge_src.push(src_id);
            edge_tgt.push(tgt_id);
            edge_directed.push(true); // all edges directed in these datasets
        }
    }

    Ok(Graph::from_raw(
        node_names, node_labels, node_props,
        edge_names, edge_labels_vec, edge_props_vec,
        edge_src, edge_tgt, edge_directed,
    ))
}

/// Case-insensitive HashMap lookup.
fn get_ci(row: &HashMap<String, String>, key: &str) -> Option<String> {
    for (k, v) in row {
        if k.eq_ignore_ascii_case(key) {
            return Some(v.clone());
        }
    }
    None
}

/// Find the ID column in a node file. Tries: "vid", "<label>_id" (case-insensitive), first "_id" column, first column.
fn find_id_column(columns: &serde_json::Map<String, serde_json::Value>, label: &str) -> String {
    if columns.contains_key("vid") { return "vid".to_string(); }
    // Try "<label>_id" case-insensitive
    let label_lower = label.to_lowercase();
    for k in columns.keys() {
        if k.to_lowercase() == format!("{}_id", label_lower) {
            return k.clone();
        }
    }
    // Try any column ending in "_id" (not SRC/DST)
    for k in columns.keys() {
        if k.ends_with("_id") && k != "SRC_ID" && k != "DST_ID" {
            return k.clone();
        }
    }
    // Fallback: first column
    columns.keys().next().cloned().unwrap_or_else(|| "vid".to_string())
}

fn infer_node_label(filename: &str) -> String {
    Path::new(filename).file_stem().unwrap().to_string_lossy().to_string()
}

/// Strip known node type names from a label string.
/// E.g., "PersonACTED_INMovie" with types [Person, Movie] → "ACTED_IN"
/// E.g., "TRANSACTIONINITIATED_BYACCOUNT" with types [TRANSACTION, ACCOUNT] → "INITIATED_BY"
fn strip_node_types(raw: &str, node_types: &[String]) -> String {
    let mut name = raw.to_string();
    // Strip ONE node type from start and ONE from end (longest match first).
    let mut sorted_types: Vec<&String> = node_types.iter().collect();
    sorted_types.sort_by(|a, b| b.len().cmp(&a.len()));
    // Strip prefix (one pass only)
    for t in &sorted_types {
        if name.starts_with(t.as_str()) {
            name = name[t.len()..].trim_start_matches('_').to_string();
            break;
        }
    }
    // Strip suffix (one pass only)
    for t in &sorted_types {
        if name.ends_with(t.as_str()) {
            name = name[..name.len() - t.len()].trim_end_matches('_').to_string();
            break;
        }
    }
    if name.is_empty() { raw.to_string() } else { name }
}

/// Parse a STRING-column value, detecting JSON-encoded lists and records.
/// Falls back to `Value::Str` on any parse failure.
fn parse_string_value(raw: &str) -> Value {
    let trimmed = raw.trim_start();
    let first = trimmed.chars().next();
    if matches!(first, Some('[') | Some('{')) {
        if let Ok(j) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(v) = json_to_value(&j) { return v; }
        }
    }
    Value::Str(raw.to_string())
}

/// Recursive JSON→Value converter used by the CSV loader for list/record strings.
/// Returns None when the JSON contains unsupported constructs (null, non-finite numbers).
fn json_to_value(j: &serde_json::Value) -> Option<Value> {
    match j {
        serde_json::Value::String(s) => Some(Value::Str(s.clone())),
        serde_json::Value::Bool(b) => Some(Value::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() { Some(Value::Int(i)) }
            else if let Some(x) = n.as_f64() { Some(Value::Float(x)) }
            else { None }
        }
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items { out.push(json_to_value(it)?); }
            Some(Value::List(out))
        }
        serde_json::Value::Object(m) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in m { out.insert(k.clone(), json_to_value(v)?); }
            Some(Value::Record(out))
        }
        serde_json::Value::Null => None,
    }
}

fn extract_props(row: &HashMap<String, String>, prop_cols: &[(&str, &str)]) -> Props {
    let mut props = HashMap::new();
    for &(col_name, col_type) in prop_cols {
        if let Some(val) = get_ci(row, col_name) {
            let val = &val;
            if val.is_empty() { continue; }
            let converted = match col_type {
                "INT64" => val.parse::<i64>().ok().map(Value::Int),
                "FLOAT64" | "DOUBLE" | "FLOAT" => val.parse::<f64>().ok().map(Value::Float),
                "BOOL" => Some(Value::Bool(val.eq_ignore_ascii_case("true") || val == "1")),
                // Fallback: STRING or unknown. Many upstream dumps (e.g. Neo4j movies
                // `roles`) encode lists/records as JSON strings in STRING columns.
                // Attempt JSON decode when the value looks like one, so list/record
                // values round-trip to `Value::List` / `Value::Record` rather than `Str`.
                _ => Some(parse_string_value(val)),
            };
            if let Some(v) = converted {
                props.insert(col_name.to_string(), v);
            }
        }
    }
    props
}

fn read_csv(path: &Path) -> io::Result<Vec<HashMap<String, String>>> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let header_line = lines.next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty CSV"))??;
    let headers: Vec<String> = parse_csv_line(&header_line);

    let mut rows = Vec::new();
    for line in lines {
        let line = line?;
        if line.trim().is_empty() { continue; }
        let values = parse_csv_line(&line);
        let mut row = HashMap::new();
        for (i, h) in headers.iter().enumerate() {
            row.insert(h.clone(), values.get(i).cloned().unwrap_or_default());
        }
        rows.push(row);
    }
    Ok(rows)
}

fn parse_csv_line(line: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next(); // escaped quote
                    current.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            result.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(c);
        }
    }
    result.push(current.trim().to_string());
    result
}
