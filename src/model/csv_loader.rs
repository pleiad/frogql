//! Load a property graph from CSV files using a spanner_import_config.json.
//!
//! The config describes node and edge CSV files with their columns and types.
//! Node files have a `vid` column (ID) + property columns.
//! Edge files have `SRC_ID`, `DST_ID`, `vid` columns + optional property columns.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::model::graph::{MemoryGraphStore, Props};
use crate::model::value::{Id, Value};
use crate::typing::label_type::LabelType;

/// Load a graph from a directory containing spanner_import_config.json and CSV files.
pub fn load_from_csv_dir(dir: &Path) -> io::Result<MemoryGraphStore> {
    let config_path = dir.join("spanner_import_config.json");
    let config_str = fs::read_to_string(&config_path)?;
    let config: serde_json::Value = serde_json::from_str(&config_str)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let files = config["files"]
        .as_array()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing 'files' array"))?;

    // First pass: identify node vs edge files
    let mut node_files = Vec::new();
    let mut edge_files = Vec::new();

    for file_config in files {
        let columns = file_config["columns"]
            .as_object()
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
        let prop_cols: Vec<(&str, &str)> = columns
            .iter()
            .filter(|(k, _)| k.as_str() != id_col)
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap_or("STRING")))
            .collect();

        let rows = read_csv(&csv_path)?;
        for row in &rows {
            let vid = get_ci(row, &id_col).unwrap_or_default();
            if vid.is_empty() {
                continue;
            }

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
    let node_type_names: Vec<String> = node_files
        .iter()
        .map(|fc| infer_node_label(fc["path"].as_str().unwrap()))
        .collect();

    for file_config in &edge_files {
        let csv_name = file_config["path"].as_str().unwrap();
        // Use the config's label field if available (strip node types from it),
        // otherwise infer from filename
        let raw_label = file_config["label"]
            .as_str()
            .unwrap_or(Path::new(csv_name).file_stem().unwrap().to_str().unwrap());
        let label = strip_node_types(raw_label, &node_type_names);
        let csv_path = dir.join(csv_name);
        let columns = file_config["columns"].as_object().unwrap();
        let prop_cols: Vec<(&str, &str)> = columns
            .iter()
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
            let eid_name = get_ci(row, "vid").unwrap_or_else(|| format!("e{}", edge_names.len()));

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

    Ok(MemoryGraphStore::from_raw(
        node_names,
        node_labels,
        node_props,
        edge_names,
        edge_labels_vec,
        edge_props_vec,
        edge_src,
        edge_tgt,
        edge_directed,
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
    if columns.contains_key("vid") {
        return "vid".to_string();
    }
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
    columns
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| "vid".to_string())
}

fn infer_node_label(filename: &str) -> String {
    Path::new(filename)
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string()
}

/// Strip known node type names from a label string.
/// E.g., "PersonACTED_INMovie" with types [Person, Movie] → "ACTED_IN"
/// E.g., "TRANSACTIONINITIATED_BYACCOUNT" with types [TRANSACTION, ACCOUNT] → "INITIATED_BY"
fn strip_node_types(raw: &str, node_types: &[String]) -> String {
    let mut name = raw.to_string();
    // Strip ONE node type from start and ONE from end (longest match first).
    let mut sorted_types: Vec<&String> = node_types.iter().collect();
    sorted_types.sort_by_key(|s| std::cmp::Reverse(s.len()));
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
            name = name[..name.len() - t.len()]
                .trim_end_matches('_')
                .to_string();
            break;
        }
    }
    if name.is_empty() {
        raw.to_string()
    } else {
        name
    }
}

