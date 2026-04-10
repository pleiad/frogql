//! Interactive GQL REPL (like sqlite3 but for graphs).
//!
//! Usage:
//!   gqlite <database.gdb>                        # open existing
//!   gqlite <database.gdb> --import-csv <dir>     # create from CSV, then open
//!   gqlite <database.gdb> --import-json <file>   # create from JSON, then open

use std::env;
use std::path::Path;
use std::time::Instant;

use rustyline::DefaultEditor;

use gqlrust::model::csv_loader;
use gqlrust::model::graph::Graph;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::model::value::PathValue;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::{IntermediateResult, QueryResult};
use gqlrust::store::lazy::LazyGraphStore;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  {} <database.gdb>                        # open existing", args[0]);
        eprintln!("  {} <database.gdb> --import-csv <dir>     # create from CSV", args[0]);
        eprintln!("  {} <database.gdb> --import-json <file>   # create from JSON", args[0]);
        std::process::exit(1);
    }

    let db_path = Path::new(&args[1]);

    // Import if requested
    if args.len() >= 4 {
        let mode = &args[2];
        let source = &args[3];
        import(db_path, mode, source);
    } else if !db_path.exists() {
        eprintln!("Error: {} does not exist. Use --import-csv or --import-json to create it.", db_path.display());
        std::process::exit(1);
    }

    // Open
    eprintln!("Opening {}...", db_path.display());
    let t0 = Instant::now();
    let store = LazyGraphStore::open(db_path).expect("failed to open database");
    eprintln!("Loaded {} nodes, {} edges in {:.2}s",
        store.node_count(), store.edge_count(), t0.elapsed().as_secs_f64());
    eprintln!("Type a GQL query or 'quit'. Try 'schema' to see labels.");
    eprintln!();

    let rt = Runtime::new(&store);
    let mut rl = DefaultEditor::new().expect("failed to init readline");

    loop {
        let line = match rl.readline("gql> ") {
            Ok(l) => l,
            Err(rustyline::error::ReadlineError::Interrupted) => continue,
            Err(rustyline::error::ReadlineError::Eof) => break,
            Err(e) => { eprintln!("Error: {e}"); break; }
        };
        let line = line.trim();
        if line.is_empty() { continue; }

        rl.add_history_entry(line).ok();

        if line == "quit" || line == "exit" { break; }

        if line == "schema" {
            print_schema(&store);
            continue;
        }

        let query = match gqlrust::compile_query(line) {
            Ok(q) => q,
            Err(e) => { eprintln!("Parse error: {e}"); continue; }
        };

        let start = Instant::now();
        let result = rt.run_query(&query, 100);
        let elapsed = start.elapsed();

        match result {
            QueryResult::Projected(rows) => {
                if let Some(returns) = &query.returns {
                    let headers: Vec<String> = returns.iter().map(|r| {
                        r.alias.clone().unwrap_or_else(|| format!("{}", r.expr))
                    }).collect();
                    println!("{}", headers.join(" | "));
                    println!("{}", headers.iter().map(|h| "-".repeat(h.len())).collect::<Vec<_>>().join("-+-"));
                }
                for row in &rows {
                    let vals: Vec<String> = row.iter().map(|v| format!("{v}")).collect();
                    println!("{}", vals.join(" | "));
                }
                eprintln!("{} rows ({:.3}s)", rows.len(), elapsed.as_secs_f64());
            }
            QueryResult::Raw(ir) => {
                print_raw_table(&store, &ir, 20);
                eprintln!("{} rows ({:.3}s)", ir.rows.len(), elapsed.as_secs_f64());
            }
        }
        println!();
    }
}

fn import(db_path: &Path, mode: &str, source: &str) {
    if db_path.exists() {
        eprintln!("{} already exists. Delete it first to reimport.", db_path.display());
        std::process::exit(1);
    }

    eprintln!("Importing from {}...", source);
    let t0 = Instant::now();

    let graph = match mode {
        "--import-csv" => {
            csv_loader::load_from_csv_dir(Path::new(source))
                .expect("failed to load CSV")
        }
        "--import-json" => {
            Graph::from_file(Path::new(source))
                .expect("failed to load JSON")
        }
        _ => {
            eprintln!("Unknown import mode: {mode}. Use --import-csv or --import-json");
            std::process::exit(1);
        }
    };

    eprintln!("Loaded {} nodes, {} edges in {:.2}s",
        graph.node_count(), graph.edge_count(), t0.elapsed().as_secs_f64());

    eprintln!("Saving to {}...", db_path.display());
    graph.save(db_path).expect("failed to save database");
    let size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    eprintln!("Saved ({:.1} MB)", size as f64 / 1_048_576.0);
}

