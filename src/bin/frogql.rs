//! Interactive GQL REPL (like sqlite3 but for graphs).
//!
//! Usage:
//!   frogql <database.gdb> [--no-typecheck]                          # open existing
//!   frogql <database.gdb> --import-csv <dir> [--no-typecheck]       # create from CSV (spanner_import_config.json)
//!   frogql <database.gdb> --import-ldbc-csv <dir> [--no-typecheck]  # create from LDBC SNB CsvBasic dataset
//!   frogql <database.gdb> --import-json <file> [--no-typecheck]     # create from JSON

use std::env;
use std::path::Path;
use std::time::Instant;

use rustyline::DefaultEditor;

use gqlrust::model::csv_loader;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::model::value::PathValue;
use gqlrust::parser::parse_statement;
use gqlrust::runtime::catalog::ValidationStatus;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::{IntermediateResult, QueryResult};
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::statement::{Statement, TypeElement};
use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::inference::infer_simple_schema;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::simple_type::SimpleType;
use gqlrust::typing::validate::{validate_against_data, ElementKind, ValidationReport};
use gqlrust::typing::variable_type::{Schema, VariableType};

fn main() {
    let mut args: Vec<String> = env::args().collect();

    // Strip --no-typecheck so positional dispatch below stays simple.
    let typecheck = !args.iter().any(|a| a == "--no-typecheck");
    args.retain(|a| a != "--no-typecheck");

    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!(
            "  {} <database.gdb> [--no-typecheck]                        # open existing",
            args[0]
        );
        eprintln!(
            "  {} <database.gdb> --import-csv <dir> [--no-typecheck]       # create from CSV (spanner_import_config.json)",
            args[0]
        );
        eprintln!(
            "  {} <database.gdb> --import-ldbc-csv <dir> [--no-typecheck]  # create from LDBC SNB CsvBasic dataset",
            args[0]
        );
        eprintln!(
            "  {} <database.gdb> --import-json <file> [--no-typecheck]     # create from JSON",
            args[0]
        );
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --no-typecheck    skip the typechecker for this session (default: on)");
        std::process::exit(1);
    }

    let db_path = Path::new(&args[1]);

    // Import if requested
    let creating_fresh = if args.len() >= 4 {
        let mode = &args[2];
        let source = &args[3];
        import(db_path, mode, source);
        false
    } else {
        // SQLite-style: opening a path that doesn't exist creates an
        // empty database. The user can then INSERT and `.save`.
        !db_path.exists()
    };
    if creating_fresh {
        eprintln!("creating new database: {}", db_path.display());
    }

    // Open
    eprintln!("Opening {}...", db_path.display());
    let t0 = Instant::now();
    let store = LazyGraphStore::open_or_create(db_path).expect("failed to open database");
    eprintln!(
        "Loaded {} nodes, {} edges in {:.2}s",
        store.node_count(),
        store.edge_count(),
        t0.elapsed().as_secs_f64()
    );
    eprintln!("Typechecker: {}", if typecheck { "on" } else { "off" });
    match store.catalog().active_name() {
        Some(name) => eprintln!("Active GRAPH TYPE: {name}."),
        None => eprintln!("Active GRAPH TYPE: (none — schema-permissive)."),
    }
    eprintln!("Type a GQL query, '.help' for meta-commands, or '.quit' to exit.");
    eprintln!();

    let rt = Runtime::new(&store);
    // Build the LTJ TripleIndex up-front so the first user query doesn't
    // pay the ~700ms (SF0.1) / multi-second (SF1) cold-build cost. The
    // load-time line below covers it.
    let t_idx = Instant::now();
    rt.warm_triple_index();
    eprintln!(
        "LTJ TripleIndex built in {:.2}s",
        t_idx.elapsed().as_secs_f64()
    );
    let mut rl = DefaultEditor::new().expect("failed to init readline");

    loop {
        let line = match rl.readline("gql> ") {
            Ok(l) => l,
            Err(rustyline::error::ReadlineError::Interrupted) => continue,
            Err(rustyline::error::ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("Error: {e}");
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        rl.add_history_entry(line).ok();

        // SQLite-style dot-commands. All REPL meta-commands start with `.`
        // so they don't collide with valid GQL syntax. `quit`/`exit` without
        // the dot also work because they're nearly universal.
        if line == ".quit" || line == ".exit" || line == "quit" || line == "exit" {
            break;
        }

        if line == ".help" {
            print_help();
            continue;
        }

        if let Some(rest) = line.strip_prefix(".schema") {
            let arg = rest.trim();
            match arg {
                // Bare `.schema` aliases `SHOW GRAPH TYPE DEFAULT`: prints the
                // auto-derived schema entry from the catalog with active markers
                // and validation status.
                "" => handle_show(&store, "DEFAULT"),
                "simple" => print_schema_simple(&store),
                _ => eprintln!("Unknown .schema arg. Use '.schema' or '.schema simple'."),
            }
            continue;
        }

        if line == ".graph-types" {
            print_graph_types(&store);
            continue;
        }

        if line == ".indexes" {
            print_indexes(&store);
            continue;
        }

        // `.dump-json <path>` — pg_dump-style snapshot of the merged
        // base+overlay view to a JSON file the same shape `import_json`
        // consumes. Useful for backup / inter-version transport without
        // depending on the binary `.gdb` format.
        if let Some(rest) = line.strip_prefix(".dump-json") {
            let target = rest.trim();
            if target.is_empty() {
                eprintln!("usage: .dump-json <path>");
                continue;
            }
            let t = Instant::now();
            match gqlrust::store::dump::dump_to_json_file(&store, Path::new(target)) {
                Ok(()) => eprintln!("Dumped to {target} ({:.3}s).", t.elapsed().as_secs_f64()),
                Err(e) => eprintln!("Dump failed: {e}"),
            }
            continue;
        }

        // `.dump-gql <path>` — emit a GQL script that recreates the
        // graph (`INSERT` per node, `MATCH+INSERT` per edge, final
        // `REMOVE` to strip the synthetic `_dump_id` key). MVP-1.F.
        if let Some(rest) = line.strip_prefix(".dump-gql") {
            let target = rest.trim();
            if target.is_empty() {
                eprintln!("usage: .dump-gql <path>");
                continue;
            }
            let t = Instant::now();
            match gqlrust::store::dump::dump_to_gql_file(&store, Path::new(target)) {
                Ok(()) => eprintln!("Dumped to {target} ({:.3}s).", t.elapsed().as_secs_f64()),
                Err(e) => eprintln!("Dump failed: {e}"),
            }
            continue;
        }

        // SQLite-style `.save` — atomically writes the merged base+overlay
        // state to `db_path`. Until the user invokes this, DML mutations
        // live only in the in-RAM overlay (no auto-commit).
        if line == ".save" {
            let t = Instant::now();
            match store.save(db_path) {
                Ok(()) => eprintln!(
                    "Saved {} ({:.3}s).",
                    db_path.display(),
                    t.elapsed().as_secs_f64()
                ),
                Err(e) => eprintln!("Save failed: {e}"),
            }
            continue;
        }

        // Parse top-level statement: query or DDL.
        let stmt = match parse_statement(line) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Parse error: {e}");
                continue;
            }
        };

        let query = match stmt {
            Statement::CreateGraphType { name, body } => {
                handle_create(&store, &name, &body);
                continue;
            }
            Statement::UseGraphType {
                name,
                refresh_default,
            } => {
                handle_use(&store, &name, refresh_default);
                continue;
            }
            Statement::DropGraphType { name } => {
                handle_drop(&store, &name);
                continue;
            }
            Statement::ShowGraphTypes => {
                print_graph_types(&store);
                continue;
            }
            Statement::ShowGraphType { name } => {
                handle_show(&store, &name);
                continue;
            }
            Statement::ShowCurrentGraphType => {
                handle_show_current(&store);
                continue;
            }
            Statement::ValidateGraphType { name } => {
                handle_validate(&store, &name);
                continue;
            }
            Statement::CreateIndex {
                name,
                label,
                prop,
                kind,
            } => {
                handle_create_index(&store, name, &label, &prop, kind);
                continue;
            }
            Statement::DropIndex { name } => {
                handle_drop_index(&store, &name);
                continue;
            }
            Statement::ShowIndexes => {
                print_indexes(&store);
                continue;
            }
            Statement::DataModification(dm) => {
                let t = Instant::now();
                // ISO §13 G2000: validate against the active GRAPH TYPE
                // unless it's DEFAULT (DEFAULT is data-derived; see Fase 7).
                let active_name = store.catalog().active_name().map(str::to_string);
                let schema_for_validation = match active_name.as_deref() {
                    None | Some("DEFAULT") => None,
                    _ => Some(store.catalog().active_schema()),
                };
                match gqlrust::runtime::dm::run_dm(&store, &dm, schema_for_validation.as_ref()) {
                    Ok(exec) => {
                        rt.invalidate_caches();
                        // ISO §13 doesn't mention DEFAULT specifically, but
                        // gqlite re-infers it lazily on next access (Fase 7).
                        store.catalog_mut().mark_default_dirty();
                        eprintln!(
                            "OK ({} nodes inserted, {} edges inserted, {} nodes deleted, \
                             {} edges deleted, {} nodes modified, {} edges modified, \
                             {} rows; {:.3}s)",
                            exec.nodes_inserted,
                            exec.edges_inserted,
                            exec.nodes_deleted,
                            exec.edges_deleted,
                            exec.nodes_modified,
                            exec.edges_modified,
                            exec.rows.len(),
                            t.elapsed().as_secs_f64()
                        );
                    }
                    Err(e) => eprintln!("DML error: {e}"),
                }
                continue;
            }
            Statement::Query(q) => {
                if typecheck {
                    let active = store.catalog().active_schema();
                    match gqlrust::compile_query_with_diagnostics_with(&active, line) {
                        Ok(r) => {
                            for w in &r.warnings {
                                eprintln!("warning: {w}");
                            }
                            if r.guaranteed_empty {
                                eprintln!(
                                    "0 rows (typechecker: guaranteed empty, runtime skipped)"
                                );
                                continue;
                            }
                            r.query
                        }
                        Err(gqlrust::CompileError::Parse(e)) => {
                            eprintln!("Parse error: {e}");
                            continue;
                        }
                        Err(gqlrust::CompileError::Type(es)) => {
                            eprintln!("Type error: {}", es.join("; "));
                            continue;
                        }
                    }
                } else {
                    // The query AST is already parsed; just run optimizer
                    // unchecked. Round-trip through the helper to keep the
                    // pipeline canonical.
                    let _ = q;
                    match gqlrust::compile_query_unchecked(line) {
                        Ok(q) => q,
                        Err(e) => {
                            eprintln!("Parse error: {e}");
                            continue;
                        }
                    }
                }
            }
        };

        let start = Instant::now();
        let result = rt.run_query(&query, 100);
        let elapsed = start.elapsed();

        match result {
            QueryResult::Projected(rows) => {
                if let Some(returns) = &query.returns {
                    let headers: Vec<String> = returns
                        .iter()
                        .map(|r| {
                            r.alias()
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| format!("{r}"))
                        })
                        .collect();

                    // Format all cell values
                    let str_rows: Vec<Vec<String>> = rows
                        .iter()
                        .map(|row| row.iter().map(|v| format!("{v}")).collect())
                        .collect();

                    // Compute column widths from headers and data
                    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
                    for row in &str_rows {
                        for (i, cell) in row.iter().enumerate() {
                            if i < widths.len() {
                                widths[i] = widths[i].max(cell.len());
                            }
                        }
                    }

                    // Print header
                    let header_line: String = headers
                        .iter()
                        .enumerate()
                        .map(|(i, h)| format!("{:width$}", h, width = widths[i]))
                        .collect::<Vec<_>>()
                        .join(" | ");
                    println!("{header_line}");

                    // Print separator
                    let sep: String = widths
                        .iter()
                        .map(|w| "-".repeat(*w))
                        .collect::<Vec<_>>()
                        .join("-+-");
                    println!("{sep}");

                    // Print rows
                    for row in &str_rows {
                        let line: String = row
                            .iter()
                            .enumerate()
                            .map(|(i, cell)| {
                                format!(
                                    "{:width$}",
                                    cell,
                                    width = widths.get(i).copied().unwrap_or(0)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" | ");
                        println!("{line}");
                    }
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
        eprintln!(
            "{} already exists. Delete it first to reimport.",
            db_path.display()
        );
        std::process::exit(1);
    }

    eprintln!("Importing from {}...", source);
    let t0 = Instant::now();

    let graph = match mode {
        "--import-csv" => {
            csv_loader::load_from_csv_dir(Path::new(source)).expect("failed to load CSV")
        }
        "--import-ldbc-csv" => {
            csv_loader::load_from_ldbc_csv_dir(Path::new(source)).expect("failed to load LDBC CSV")
        }
        "--import-json" => {
            MemoryGraphStore::from_file(Path::new(source)).expect("failed to load JSON")
        }
        _ => {
            eprintln!(
                "Unknown import mode: {mode}. Use --import-csv, --import-ldbc-csv, or --import-json"
            );
            std::process::exit(1);
        }
    };

    eprintln!(
        "Loaded {} nodes, {} edges in {:.2}s",
        graph.node_count(),
        graph.edge_count(),
        t0.elapsed().as_secs_f64()
    );

    eprintln!("Saving to {}...", db_path.display());
    graph.save(db_path).expect("failed to save database");
    let size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    eprintln!("Saved ({:.1} MB)", size as f64 / 1_048_576.0);

    // Populate the DEFAULT graph type from the freshly-loaded data and
    // mark it active. Any errors here are non-fatal — the .gdb is still
    // usable, the user can refresh DEFAULT later via `USE GRAPH TYPE
    // DEFAULT`.
    match LazyGraphStore::open(db_path) {
        Ok(store) => {
            let schema = infer_simple_schema(&store);
            let node_count = schema.nodes.len();
            let edge_count = schema.edges.len();
            store.catalog_mut().install_default(schema);
            if let Err(e) = store.save_catalog() {
                eprintln!("warning: failed to persist DEFAULT graph type: {e}");
            } else {
                eprintln!(
                    "Inferred DEFAULT graph type: {node_count} node types, {edge_count} edge types"
                );
            }
        }
        Err(e) => eprintln!("warning: failed to reopen db for DEFAULT inference: {e}"),
    }
}

/// Print raw results as a table with columns [path, var1, var2, ...].
/// Resolves internal IDs to user-facing names.
fn print_raw_table(store: &LazyGraphStore, ir: &IntermediateResult, max_rows: usize) {
    if ir.rows.is_empty() {
        return;
    }

    // Collect variable names (sorted, consistent across rows)
    let var_names: Vec<String> = {
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
    let display_rows: Vec<Vec<String>> = ir
        .rows
        .iter()
        .take(max_rows)
        .map(|row| {
            let mut cells = Vec::new();

            // Path column: tuple of paths, separated by " | "
            let path_str = row
                .paths
                .iter()
                .map(|p| format!("{}", p))
                .collect::<Vec<_>>()
                .join(" | ");
            cells.push(path_str);

            // Variable columns: labels + properties
            for var in &var_names {
                let val = row
                    .assignment
                    .m
                    .get(var)
                    .map(|pv| format_pathvalue_rich(store, pv))
                    .unwrap_or_else(|| "-".to_string());
                cells.push(val);
            }

            cells
        })
        .collect();

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
    let header_line: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| format!("{:width$}", h, width = widths[i]))
        .collect();
    println!("{}", header_line.join(" | "));

    // Print separator
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    println!("{}", sep.join("-+-"));

    // Print rows
    for row in &display_rows {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                format!(
                    "{:width$}",
                    cell,
                    width = widths.get(i).copied().unwrap_or(0)
                )
            })
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
                let prop_parts: Vec<String> =
                    props.iter().map(|(k, v)| format!("{k}: {v}")).collect();
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
                let prop_parts: Vec<String> =
                    props.iter().map(|(k, v)| format!("{k}: {v}")).collect();
                format!("{label_part} {{{}}}", prop_parts.join(", "))
            }
        }
        PathValue::Nothing => "-".to_string(),
        PathValue::Group(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|v| format_pathvalue_rich(store, v))
                .collect();
            format!("[{}]", parts.join(", "))
        }
        PathValue::Path(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|v| format_pathvalue_rich(store, v))
                .collect();
            format!("<{}>", parts.join(", "))
        }
    }
}

