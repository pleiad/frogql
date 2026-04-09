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
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
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
                for row in ir.rows.iter().take(20) {
                    println!("{}", row);
                }
                if ir.rows.len() > 20 {
                    println!("... ({} more rows)", ir.rows.len() - 20);
                }
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

fn print_schema(store: &LazyGraphStore) {
    let mut label_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for nid in 0..store.node_count() {
        let lt = store.node_labels(nid);
        for l in Graph::label_strings(&lt) {
            *label_counts.entry(l).or_default() += 1;
        }
    }
    println!("Node labels:");
    for (label, count) in &label_counts {
        println!("  :{label} ({count} nodes)");
    }

    let mut edge_label_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for eid in 0..store.edge_count() {
        let lt = store.edge_labels(eid);
        for l in Graph::label_strings(&lt) {
            *edge_label_counts.entry(l).or_default() += 1;
        }
    }
    println!("Edge labels:");
    for (label, count) in &edge_label_counts {
        println!("  :{label} ({count} edges)");
    }
}