/// Print raw results as a table with columns [path, var1, var2, ...].
/// Resolves internal IDs to user-facing names.
fn print_raw_table(store: &LazyGraphStore, ir: &IntermediateResult, max_rows: usize) {
    if ir.rows.is_empty() {
        return;
    }

    // Collect variable names (sorted, consistent across rows)
    let mut var_names: Vec<String> = {
        let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for row in &ir.rows {
            for k in row.assignment.m.keys() {
                names.insert(k.clone());
            }
        }
        names.into_iter().collect()
    };

    // Build headers
    let mut headers = vec!["path".to_string()];
    headers.extend(var_names.iter().cloned());

    // Build cell values for each row
    let display_rows: Vec<Vec<String>> = ir.rows.iter().take(max_rows).map(|row| {
        let mut cells = Vec::new();

        // Path column: raw IDs (n0 e200 n1332)
        let path_str = format!("{}", row.path);
        cells.push(path_str);

        // Variable columns: labels + properties
        for var in &var_names {
            let val = row.assignment.m.get(var)
                .map(|pv| format_pathvalue_rich(store, pv))
                .unwrap_or_else(|| "-".to_string());
            cells.push(val);
        }

        cells
    }).collect();

    // Compute column widths
    let num_cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in &display_rows {
        for (i, cell) in row.iter().enumerate() {
            if i < num_cols {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }

    // Print header
    let header_line: Vec<String> = headers.iter().enumerate()
        .map(|(i, h)| format!("{:width$}", h, width = widths[i]))
        .collect();
    println!("{}", header_line.join(" | "));

    // Print separator
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    println!("{}", sep.join("-+-"));

    // Print rows
    for row in &display_rows {
        let line: Vec<String> = row.iter().enumerate()
            .map(|(i, cell)| format!("{:width$}", cell, width = widths.get(i).copied().unwrap_or(0)))
            .collect();
        println!("{}", line.join(" | "));
    }

    if ir.rows.len() > max_rows {
        println!("... ({} more rows)", ir.rows.len() - max_rows);
    }
}

/// Format a PathValue with labels and properties (for variable columns).
fn format_pathvalue_rich(store: &LazyGraphStore, pv: &PathValue) -> String {
    match pv {
        PathValue::Node(id) => {
            let labels = store.node_labels(*id);
            let label_strs = labels.required_labels();
            let props = store.node_props(*id);
            let label_part = if label_strs.is_empty() {
                format!("n{id}")
            } else {
                label_strs.join("&")
            };
            if props.is_empty() {
                label_part
            } else {
                let prop_parts: Vec<String> = props.iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect();
                format!("{label_part} {{{}}}", prop_parts.join(", "))
            }
        }
        PathValue::EdgeDirectional(id) | PathValue::EdgeUndirectional(id) => {
            let labels = store.edge_labels(*id);
            let label_strs = labels.required_labels();
            let props = store.edge_props(*id);
            let label_part = if label_strs.is_empty() {
                format!("e{id}")
            } else {
                label_strs.join("&")
            };
            if props.is_empty() {
                label_part
            } else {
                let prop_parts: Vec<String> = props.iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect();
                format!("{label_part} {{{}}}", prop_parts.join(", "))
            }
        }
        PathValue::Nothing => "-".to_string(),
        PathValue::List(items) => {
            let parts: Vec<String> = items.iter().map(|v| format_pathvalue_rich(store, v)).collect();
            format!("[{}]", parts.join(", "))
        }
    }
}

/// A node type: label combination + property name→type map.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct NodeType {
    labels: Vec<String>,               // sorted
    props: std::collections::BTreeMap<String, &'static str>, // prop_name → "str"|"int"|"bool"
}

/// An edge type: label + props + src/tgt node types + directed?
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct EdgeType {
    labels: Vec<String>,
    props: std::collections::BTreeMap<String, &'static str>,
    src: NodeType,
    tgt: NodeType,
    directed: bool,
}

impl NodeType {
    fn from_graph_element(store: &LazyGraphStore, id: u32, is_node: bool) -> Self {
        let lt = if is_node { store.node_labels(id) } else { store.edge_labels(id) };
        let mut labels = Graph::label_strings(&lt);
        labels.sort();

        let raw_props = if is_node { store.node_props(id) } else { store.edge_props(id) };
        let mut props = std::collections::BTreeMap::new();
        for (k, v) in &raw_props {
            let t = match v {
                gqlrust::model::value::Value::Int(_) => "int",
                gqlrust::model::value::Value::Str(_) => "str",
                gqlrust::model::value::Value::Bool(_) => "bool",
            };
            props.insert(k.clone(), t);
        }

        NodeType { labels, props }
    }

    fn format(&self) -> String {
        let label_part = if self.labels.is_empty() {
            String::new()
        } else {
            format!(":{}", self.labels.join("&"))
        };

        if self.props.is_empty() {
            format!("({label_part})")
        } else {
            let prop_parts: Vec<String> = self.props.iter()
                .map(|(k, t)| format!("{k}: {t}"))
                .collect();
            format!("({label_part} {{{}}})", prop_parts.join(", "))
        }
    }
}

fn print_schema(store: &LazyGraphStore) {
    use std::collections::{HashMap, BTreeSet};

    // 1. Infer node types: group by (labels, prop_types)
    let mut node_type_counts: HashMap<NodeType, usize> = HashMap::new();
    let mut node_id_to_type: Vec<NodeType> = Vec::with_capacity(store.node_count() as usize);

    for nid in 0..store.node_count() {
        let nt = NodeType::from_graph_element(store, nid, true);
        *node_type_counts.entry(nt.clone()).or_default() += 1;
        node_id_to_type.push(nt);
    }

    // 2. Infer edge types: group by (labels, props, src_type, tgt_type, directed)
    let mut edge_type_counts: HashMap<EdgeType, usize> = HashMap::new();

    for eid in 0..store.edge_count() {
        let src_id = store.src(eid);
        let tgt_id = store.tgt(eid);
        let directed = store.is_directed(eid);

        let lt = store.edge_labels(eid);
        let mut labels = Graph::label_strings(&lt);
        labels.sort();

        let raw_props = store.edge_props(eid);
        let mut props = std::collections::BTreeMap::new();
        for (k, v) in &raw_props {
            let t = match v {
                gqlrust::model::value::Value::Int(_) => "int",
                gqlrust::model::value::Value::Str(_) => "str",
                gqlrust::model::value::Value::Bool(_) => "bool",
            };
            props.insert(k.clone(), t);
        }

        let et = EdgeType {
            labels,
            props,
            src: node_id_to_type[src_id as usize].clone(),
            tgt: node_id_to_type[tgt_id as usize].clone(),
            directed,
        };
        *edge_type_counts.entry(et).or_default() += 1;
    }

    // 3. Collect node types that appear as edge endpoints
    let mut endpoint_types: BTreeSet<NodeType> = BTreeSet::new();
    for et in edge_type_counts.keys() {
        endpoint_types.insert(et.src.clone());
        endpoint_types.insert(et.tgt.clone());
    }

    // 4. Print node types NOT already visible as edge endpoints
    let mut standalone_nodes: Vec<(&NodeType, &usize)> = node_type_counts.iter()
        .filter(|(nt, _)| !endpoint_types.contains(nt))
        .collect();
    standalone_nodes.sort_by_key(|(nt, _)| (*nt).clone());

    if !standalone_nodes.is_empty() {
        println!("Node types:");
        for (nt, count) in &standalone_nodes {
            println!("  {} ({} nodes)", nt.format(), count);
        }
    }

    // 5. Print edge types
    if !edge_type_counts.is_empty() {
        if !standalone_nodes.is_empty() {
            println!();
        }
        println!("Edge types:");
        let mut edge_types: Vec<(&EdgeType, &usize)> = edge_type_counts.iter().collect();
        edge_types.sort_by_key(|(et, _)| (*et).clone());

        for (et, count) in &edge_types {
            let edge_label = if et.labels.is_empty() {
                String::new()
            } else {
                format!(":{}", et.labels.join("&"))
            };
            let edge_props = if et.props.is_empty() {
                String::new()
            } else {
                let parts: Vec<String> = et.props.iter()
                    .map(|(k, t)| format!("{k}: {t}"))
                    .collect();
                format!(" {{{}}}", parts.join(", "))
            };

            let arrow = if et.directed {
                format!("-[{edge_label}{edge_props}]->")
            } else {
                format!("~[{edge_label}{edge_props}]~")
            };

            println!("  {} {} {} ({} edges)", et.src.format(), arrow, et.tgt.format(), count);
        }
    }

    // 6. Print summary
    println!();
    println!("{} node types, {} edge types ({} nodes, {} edges)",
        node_type_counts.len(), edge_type_counts.len(),
        store.node_count(), store.edge_count());
}