// --- ANSI color helpers ---

const C_RESET: &str = "\x1b[0m";
const C_BOLD: &str = "\x1b[1m";
const C_DIM: &str = "\x1b[2m";
const C_CYAN: &str = "\x1b[36m";
const C_GREEN: &str = "\x1b[32m";
const C_YELLOW: &str = "\x1b[33m";
const C_MAGENTA: &str = "\x1b[35m";

fn color_label(labels: &[String]) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!("{C_BOLD}{C_CYAN}:{}{C_RESET}", labels.join("&"))
    }
}

#[allow(dead_code)] // kept for `schema` legacy renderer; reintroduced if needed
fn color_props(props: &std::collections::BTreeMap<String, &str>) -> String {
    if props.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = props
        .iter()
        .map(|(k, t)| format!("{k}: {C_GREEN}{t}{C_RESET}"))
        .collect();
    format!(" {{{}}}", parts.join(", "))
}

fn color_props_with_star(
    props: &std::collections::BTreeMap<String, &str>,
    has_opt: bool,
) -> String {
    if props.is_empty() && !has_opt {
        return String::new();
    }
    let mut parts: Vec<String> = props
        .iter()
        .map(|(k, t)| format!("{k}: {C_GREEN}{t}{C_RESET}"))
        .collect();
    if has_opt {
        parts.push(format!("{C_MAGENTA}*{C_RESET}"));
    }
    format!(" {{{}}}", parts.join(", "))
}