/// Parse a STRING-column value, detecting JSON-encoded lists and records.
/// Falls back to `Value::Str` on any parse failure.
fn parse_string_value(raw: &str) -> Value {
    let trimmed = raw.trim_start();
    let first = trimmed.chars().next();
    if matches!(first, Some('[') | Some('{')) {
        if let Ok(j) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(v) = json_to_value(&j) {
                return v;
            }
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
            if let Some(i) = n.as_i64() {
                Some(Value::Int(i))
            } else {
                n.as_f64().map(Value::Float)
            }
        }
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(json_to_value(it)?);
            }
            Some(Value::List(out))
        }
        serde_json::Value::Object(m) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in m {
                out.insert(k.clone(), json_to_value(v)?);
            }
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
            if val.is_empty() {
                continue;
            }
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

    let header_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty CSV"))??;
    let headers: Vec<String> = parse_csv_line(&header_line, ',');

    let mut rows = Vec::new();
    for line in lines {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let values = parse_csv_line(&line, ',');
        let mut row = HashMap::new();
        for (i, h) in headers.iter().enumerate() {
            row.insert(h.clone(), values.get(i).cloned().unwrap_or_default());
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Read a CSV file using a custom delimiter, returning headers and rows as
/// positional vectors (instead of a HashMap). Required for LDBC files where
/// edge headers can repeat the same column name (e.g. `Place.id|Place.id`).
fn read_csv_positional(path: &Path, delim: char) -> io::Result<(Vec<String>, Vec<Vec<String>>)> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let header_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty CSV"))??;
    let headers = parse_csv_line(&header_line, delim);

    let mut rows = Vec::new();
    for line in lines {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        rows.push(parse_csv_line(&line, delim));
    }
    Ok((headers, rows))
}

fn parse_csv_line(line: &str, delim: char) -> Vec<String> {
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
        } else if c == delim {
            result.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(c);
        }
    }
    result.push(current.trim().to_string());
    result
}

// ============================================================================
// LDBC SNB CSV loader (csv-singular-projected-fk / "CsvBasic")
// ============================================================================
//
// Distinct from `load_from_csv_dir`:
// - Pipe-delimited, no spanner_import_config.json.
// - Files split across `<dir>/static/` and `<dir>/dynamic/`.
// - Node files: header has one `id` column. Filename `<entity>_0_0.csv`.
// - Edge files: header has 2+ `<NodeType>.id` columns. Filename
//   `<src>_<edgeName>_<dst>_0_0.csv`.
// - Multi-valued attribute files (e.g. `person_email_emailaddress_0_0.csv`,
//   `person_speaks_language_0_0.csv`) look like edges by filename but the
//   header has only one `<NodeType>.id` column; per LDBC SNB §3.5 they
//   populate the source node's set-valued property (Person.email, Person.speaks).
// - `place_0_0.csv` and `organisation_0_0.csv` carry a `type` discriminator
//   column whose values (city|country|continent, company|university) get
//   promoted to a sub-label via `LabelType::And`.
// - The `knows` edge is undirected per the spec — only one direction is
//   serialized; we mark it with `edge_directed = false` so the runtime
//   treats both senses.

enum LdbcFileKind {
    Node {
        label: String,
    },
    Edge {
        label: String,
        directed: bool,
    },
    /// Set-valued attribute on the source node — e.g. Person.email.
    MultiValuedAttr {
        attr_name: String,
    },
}

struct LdbcFile {
    path: PathBuf,
    kind: LdbcFileKind,
    headers: Vec<String>,
}

/// Load a graph from an LDBC SNB CSV-Basic dataset directory.
pub fn load_from_ldbc_csv_dir(dir: &Path) -> io::Result<MemoryGraphStore> {
    let files = collect_and_classify_ldbc(dir)?;

    let mut node_names: Vec<String> = Vec::new();
    let mut node_labels: Vec<LabelType> = Vec::new();
    let mut node_props: Vec<Props> = Vec::new();
    // Keyed by (entity_label, external_id_str). Each LDBC entity type owns
    // its own id space (e.g. Place id 0 ≠ Organisation id 0), so a global
    // map would silently dedup-collide across types.
    let mut node_name_to_id: HashMap<(String, String), Id> = HashMap::new();

    // Pass 1: nodes.
    for f in &files {
        if let LdbcFileKind::Node { label } = &f.kind {
            load_ldbc_node_file(
                f,
                label,
                &mut node_names,
                &mut node_labels,
                &mut node_props,
                &mut node_name_to_id,
            )?;
        }
    }

    // Pass 2: multi-valued attributes -> Value::List on the owning node.
    for f in &files {
        if let LdbcFileKind::MultiValuedAttr { attr_name } = &f.kind {
            load_ldbc_mva_file(f, attr_name, &node_name_to_id, &mut node_props)?;
        }
    }

    // Pass 3: edges.
    let mut edge_names: Vec<String> = Vec::new();
    let mut edge_labels_vec: Vec<LabelType> = Vec::new();
    let mut edge_props_vec: Vec<Props> = Vec::new();
    let mut edge_src: Vec<Id> = Vec::new();
    let mut edge_tgt: Vec<Id> = Vec::new();
    let mut edge_directed: Vec<bool> = Vec::new();

    for f in &files {
        if let LdbcFileKind::Edge { label, directed } = &f.kind {
            load_ldbc_edge_file(
                f,
                label,
                *directed,
                &node_name_to_id,
                &mut edge_names,
                &mut edge_labels_vec,
                &mut edge_props_vec,
                &mut edge_src,
                &mut edge_tgt,
                &mut edge_directed,
            )?;
        }
    }

    Ok(MemoryGraphStore::from_raw(
        node_names,
        node_labels,
        node_props,
        edge_names,
        edge_labels_vec,
        edge_props_vec,
        edge_src,
        edge_tgt,
        edge_directed,
    ))
}

