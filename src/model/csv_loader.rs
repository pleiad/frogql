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
        let is_edge = columns.contains_key("SRC_ID") && columns.contains_key("DST_ID");
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
        let prop_cols: Vec<(&str, &str)> = columns.iter()
            .filter(|(k, _)| k.as_str() != "vid")
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap_or("STRING")))
            .collect();

        let rows = read_csv(&csv_path)?;
        for row in &rows {
            let vid = row.get("vid").cloned().unwrap_or_default();
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

    for file_config in &edge_files {
        let csv_name = file_config["path"].as_str().unwrap();
        let label = infer_edge_label(csv_name);
        let csv_path = dir.join(csv_name);
        let columns = file_config["columns"].as_object().unwrap();
        let prop_cols: Vec<(&str, &str)> = columns.iter()
            .filter(|(k, _)| !matches!(k.as_str(), "SRC_ID" | "DST_ID" | "vid"))
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap_or("STRING")))
            .collect();

        let rows = read_csv(&csv_path)?;
        for row in &rows {
            let src_name = row.get("SRC_ID").cloned().unwrap_or_default();
            let dst_name = row.get("DST_ID").cloned().unwrap_or_default();
            let eid_name = row.get("vid").cloned()
                .unwrap_or_else(|| format!("e_{}_{}", src_name, dst_name));

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

fn infer_node_label(filename: &str) -> String {
    Path::new(filename).file_stem().unwrap().to_string_lossy().to_string()
}

fn infer_edge_label(filename: &str) -> String {
    let name = Path::new(filename).file_stem().unwrap().to_string_lossy();
    // Pattern: PersonACTED_INMovie → ACTED_IN
    // Find the uppercase+underscore segment between two CamelCase words
    let chars: Vec<char> = name.chars().collect();
    let mut start = 0;
    // Skip first CamelCase word (starts uppercase, then lowercase)
    for i in 1..chars.len() {
        if chars[i].is_uppercase() || chars[i] == '_' {
            start = i;
            break;
        }
    }
    // Find where the trailing CamelCase word starts
    let mut end = chars.len();
    for i in (start + 1..chars.len()).rev() {
        if chars[i].is_lowercase() && i > 0 && chars[i - 1].is_uppercase() {
            end = i - 1;
            break;
        }
    }
    name[start..end].to_string()
}

fn extract_props(row: &HashMap<String, String>, prop_cols: &[(&str, &str)]) -> Props {
    let mut props = HashMap::new();
    for &(col_name, col_type) in prop_cols {
        if let Some(val) = row.get(col_name) {
            if val.is_empty() { continue; }
            let converted = match col_type {
                "INT64" => val.parse::<i64>().ok().map(Value::Int),
                "BOOL" => Some(Value::Bool(val.eq_ignore_ascii_case("true") || val == "1")),
                _ => Some(Value::Str(val.clone())),
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