fn color_node(labels: &[String], props_str: &str) -> String {
    format!("({}{props_str})", color_label(labels))
}

fn color_arrow(labels: &[String], props_str: &str, directed: bool) -> String {
    let label_str = color_label(labels);
    if directed {
        format!("{C_YELLOW}-[{C_RESET}{label_str}{props_str}{C_YELLOW}]->{C_RESET}")
    } else {
        format!("{C_YELLOW}~[{C_RESET}{label_str}{props_str}{C_YELLOW}]~{C_RESET}")
    }
}

fn color_count(count: usize, kind: &str) -> String {
    format!("{C_DIM}({count} {kind}){C_RESET}")
}

/// A node type: label combination + property name→type map.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct NodeType {
    labels: Vec<String>,                                     // sorted
    props: std::collections::BTreeMap<String, &'static str>, // prop_name → "str"|"int"|"bool"
}

/// An edge type: label + props + src/tgt node types + directed?
#[allow(dead_code)] // kept for `schema` legacy renderer; reintroduced if needed
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct EdgeType {
    labels: Vec<String>,
    props: std::collections::BTreeMap<String, &'static str>,
    src: NodeType,
    tgt: NodeType,
    directed: bool,
}

impl NodeType {
    #[allow(dead_code)] // kept for `schema` legacy renderer; reintroduced if needed
    fn from_graph_element(store: &LazyGraphStore, id: u32, is_node: bool) -> Self {
        let lt = if is_node {
            store.node_labels(id)
        } else {
            store.edge_labels(id)
        };
        let mut labels = MemoryGraphStore::label_strings(&lt);
        labels.sort();

        let raw_props = if is_node {
            store.node_props(id)
        } else {
            store.edge_props(id)
        };
        let mut props = std::collections::BTreeMap::new();
        for (k, v) in &raw_props {
            let t = match v {
                gqlrust::model::value::Value::Null => "null",
                gqlrust::model::value::Value::Int(_) => "int",
                gqlrust::model::value::Value::Float(_) => "float",
                gqlrust::model::value::Value::Str(_) => "str",
                gqlrust::model::value::Value::Bool(_) => "bool",
                gqlrust::model::value::Value::List(_) => "list",
                gqlrust::model::value::Value::Record(_) => "record",
                gqlrust::model::value::Value::Node(_) => "node",
                gqlrust::model::value::Value::Edge(_) => "edge",
                gqlrust::model::value::Value::Path(_) => "path",
            };
            props.insert(k.clone(), t);
        }

        NodeType { labels, props }
    }