fn collect_and_classify_ldbc(dir: &Path) -> io::Result<Vec<LdbcFile>> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for sub in ["static", "dynamic"] {
        let subdir = dir.join(sub);
        if !subdir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&subdir)? {
            let entry = entry?;
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("csv") {
                paths.push(p);
            }
        }
    }
    paths.sort();

    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no .csv files found under {}/static or {}/dynamic",
                dir.display(),
                dir.display()
            ),
        ));
    }

    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let header = read_first_line(&p, '|')?;
        let kind = classify_ldbc_file(&p, &header)?;
        out.push(LdbcFile {
            path: p,
            kind,
            headers: header,
        });
    }
    Ok(out)
}

fn read_first_line(path: &Path, delim: char) -> io::Result<Vec<String>> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("empty CSV: {}", path.display()),
        ));
    }
    Ok(parse_csv_line(line.trim_end_matches(['\r', '\n']), delim))
}

fn classify_ldbc_file(path: &Path, headers: &[String]) -> io::Result<LdbcFileKind> {
    let stems = ldbc_filename_stems(path)?;
    let plain_id_count = headers
        .iter()
        .filter(|c| c.eq_ignore_ascii_case("id"))
        .count();
    let dotted_id_count = headers.iter().filter(|c| is_dotted_id(c)).count();

    if plain_id_count == 1 && stems.len() == 1 {
        return Ok(LdbcFileKind::Node {
            label: ldbc_node_label(&stems[0]),
        });
    }

    if dotted_id_count >= 2 && stems.len() == 3 {
        let label = stems[1].clone();
        let directed = !label.eq_ignore_ascii_case("knows");
        return Ok(LdbcFileKind::Edge { label, directed });
    }

    if dotted_id_count == 1 && stems.len() == 3 {
        return Ok(LdbcFileKind::MultiValuedAttr {
            attr_name: stems[1].clone(),
        });
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "cannot classify LDBC CSV file: {} (headers: {:?}, stems: {:?})",
            path.display(),
            headers,
            stems
        ),
    ))
}

fn is_dotted_id(col: &str) -> bool {
    // Match `<NodeType>.id` (case-insensitive on `.id`). Rejects bare `id`.
    let lower = col.to_lowercase();
    let dot = match col.find('.') {
        Some(i) => i,
        None => return false,
    };
    dot > 0 && &lower[dot..] == ".id"
}

fn ldbc_filename_stems(path: &Path) -> io::Result<Vec<String>> {
    let stem = path.file_stem().and_then(|s| s.to_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid filename: {}", path.display()),
        )
    })?;
    // Strip the trailing `_<block>_<part>` (e.g. `_0_0`). LDBC always uses
    // two trailing numeric segments per the CsvBasic naming scheme.
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.len() < 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected filename stem: {stem}"),
        ));
    }
    let trailing_numeric = parts[parts.len() - 2].chars().all(|c| c.is_ascii_digit())
        && parts[parts.len() - 1].chars().all(|c| c.is_ascii_digit());
    if !trailing_numeric {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected `_<n>_<m>` suffix in: {stem}"),
        ));
    }
    Ok(parts[..parts.len() - 2]
        .iter()
        .map(|s| (*s).to_string())
        .collect())
}

/// Map an LDBC entity stem (lowercase) to its canonical label.
/// Camel-case entities (TagClass) get a small lookup; everything else is
/// just first-letter capitalisation.
fn ldbc_node_label(stem: &str) -> String {
    match stem.to_ascii_lowercase().as_str() {
        "tagclass" => "TagClass".to_string(),
        _ => capitalize_first(stem),
    }
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
    }
}

