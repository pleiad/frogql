//! Benchmark runner: loads a .gql database, runs queries from files, measures time.
//!
//! Usage: bench_queries <db.gql> <queries_file> [--limit N] [--timeout SECS]
//!
//! Output format (matching CLTJ): query_id;result_count;elapsed_ns
//! If a query exceeds the timeout, it still reports but appends ";timeout".

use std::env;
use std::fs;
use std::time::{Duration, Instant};

use gqlrust::runtime::engine::Runtime;
use gqlrust::store::lazy::LazyGraphStore;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <db.gql> <queries_file> [--limit N] [--timeout SECS]",
            args[0]
        );
        std::process::exit(1);
    }

    let db_path = &args[1];
    let queries_path = &args[2];

    let mut limit: usize = 0;
    let mut timeout_secs: u64 = 600;

    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                limit = args[i + 1].parse().expect("invalid limit");
                i += 2;
            }
            "--timeout" => {
                timeout_secs = args[i + 1].parse().expect("invalid timeout");
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    let timeout = Duration::from_secs(timeout_secs);

    eprintln!("Loading database: {}", db_path);
    let t0 = Instant::now();
    let store =
        LazyGraphStore::open(std::path::Path::new(db_path)).expect("failed to open database");
    eprintln!(
        "Loaded in {:.2}s ({} nodes, {} edges)",
        t0.elapsed().as_secs_f64(),
        store.node_count(),
        store.edge_count()
    );

    let rt = Runtime::new(&store);

    let query_text = fs::read_to_string(queries_path).expect("cannot read queries file");
    let queries: Vec<&str> = query_text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .collect();

    eprintln!(
        "Running {} queries (limit={}, timeout={}s)...",
        queries.len(),
        limit,
        timeout_secs
    );

    for (qid, query_str) in queries.iter().enumerate() {
        let pattern = match gqlrust::compile(query_str) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  Q{}: parse error: {}", qid, e);
                println!("{};error;0", qid);
                continue;
            }
        };

        let start = Instant::now();
        let result = rt.run_with_limit(&pattern, limit);
        let elapsed = start.elapsed();
        let count = result.rows.len();
        let ns = elapsed.as_nanos();

        if elapsed > timeout {
            println!("{};{};{};timeout", qid, count, ns);
            eprintln!(
                "  Q{}: {} results in {:.3}s (TIMEOUT)",
                qid,
                count,
                elapsed.as_secs_f64()
            );
        } else {
            println!("{};{};{}", qid, count, ns);
            eprintln!(
                "  Q{}: {} results in {:.3}s",
                qid,
                count,
                elapsed.as_secs_f64()
            );
        }
    }
}