    #[allow(dead_code)]
    fn format(&self) -> String {
        let label_part = if self.labels.is_empty() {
            String::new()
        } else {
            format!(":{}", self.labels.join("&"))
        };

        if self.props.is_empty() {
            format!("({label_part})")
        } else {
            let prop_parts: Vec<String> = self
                .props
                .iter()
                .map(|(k, t)| format!("{k}: {t}"))
                .collect();
            format!("({label_part} {{{}}})", prop_parts.join(", "))
        }
    }
}

#[allow(dead_code)] // bare `schema` now aliases `SHOW GRAPH TYPE DEFAULT`; this
                    // legacy renderer is preserved for future re-binding
fn print_schema(store: &LazyGraphStore) {
    use std::collections::{BTreeSet, HashMap};

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
        let mut labels = MemoryGraphStore::label_strings(&lt);
        labels.sort();

        let raw_props = store.edge_props(eid);
        let mut props = std::collections::BTreeMap::new();
        for (k, v) in &raw_props {
            let t = match v {
                gqlrust::model::value::Value::Null => "null",
                gqlrust::model::value::Value::Int(_) => "int",
                gqlrust::model::value::Value::Float(_) => "float",
                gqlrust::model::value::Value::Str(_) => "str",
                gqlrust::model::value::Value::Bool(_) => "bool",
                gqlrust::model::value::Value::List(_) => "list",
                gqlrust::model::value::Value::Record(_) => "record",
                gqlrust::model::value::Value::Node(_) => "node",
                gqlrust::model::value::Value::Edge(_) => "edge",
                gqlrust::model::value::Value::Path(_) => "path",
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
    let mut standalone_nodes: Vec<(&NodeType, &usize)> = node_type_counts
        .iter()
        .filter(|(nt, _)| !endpoint_types.contains(nt))
        .collect();
    standalone_nodes.sort_by_key(|(nt, _)| (*nt).clone());

    if !standalone_nodes.is_empty() {
        println!("{C_BOLD}Node types:{C_RESET}");
        for (nt, count) in &standalone_nodes {
            let node_str = color_node(&nt.labels, &color_props(&nt.props));
            println!("  {node_str} {}", color_count(**count, "nodes"));
        }
    }

    // 5. Print edge types
    if !edge_type_counts.is_empty() {
        if !standalone_nodes.is_empty() {
            println!();
        }
        println!("{C_BOLD}Edge types:{C_RESET}");
        let mut edge_types: Vec<(&EdgeType, &usize)> = edge_type_counts.iter().collect();
        edge_types.sort_by_key(|(et, _)| (*et).clone());

        for (et, count) in &edge_types {
            let src = color_node(&et.src.labels, &color_props(&et.src.props));
            let tgt = color_node(&et.tgt.labels, &color_props(&et.tgt.props));
            let arrow = color_arrow(&et.labels, &color_props(&et.props), et.directed);
            println!("  {src} {arrow} {tgt} {}", color_count(**count, "edges"));
        }
    }

    // 6. Print summary
    println!();
    println!(
        "{C_DIM}{} node types, {} edge types ({} nodes, {} edges){C_RESET}",
        node_type_counts.len(),
        edge_type_counts.len(),
        store.node_count(),
        store.edge_count()
    );
}

/// Simplified schema: group by labels, intersect properties across all instances.
/// Properties common to all instances of a label combo are shown; optional ones become `*`.
fn print_schema_simple(store: &LazyGraphStore) {
    use std::collections::{BTreeMap, HashMap};

    // --- Node types: group by labels, intersect props ---

    // For each label combo: collect (intersection of prop names with consistent types, total count)
    #[allow(clippy::type_complexity)]
    let mut node_groups: HashMap<Vec<String>, (Option<BTreeMap<String, &'static str>>, usize)> =
        HashMap::new();

    for nid in 0..store.node_count() {
        let mut labels = MemoryGraphStore::label_strings(&store.node_labels(nid));
        labels.sort();

        let raw_props = store.node_props(nid);
        let mut prop_types: BTreeMap<String, &'static str> = BTreeMap::new();
        for (k, v) in &raw_props {
            let t = match v {
                gqlrust::model::value::Value::Null => "null",
                gqlrust::model::value::Value::Int(_) => "int",
                gqlrust::model::value::Value::Float(_) => "float",
                gqlrust::model::value::Value::Str(_) => "str",
                gqlrust::model::value::Value::Bool(_) => "bool",
                gqlrust::model::value::Value::List(_) => "list",
                gqlrust::model::value::Value::Record(_) => "record",
                gqlrust::model::value::Value::Node(_) => "node",
                gqlrust::model::value::Value::Edge(_) => "edge",
                gqlrust::model::value::Value::Path(_) => "path",
            };
            prop_types.insert(k.clone(), t);
        }

        let entry = node_groups.entry(labels.clone()).or_insert((None, 0));
        entry.1 += 1;
        match &mut entry.0 {
            None => entry.0 = Some(prop_types),
            Some(common) => {
                // Intersect: keep only props present in both with same type
                let keys: Vec<String> = common.keys().cloned().collect();
                for k in keys {
                    match prop_types.get(&k) {
                        Some(t) if *t == common[&k] => {} // same type, keep
                        _ => {
                            common.remove(&k);
                        } // missing or different type, drop
                    }
                }
            }
        }
    }

    // Check if there are optional props (i.e., the full schema had more than the intersection)
    // We track this by checking if any node had props beyond the common set
    let mut node_has_optional: HashMap<Vec<String>, bool> = HashMap::new();
    for nid in 0..store.node_count() {
        let mut labels = MemoryGraphStore::label_strings(&store.node_labels(nid));
        labels.sort();
        let raw_props = store.node_props(nid);
        let common = node_groups[&labels].0.as_ref().unwrap();
        if raw_props.len() > common.len() {
            node_has_optional.insert(labels, true);
        }
    }

    // --- Edge types: group by (edge_labels, src_labels, tgt_labels, directed), intersect props ---

    #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct SimpleEdgeKey {
        edge_labels: Vec<String>,
        src_labels: Vec<String>,
        tgt_labels: Vec<String>,
        directed: bool,
    }

    // edge_key -> (intersection of prop name+types across instances, total count)
    #[allow(clippy::type_complexity)]
    let mut edge_groups: HashMap<
        SimpleEdgeKey,
        (Option<BTreeMap<String, &'static str>>, usize),
    > = HashMap::new();
    let mut edge_has_optional: HashMap<SimpleEdgeKey, bool> = HashMap::new();

    for eid in 0..store.edge_count() {
        let mut edge_labels = MemoryGraphStore::label_strings(&store.edge_labels(eid));
        edge_labels.sort();
        let mut src_labels = MemoryGraphStore::label_strings(&store.node_labels(store.src(eid)));
        src_labels.sort();
        let mut tgt_labels = MemoryGraphStore::label_strings(&store.node_labels(store.tgt(eid)));
        tgt_labels.sort();
        let directed = store.is_directed(eid);

        let raw_props = store.edge_props(eid);
        let mut prop_types: BTreeMap<String, &'static str> = BTreeMap::new();
        for (k, v) in &raw_props {
            let t = match v {
                gqlrust::model::value::Value::Null => "null",
                gqlrust::model::value::Value::Int(_) => "int",
                gqlrust::model::value::Value::Float(_) => "float",
                gqlrust::model::value::Value::Str(_) => "str",
                gqlrust::model::value::Value::Bool(_) => "bool",
                gqlrust::model::value::Value::List(_) => "list",
                gqlrust::model::value::Value::Record(_) => "record",
                gqlrust::model::value::Value::Node(_) => "node",
                gqlrust::model::value::Value::Edge(_) => "edge",
                gqlrust::model::value::Value::Path(_) => "path",
            };
            prop_types.insert(k.clone(), t);
        }

        let key = SimpleEdgeKey {
            edge_labels,
            src_labels,
            tgt_labels,
            directed,
        };
        let entry = edge_groups.entry(key.clone()).or_insert((None, 0));
        entry.1 += 1;
        match &mut entry.0 {
            None => entry.0 = Some(prop_types),
            Some(common) => {
                let keys: Vec<String> = common.keys().cloned().collect();
                for k in keys {
                    match prop_types.get(&k) {
                        Some(t) if *t == common[&k] => {}
                        _ => {
                            common.remove(&k);
                        }
                    }
                }
            }
        }
    }

    for eid in 0..store.edge_count() {
        let mut edge_labels = MemoryGraphStore::label_strings(&store.edge_labels(eid));
        edge_labels.sort();
        let mut src_labels = MemoryGraphStore::label_strings(&store.node_labels(store.src(eid)));
        src_labels.sort();
        let mut tgt_labels = MemoryGraphStore::label_strings(&store.node_labels(store.tgt(eid)));
        tgt_labels.sort();
        let directed = store.is_directed(eid);
        let key = SimpleEdgeKey {
            edge_labels,
            src_labels,
            tgt_labels,
            directed,
        };
        let raw_props = store.edge_props(eid);
        let common = edge_groups[&key].0.as_ref().unwrap();
        if raw_props.len() > common.len() {
            edge_has_optional.insert(key, true);
        }
    }

    // --- Format helpers ---

    let format_simple_node =
        |labels: &[String], common: &BTreeMap<String, &str>, has_opt: bool| -> String {
            color_node(labels, &color_props_with_star(common, has_opt))
        };

    // --- Print ---

    // Always list every node type, even when it appears as an edge endpoint.
    // The redundancy with the endpoint descriptors below is intentional: it
    // gives parity with `.schema` (which lists all node types unconditionally)
    // and with the DEFAULT catalog entry, which is built from the same source.
    let mut all_nodes: Vec<_> = node_groups.iter().collect();
    all_nodes.sort_by_key(|(labels, _)| (*labels).clone());

    if !all_nodes.is_empty() {
        println!("{C_BOLD}Node types:{C_RESET}");
        for (labels, (common, count)) in &all_nodes {
            let has_opt = node_has_optional.get(*labels).copied().unwrap_or(false);
            println!(
                "  {} {}",
                format_simple_node(labels, common.as_ref().unwrap(), has_opt),
                color_count(*count, "nodes")
            );
        }
    }

    // Edge types
    if !edge_groups.is_empty() {
        if !all_nodes.is_empty() {
            println!();
        }
        println!("{C_BOLD}Edge types:{C_RESET}");

        let mut edges: Vec<_> = edge_groups.iter().collect();
        edges.sort_by_key(|(k, _)| (*k).clone());

        for (key, (common, count)) in &edges {
            let src_common = node_groups
                .get(&key.src_labels)
                .and_then(|(c, _)| c.as_ref())
                .cloned()
                .unwrap_or_default();
            let src_opt = node_has_optional
                .get(&key.src_labels)
                .copied()
                .unwrap_or(false);
            let tgt_common = node_groups
                .get(&key.tgt_labels)
                .and_then(|(c, _)| c.as_ref())
                .cloned()
                .unwrap_or_default();
            let tgt_opt = node_has_optional
                .get(&key.tgt_labels)
                .copied()
                .unwrap_or(false);

            let src_str = format_simple_node(&key.src_labels, &src_common, src_opt);
            let tgt_str = format_simple_node(&key.tgt_labels, &tgt_common, tgt_opt);

            let edge_common = common.as_ref().unwrap();
            let e_has_opt = edge_has_optional.get(key).copied().unwrap_or(false);
            let arrow = color_arrow(
                &key.edge_labels,
                &color_props_with_star(edge_common, e_has_opt),
                key.directed,
            );

            println!(
                "  {src_str} {arrow} {tgt_str} {}",
                color_count(*count, "edges")
            );
        }
    }

    let node_type_count = node_groups.len();
    let edge_type_count = edge_groups.len();
    println!();
    println!(
        "{C_DIM}{} node types, {} edge types ({} nodes, {} edges){C_RESET}",
        node_type_count,
        edge_type_count,
        store.node_count(),
        store.edge_count()
    );
}

// ===== Catalog DDL handlers =====

fn build_schema_from_body(body: &[TypeElement]) -> Schema {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for el in body {
        match el {
            TypeElement::Node(vt) => nodes.push(vt.clone()),
            TypeElement::Edge(vt) => edges.push(vt.clone()),
        }
    }
    Schema::from_parts(nodes, edges)
}

fn handle_create(store: &LazyGraphStore, name: &str, body: &[TypeElement]) {
    let schema = build_schema_from_body(body);
    let node_count = schema.nodes.len();
    let edge_count = schema.edges.len();
    {
        let mut cat = store.catalog_mut();
        if let Err(e) = cat.register(name.to_string(), schema) {
            eprintln!("error: {e}");
            return;
        }
    }
    if let Err(e) = store.save_catalog() {
        eprintln!("error: failed to persist catalog: {e}");
        return;
    }
    println!("GRAPH TYPE '{name}' created ({node_count} node types, {edge_count} edge types).");
}

fn handle_use(store: &LazyGraphStore, name: &str, refresh_default: bool) {
    if refresh_default {
        let schema = infer_simple_schema(store);
        let node_count = schema.nodes.len();
        let edge_count = schema.edges.len();
        store.catalog_mut().install_default(schema);
        if let Err(e) = store.save_catalog() {
            eprintln!("error: failed to persist catalog: {e}");
            return;
        }
        println!(
            "GRAPH TYPE 'DEFAULT' refreshed ({node_count} node types, {edge_count} edge types) and activated."
        );
    } else {
        if let Err(e) = store.catalog_mut().set_active(name) {
            eprintln!("error: {e}");
            return;
        }
        if let Err(e) = store.save_catalog() {
            eprintln!("error: failed to persist catalog: {e}");
            return;
        }
        println!("Active GRAPH TYPE: {name}.");
    }
}

fn handle_drop(store: &LazyGraphStore, name: &str) {
    {
        let mut cat = store.catalog_mut();
        if let Err(e) = cat.drop(name) {
            eprintln!("error: {e}");
            return;
        }
    }
    if let Err(e) = store.save_catalog() {
        eprintln!("error: failed to persist catalog: {e}");
        return;
    }
    println!("GRAPH TYPE '{name}' dropped.");
}

fn print_help() {
    println!("REPL meta-commands (sqlite-style, all start with '.'):");
    println!("  .schema             show DEFAULT GRAPH TYPE (alias for SHOW GRAPH TYPE DEFAULT)");
    println!("  .schema simple      grouped by-label renderer of the inferred schema");
    println!("  .graph-types        list all GRAPH TYPEs (alias for SHOW GRAPH TYPES)");
    println!("  .indexes            list secondary indexes (alias for SHOW INDEXES)");
    println!("  .save               persist overlay to the open .gdb (atomic tmp+rename)");
    println!("  .dump-json <path>   write a JSON snapshot (round-trips with import_json)");
    println!("  .dump-gql <path>    write a GQL script that recreates the graph");
    println!("  .help               this message");
    println!("  .quit / .exit       exit the REPL");
    println!();
    println!("Catalog DDL (full ISO statements, terminated by ';' optional in REPL):");
    println!("  CREATE GRAPH TYPE <name> AS {{ <type-elements> }}");
    println!("  USE GRAPH TYPE <name>            (USE GRAPH TYPE DEFAULT re-infers from data)");
    println!("  DROP GRAPH TYPE <name>           (DEFAULT cannot be dropped)");
    println!("  SHOW GRAPH TYPES");
    println!("  SHOW GRAPH TYPE <name>");
    println!("  SHOW CURRENT GRAPH TYPE");
    println!("  VALIDATE GRAPH TYPE <name>       (walks the graph, O(N+E), result cached)");
    println!();
    println!("Index DDL:");
    println!("  CREATE [HASH | BTREE] INDEX [<name>] ON :Label(prop) [USING HASH | BTREE]");
    println!("  DROP INDEX <name>                (auto-inferred indexes cannot be dropped)");
    println!("  SHOW INDEXES                     (kind, label, prop, entry count)");
    println!();
    println!("Queries: MATCH? <pattern> [WHERE <expr>] [RETURN <items>]");
    println!("         OPTIONAL MATCH ...        left-join semantics");
    println!("         WHERE x.attr IS NULL      / IS NOT NULL");
}

fn print_graph_types(store: &LazyGraphStore) {
    let cat = store.catalog();
    let entries = cat.list();
    if entries.is_empty() {
        println!("(no graph types)");
        return;
    }
    for (name, is_active) in entries {
        if is_active {
            println!("* {name}");
        } else {
            println!("  {name}");
        }
    }
}

fn handle_show(store: &LazyGraphStore, name: &str) {
    if name.eq_ignore_ascii_case("DEFAULT") {
        // Lazy refresh: pull the current schema from the live store
        // before pretty-printing. No-op when nothing has changed.
        store.refresh_default_if_dirty();
    }
    let cat = store.catalog();
    let lookup_key = if name.eq_ignore_ascii_case("DEFAULT") {
        "DEFAULT"
    } else {
        name
    };
    match cat.types.get(lookup_key) {
        Some(schema) => {
            let active_marker = if cat.active_name() == Some(lookup_key) {
                format!(" {C_DIM}(active){C_RESET}")
            } else {
                String::new()
            };
            println!("{C_BOLD}GRAPH TYPE {lookup_key}{C_RESET}{active_marker}");
            print_schema_colored(schema);
            if let Some(v) = cat.validation_for(lookup_key) {
                print_validation_summary(v);
            }
        }
        None => eprintln!("error: graph type '{name}' not found"),
    }
}

fn handle_show_current(store: &LazyGraphStore) {
    let cat = store.catalog();
    match cat.active_name() {
        None => println!("CURRENT GRAPH TYPE: (none)"),
        Some(name) => {
            // Lookup uses the canonical key — `active` already holds it.
            match cat.types.get(name) {
                Some(schema) => {
                    println!("{C_BOLD}CURRENT GRAPH TYPE: {name}{C_RESET}");
                    print_schema_colored(schema);
                    if let Some(v) = cat.validation_for(name) {
                        print_validation_summary(v);
                    }
                }
                None => eprintln!("error: active graph type '{name}' is missing from the catalog"),
            }
        }
    }
}

// ---- Colored schema renderer ----
//
// Mirrors `typing::format::format_schema` but emits ANSI codes consistent
// with the rest of the REPL output (labels bold cyan, type names green,
// arrows yellow, open-record marker magenta).

fn print_schema_colored(schema: &Schema) {
    if !schema.nodes.is_empty() {
        println!("{C_BOLD}Node types:{C_RESET}");
        for vt in schema.nodes.iter() {
            println!("    {}", color_variable(vt));
        }
    }
    if !schema.edges.is_empty() {
        if !schema.nodes.is_empty() {
            println!();
        }
        println!("{C_BOLD}Edge types:{C_RESET}");
        for vt in schema.edges.iter() {
            println!("    {}", color_variable(vt));
        }
    }
    if schema.nodes.is_empty() && schema.edges.is_empty() {
        println!("{C_DIM}(empty schema){C_RESET}");
    }
}

fn color_variable(vt: &VariableType) -> String {
    match vt {
        VariableType::Node(d) => color_node_descriptor(d),
        VariableType::EdgeDirectional { desc, left, right } => format!(
            "{}{}{}",
            color_endpoint(left),
            color_edge_arrow(desc, true),
            color_endpoint(right),
        ),
        VariableType::EdgeNonDirectional { desc, left, right } => format!(
            "{}{}{}",
            color_endpoint(left),
            color_edge_arrow(desc, false),
            color_endpoint(right),
        ),
        VariableType::Union(a, b) => {
            format!("({}) | ({})", color_variable(a), color_variable(b))
        }
        VariableType::Group(t) => format!("group<{}>", color_variable(t)),
        VariableType::Null => "Null".to_string(),
        VariableType::Path => format!("{C_GREEN}PATH{C_RESET}"),
        VariableType::Zero => "⊥".to_string(),
    }
}

fn color_endpoint(vt: &VariableType) -> String {
    match vt {
        VariableType::Node(d) => color_node_descriptor(d),
        _ => format!("({})", color_variable(vt)),
    }
}

fn color_node_descriptor(d: &DescriptorType) -> String {
    let label = color_label_type(&d.label);
    let props = color_property_record(&d.props);
    match (label.is_empty(), props.is_empty()) {
        (true, true) => "()".to_string(),
        (false, true) => format!("(:{label})"),
        (true, false) => format!("({props})"),
        (false, false) => format!("(:{label} {props})"),
    }
}

fn color_edge_arrow(d: &DescriptorType, directed: bool) -> String {
    let body = color_edge_descriptor(d);
    let (open, close) = if directed {
        (
            format!("{C_YELLOW}-[{C_RESET}"),
            format!("{C_YELLOW}]->{C_RESET}"),
        )
    } else {
        (
            format!("{C_YELLOW}~[{C_RESET}"),
            format!("{C_YELLOW}]~{C_RESET}"),
        )
    };
    format!("{open}{body}{close}")
}

fn color_edge_descriptor(d: &DescriptorType) -> String {
    let label = color_label_type(&d.label);
    let props = color_property_record(&d.props);
    match (label.is_empty(), props.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(":{label}"),
        (true, false) => props,
        (false, false) => format!(":{label} {props}"),
    }
}

/// Render a `LabelType` without a leading colon. Empty for `Star`/`Top`.
/// Composite labels (And/Or/Neg) are colored token-by-token.
fn color_label_type(lt: &LabelType) -> String {
    match lt {
        LabelType::Label(s) => format!("{C_BOLD}{C_CYAN}{s}{C_RESET}"),
        LabelType::Star | LabelType::Top => String::new(),
        LabelType::Empty => format!("{C_DIM}ε{C_RESET}"),
        LabelType::And(a, b) => {
            format!("{}&{}", color_label_inner(a), color_label_inner(b))
        }
        LabelType::Or(a, b) => {
            format!("{}|{}", color_label_inner(a), color_label_inner(b))
        }
        LabelType::Neg(inner) => format!("!{}", color_label_inner(inner)),
    }
}

fn color_label_inner(lt: &LabelType) -> String {
    match lt {
        LabelType::And(_, _) | LabelType::Or(_, _) => format!("({})", color_label_type(lt)),
        _ => color_label_type(lt),
    }
}

fn color_property_record(pt: &PropertyType) -> String {
    match pt {
        PropertyType::Open(m) if m.is_empty() => String::new(),
        PropertyType::Closed(m) if m.is_empty() => "{}".to_string(),
        PropertyType::Open(m) => format!("{{{}, {C_MAGENTA}*{C_RESET}}}", color_field_map(m)),
        PropertyType::Closed(m) => format!("{{{}}}", color_field_map(m)),
        PropertyType::Zero => format!("{C_DIM}⊥{C_RESET}"),
    }
}

fn color_field_map(m: &std::collections::BTreeMap<String, SimpleType>) -> String {
    m.iter()
        .map(|(k, v)| format!("{k} {}", color_simple_type(v)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn color_simple_type(t: &SimpleType) -> String {
    match t {
        SimpleType::Z => format!("{C_GREEN}INT{C_RESET}"),
        SimpleType::F => format!("{C_GREEN}FLOAT{C_RESET}"),
        SimpleType::B => format!("{C_GREEN}BOOL{C_RESET}"),
        SimpleType::S => format!("{C_GREEN}STRING{C_RESET}"),
        SimpleType::Star => format!("{C_MAGENTA}ANY{C_RESET}"),
        SimpleType::Zero => format!("{C_DIM}⊥{C_RESET}"),
        SimpleType::Union(a, b) => format!(
            "{} | {}",
            color_simple_type_atom(a),
            color_simple_type_atom(b)
        ),
        SimpleType::List(inner) => format!("{C_GREEN}LIST{C_RESET}<{}>", color_simple_type(inner)),
        SimpleType::Group(inner) => format!("group<{}>", color_simple_type(inner)),
        SimpleType::Record(fields) => format!("{{{}}}", color_field_map(fields)),
        SimpleType::Node => format!("{C_GREEN}NODE{C_RESET}"),
        SimpleType::Edge => format!("{C_GREEN}EDGE{C_RESET}"),
        SimpleType::Path => format!("{C_GREEN}PATH{C_RESET}"),
    }
}

fn color_simple_type_atom(t: &SimpleType) -> String {
    match t {
        SimpleType::Union(_, _) => format!("({})", color_simple_type(t)),
        _ => color_simple_type(t),
    }
}

fn handle_create_index(
    store: &LazyGraphStore,
    name: Option<String>,
    label: &str,
    prop: &str,
    kind: gqlrust::syntax::statement::IndexKindStmt,
) {
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
    let start = Instant::now();
    let result = store
        .secondary_indexes_mut()
        .build_declared(store, final_name, label, prop, store_kind);
    let elapsed = start.elapsed();
    match result {
        Ok(spec) => {
            let kind_str = match spec.kind {
                IndexKind::Hash => "HASH",
                IndexKind::BTree => "BTREE",
            };
            println!(
                "INDEX '{}' created ({} on (:{} {{{}}}), {} entries) in {:.3}s.",
                spec.name,
                kind_str,
                spec.label,
                spec.prop,
                spec.entries,
                elapsed.as_secs_f64(),
            );
        }
        Err(e) => eprintln!("error: {e}"),
    }
}

fn handle_drop_index(store: &LazyGraphStore, name: &str) {
    let dropped = store.secondary_indexes_mut().drop_named(name);
    if dropped {
        println!("INDEX '{name}' dropped.");
    } else {
        eprintln!("error: index '{name}' not found");
    }
}

fn print_indexes(store: &LazyGraphStore) {
    let idx = store.secondary_indexes();
    let specs = idx.list();
    if specs.is_empty() {
        println!("(no secondary indexes)");
        return;
    }
    use gqlrust::store::secondary_index::IndexKind;
    println!(
        "{:<40}  {:<6}  {:<22}  {:<14}  origin",
        "name", "kind", "label", "entries"
    );
    println!("{}", "-".repeat(100));
    for spec in specs {
        let kind = match spec.kind {
            IndexKind::Hash => "HASH",
            IndexKind::BTree => "BTREE",
        };
        let origin = if spec.auto { "auto" } else { "declared" };
        println!(
            "{:<40}  {:<6}  :{:<21}  {:>14}  {origin}",
            spec.name,
            kind,
            format!("{} {{{}}}", spec.label, spec.prop),
            spec.entries,
        );
    }
}

fn handle_validate(store: &LazyGraphStore, name: &str) {
    let lookup_key = if name.eq_ignore_ascii_case("DEFAULT") {
        "DEFAULT"
    } else {
        name
    };
    let schema = match store.catalog().types.get(lookup_key).cloned() {
        Some(s) => s,
        None => {
            eprintln!("error: graph type '{name}' not found");
            return;
        }
    };
    let start = Instant::now();
    let report = validate_against_data(store, &schema);
    let elapsed = start.elapsed();

    print_validation_report(&report, lookup_key, elapsed);

    let status = ValidationStatus {
        against_node_count: report.nodes_checked,
        against_edge_count: report.edges_checked,
        violations: report.total_violations(),
        validated_at_unix: now_unix(),
    };
    store.catalog_mut().record_validation(lookup_key, status);
    if let Err(e) = store.save_catalog() {
        eprintln!("warning: failed to persist validation result: {e}");
    }
}

fn print_validation_report(
    report: &ValidationReport,
    type_name: &str,
    elapsed: std::time::Duration,
) {
    println!(
        "Validated {} nodes, {} edges against GRAPH TYPE '{type_name}' in {:.3}s",
        report.nodes_checked,
        report.edges_checked,
        elapsed.as_secs_f64()
    );
    if report.ok() {
        println!("  OK: every element fits the schema.");
        return;
    }
    println!(
        "  Violations: {} node(s), {} edge(s)",
        report.node_violations, report.edge_violations
    );
    if !report.samples.is_empty() {
        println!("  Samples:");
        for v in &report.samples {
            let kind = match v.kind {
                ElementKind::Node => "node",
                ElementKind::Edge => "edge",
            };
            let label_part = if v.labels.is_empty() {
                "()".to_string()
            } else {
                format!(":{}", v.labels.join("&"))
            };
            let prop_part = if v.props.is_empty() {
                String::new()
            } else {
                let parts: Vec<String> = v.props.iter().map(|(k, t)| format!("{k}: {t}")).collect();
                format!(" {{{}}}", parts.join(", "))
            };
            println!("    {kind} {} ({}{}) ", v.id, label_part, prop_part);
        }
    }
}

fn print_validation_summary(v: &ValidationStatus) {
    let verdict = if v.violations == 0 {
        "OK".to_string()
    } else {
        format!("{} violation(s)", v.violations)
    };
    println!(
        "Last validated: {verdict} (against {} nodes, {} edges)",
        v.against_node_count, v.against_edge_count
    );
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