fn load_ldbc_node_file(
    f: &LdbcFile,
    label: &str,
    node_names: &mut Vec<String>,
    node_labels: &mut Vec<LabelType>,
    node_props: &mut Vec<Props>,
    node_name_to_id: &mut HashMap<(String, String), Id>,
) -> io::Result<()> {
    let id_idx = f
        .headers
        .iter()
        .position(|c| c.eq_ignore_ascii_case("id"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("node file missing `id` column: {}", f.path.display()),
            )
        })?;
    let type_idx = f
        .headers
        .iter()
        .position(|c| c.eq_ignore_ascii_case("type"));
    let prop_cols: Vec<(usize, &str)> = f
        .headers
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != id_idx && Some(*i) != type_idx)
        .map(|(i, c)| (i, c.as_str()))
        .collect();

    let (_headers, rows) = read_csv_positional(&f.path, '|')?;
    for row in rows {
        let id_str = row.get(id_idx).cloned().unwrap_or_default();
        if id_str.is_empty() {
            continue;
        }
        let key = (label.to_string(), id_str.clone());
        if node_name_to_id.contains_key(&key) {
            continue;
        }

        let lt = match type_idx {
            Some(idx) => {
                let raw = row.get(idx).cloned().unwrap_or_default();
                if raw.is_empty() {
                    LabelType::Label(label.to_string())
                } else {
                    LabelType::And(
                        Box::new(LabelType::Label(label.to_string())),
                        Box::new(LabelType::Label(capitalize_first(&raw))),
                    )
                }
            }
            None => LabelType::Label(label.to_string()),
        };

        let mut props: Props = HashMap::new();
        for (i, name) in &prop_cols {
            if let Some(v) = row.get(*i) {
                if !v.is_empty() {
                    props.insert((*name).to_string(), parse_ldbc_value(v));
                }
            }
        }
        // Store the LDBC `id` as a queryable property too. LDBC SNB
        // Interactive queries (IC1-IC14) all parameterize by `Person.id`
        // / `Comment.id` / `Post.id` etc. via `{id: $param}` shorthand
        // or `WHERE x.id = $param`. Without this it can't be expressed
        // faithfully against the spec — `id` would render as NULL. The
        // value is also folded into the internal node name (below)
        // for cross-file edge resolution; the duplication is small
        // (~12 bytes per node) and required for spec-faithful queries.
        props.insert("id".to_string(), parse_ldbc_value(&id_str));

        let nid = node_names.len() as Id;
        node_name_to_id.insert(key, nid);
        // Prefix with the entity label so the MemoryGraphStore's internal node-name
        // index stays unique across the LDBC entity types that share id 0.
        node_names.push(format!("{label}:{id_str}"));
        node_labels.push(lt);
        node_props.push(props);
    }
    Ok(())
}

fn load_ldbc_mva_file(
    f: &LdbcFile,
    attr_name: &str,
    node_name_to_id: &HashMap<(String, String), Id>,
    node_props: &mut [Props],
) -> io::Result<()> {
    let id_idx = f
        .headers
        .iter()
        .position(|c| is_dotted_id(c))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "multi-valued attr file missing `<Type>.id` column: {}",
                    f.path.display()
                ),
            )
        })?;
    let val_idx = f
        .headers
        .iter()
        .enumerate()
        .find(|(i, _)| *i != id_idx)
        .map(|(i, _)| i)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "multi-valued attr file missing value column: {}",
                    f.path.display()
                ),
            )
        })?;
    let entity = entity_from_dotted_id(&f.headers[id_idx]);

    let (_headers, rows) = read_csv_positional(&f.path, '|')?;
    for row in rows {
        let id_str = row.get(id_idx).cloned().unwrap_or_default();
        let val = row.get(val_idx).cloned().unwrap_or_default();
        if id_str.is_empty() || val.is_empty() {
            continue;
        }
        let Some(&nid) = node_name_to_id.get(&(entity.clone(), id_str)) else {
            continue;
        };
        let entry = node_props[nid as usize]
            .entry(attr_name.to_string())
            .or_insert_with(|| Value::List(Vec::new()));
        if let Value::List(items) = entry {
            items.push(parse_ldbc_value(&val));
        }
    }
    Ok(())
}

/// Pull the entity type out of a dotted-id header like `Person.id` -> `Person`.
fn entity_from_dotted_id(header: &str) -> String {
    header
        .split('.')
        .next()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn load_ldbc_edge_file(
    f: &LdbcFile,
    label: &str,
    directed: bool,
    node_name_to_id: &HashMap<(String, String), Id>,
    edge_names: &mut Vec<String>,
    edge_labels_vec: &mut Vec<LabelType>,
    edge_props_vec: &mut Vec<Props>,
    edge_src: &mut Vec<Id>,
    edge_tgt: &mut Vec<Id>,
    edge_directed: &mut Vec<bool>,
) -> io::Result<()> {
    let id_indices: Vec<usize> = f
        .headers
        .iter()
        .enumerate()
        .filter(|(_, c)| is_dotted_id(c))
        .map(|(i, _)| i)
        .collect();
    if id_indices.len() < 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "edge file expected 2+ `<Type>.id` columns: {}",
                f.path.display()
            ),
        ));
    }
    let (src_idx, tgt_idx) = (id_indices[0], id_indices[1]);
    let src_entity = entity_from_dotted_id(&f.headers[src_idx]);
    let tgt_entity = entity_from_dotted_id(&f.headers[tgt_idx]);
    let prop_cols: Vec<(usize, &str)> = f
        .headers
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != src_idx && *i != tgt_idx)
        .map(|(i, c)| (i, c.as_str()))
        .collect();

    let (_headers, rows) = read_csv_positional(&f.path, '|')?;
    for row in rows {
        let src_name = row.get(src_idx).cloned().unwrap_or_default();
        let tgt_name = row.get(tgt_idx).cloned().unwrap_or_default();
        let Some(&src_id) = node_name_to_id.get(&(src_entity.clone(), src_name)) else {
            continue;
        };
        let Some(&tgt_id) = node_name_to_id.get(&(tgt_entity.clone(), tgt_name)) else {
            continue;
        };

        let mut props: Props = HashMap::new();
        for (i, name) in &prop_cols {
            if let Some(v) = row.get(*i) {
                if !v.is_empty() {
                    props.insert((*name).to_string(), parse_ldbc_value(v));
                }
            }
        }

        edge_names.push(format!("e{}", edge_names.len()));
        edge_labels_vec.push(LabelType::Label(label.to_string()));
        edge_props_vec.push(props);
        edge_src.push(src_id);
        edge_tgt.push(tgt_id);
        edge_directed.push(directed);
    }
    Ok(())
}

/// Best-effort scalar coercion for LDBC values: try i64 -> f64 -> bool -> str.
/// LDBC encodes ids and timestamps as integers; names/IPs/etc. stay strings.
fn parse_ldbc_value(s: &str) -> Value {
    if let Ok(i) = s.parse::<i64>() {
        return Value::Int(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        if f.is_finite() {
            return Value::Float(f);
        }
    }
    match s {
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    Value::Str(s.to_string())
}

#[cfg(test)]
mod ldbc_tests {
    use super::*;
    use crate::model::graph_access::GraphAccess;
    use std::fs;

    fn write_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn fixture_dir() -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("gqlrust-ldbc-fixture-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        // Static: place + organisation with discriminators, plus tag/tagclass.
        write_file(
            &base.join("static/place_0_0.csv"),
            "id|name|url|type\n\
             1|Chile|http://x/Chile|country\n\
             2|Santiago|http://x/Santiago|city\n\
             3|SouthAmerica|http://x/SouthAmerica|continent\n",
        );
        write_file(
            &base.join("static/place_isPartOf_place_0_0.csv"),
            "Place.id|Place.id\n\
             2|1\n\
             1|3\n",
        );
        write_file(
            &base.join("static/organisation_0_0.csv"),
            "id|type|name|url\n\
             10|university|UTalca|http://x/UTalca\n\
             11|company|Acme|http://x/Acme\n",
        );
        write_file(
            &base.join("static/tag_0_0.csv"),
            "id|name|url\n\
             100|graphs|http://x/graphs\n",
        );
        write_file(
            &base.join("static/tagclass_0_0.csv"),
            "id|name|url\n\
             200|Topic|http://x/Topic\n",
        );

        // Dynamic: persons with multi-valued email/speaks, knows (undirected).
        write_file(
            &base.join("dynamic/person_0_0.csv"),
            "id|firstName|lastName|gender|birthday|creationDate|locationIP|browserUsed\n\
             50|Ana|Soto|female|628646400000|1266161530447|1.2.3.4|Firefox\n\
             51|Beto|Lara|male|628646400000|1266161530448|1.2.3.5|Chrome\n",
        );
        write_file(
            &base.join("dynamic/person_email_emailaddress_0_0.csv"),
            "Person.id|email\n\
             50|ana@x.cl\n\
             50|ana@y.cl\n\
             51|beto@x.cl\n",
        );
        write_file(
            &base.join("dynamic/person_speaks_language_0_0.csv"),
            "Person.id|language\n\
             50|es\n\
             50|en\n\
             51|es\n",
        );
        write_file(
            &base.join("dynamic/person_knows_person_0_0.csv"),
            "Person.id|Person.id|creationDate\n\
             50|51|1271939457947\n",
        );
        write_file(
            &base.join("dynamic/person_isLocatedIn_place_0_0.csv"),
            "Person.id|Place.id\n\
             50|2\n\
             51|2\n",
        );

        base
    }

    #[test]
    fn loads_ldbc_fixture() {
        let dir = fixture_dir();
        let g = load_from_ldbc_csv_dir(&dir).expect("load");

        // 3 places + 2 orgs + 1 tag + 1 tagclass + 2 persons = 9 nodes
        assert_eq!(g.node_count(), 9);

        // Edges: 2 isPartOf + 1 knows + 2 isLocatedIn = 5
        assert_eq!(g.edge_count(), 5);

        // knows is undirected, others directed
        let mut knows_undirected = false;
        let mut knows_count = 0;
        let mut directed_count = 0;
        for eid in 0..g.edge_count() as Id {
            let labels = MemoryGraphStore::label_strings(&g.edge_labels(eid));
            if labels.iter().any(|s| s == "knows") {
                knows_count += 1;
                if !g.is_directed(eid) {
                    knows_undirected = true;
                }
            } else if g.is_directed(eid) {
                directed_count += 1;
            }
        }
        assert_eq!(knows_count, 1);
        assert!(knows_undirected, "knows should be undirected");
        assert_eq!(directed_count, 4);

        // Place subtype discriminator was promoted to a sub-label.
        let mut city_count = 0;
        let mut country_count = 0;
        let mut continent_count = 0;
        for nid in 0..g.node_count() as Id {
            let labels = MemoryGraphStore::label_strings(&g.node_labels(nid));
            if labels.contains(&"City".to_string()) {
                city_count += 1;
            }
            if labels.contains(&"Country".to_string()) {
                country_count += 1;
            }
            if labels.contains(&"Continent".to_string()) {
                continent_count += 1;
            }
        }
        assert_eq!(city_count, 1);
        assert_eq!(country_count, 1);
        assert_eq!(continent_count, 1);

        // Person.email and Person.speaks landed as Value::List on the source.
        // Names are stored prefixed with the entity label to keep the global
        // `node_name → id` map collision-free across LDBC entity types.
        let person_50 = g
            .node_id_by_name("Person:50")
            .expect("person 50 should be loaded by external id");
        let props = g.node_props(person_50);
        match props.get("email") {
            Some(Value::List(items)) => {
                assert_eq!(items.len(), 2, "ana has two emails");
            }
            other => panic!("expected email as List, got {:?}", other),
        }
        match props.get("speaks") {
            Some(Value::List(items)) => {
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected speaks as List, got {:?}", other),
        }

        // LDBC `id` is also surfaced as a queryable property — required
        // for spec-faithful IC1-IC14 queries that anchor by `Person.id`.
        match props.get("id") {
            Some(Value::Int(v)) => assert_eq!(*v, 50),
            other => panic!("expected id=Int(50) on person 50, got {:?}", other),
        }

        // Same for non-Person entities. Sanity-check on a Place node.
        let place = g
            .node_id_by_name("Place:1")
            .expect("place 1 should be loaded");
        match g.node_props(place).get("id") {
            Some(Value::Int(v)) => assert_eq!(*v, 1),
            other => panic!("expected id=Int(1) on place 1, got {:?}", other),
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
