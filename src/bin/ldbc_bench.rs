//! LDBC SNB Interactive workload benchmark runner.
//!
//! Section 6 of the LDBC SNB v1 paper (arXiv:2001.02299) defines the
//! Interactive Complex (IC) reads. This binary runs the subset of those
//! queries that gqlite supports today, measuring runtime per query.
//!
//! Usage:
//!     ldbc_bench <db.gdb> [--iters N] [--limit N]
//!
//! The DB is built from the LDBC SF0.1 CsvBasic dataset via:
//!     gqlite db.gdb --import-ldbc-csv <path-to-extracted-dataset>
//!
//! Per LDBC methodology, each IC is parameterized — we sweep a small
//! curated parameter set per query and report min / median / max wall
//! time across (params × iters) runs.
//!
//! Currently supported: **IC2** (recent messages by friends).
//! Skipped (need features gqlite doesn't yet have):
//!   - IC1 (shortest paths, OPTIONAL MATCH, complex aggregation)
//!   - IC3, IC4, IC5, IC6, IC7, IC8, IC10, IC11, IC12, IC13, IC14
//!     (various combinations of date arithmetic, OPTIONAL, ORDER BY,
//!     transitive paths, aggregate-with-having, etc.)
//!   - IC9 (close to IC2 shape but adds `<= maxDate` filter on Comment)
//!     could be added once date predicates are wired through.
//!
//! Typechecking is skipped — the IC queries are well-formed by design,
//! and bench timing should reflect runtime dominance, not checker work.

use std::env;
use std::path::Path;
use std::time::{Duration, Instant};

use gqlrust::compile_query_unchecked;
use gqlrust::runtime::engine::Runtime;
use gqlrust::store::lazy::LazyGraphStore;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <db.gdb> [--iters N] [--limit N]", args[0]);
        std::process::exit(1);
    }
    let db_path = &args[1];

    let mut iters: usize = 5;
    let mut limit: usize = 20; // matches IC2's `LIMIT 20` in spec
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--iters" => {
                iters = args[i + 1].parse().expect("invalid iters");
                i += 2;
            }
            "--limit" => {
                limit = args[i + 1].parse().expect("invalid limit");
                i += 2;
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(1);
            }
        }
    }

    eprintln!("Loading {db_path}...");
    let t0 = Instant::now();
    let store = LazyGraphStore::open(Path::new(db_path)).expect("open .gdb");
    eprintln!(
        "  loaded {} nodes / {} edges in {:.2}s",
        store.node_count(),
        store.edge_count(),
        t0.elapsed().as_secs_f64()
    );
    let rt = Runtime::new(&store);

    eprintln!("\n=== IC2: Recent messages by friends ===");
    eprintln!("Parameterized over Person.firstName ({iters} iters each, limit={limit})");
    println!("query;param;iter;result_count;elapsed_ns");

    // Curated params: a few reasonably-common firstNames in the SF0.1
    // dataset. Each maps to one or a handful of Persons; together they
    // give enough variation in friend / comment fan-out to exercise
    // both warm-cache and cold-key paths.
    for first_name in &["Mahinda", "Carmen", "Bryn", "Cheng", "Hồ Chí"] {
        let q = format!(
            "MATCH (p: Person)~[:knows]~(friend: Person)\
             <-[:hasCreator]-(c: Comment) \
             WHERE p.firstName = '{first_name}' \
             RETURN c.creationDate"
        );
        let parsed = match compile_query_unchecked(&q) {
            Ok(parsed) => parsed,
            Err(e) => {
                eprintln!("  PARSE ERROR for {first_name}: {e}");
                continue;
            }
        };

        let mut samples: Vec<Duration> = Vec::with_capacity(iters);
        let mut last_count = 0usize;
        for n in 0..iters {
            let start = Instant::now();
            let result = rt.run_query(&parsed, limit);
            let elapsed = start.elapsed();
            samples.push(elapsed);
            last_count = result.row_count();
            println!(
                "IC2;{first_name};{n};{};{}",
                last_count,
                elapsed.as_nanos()
            );
        }
        report("IC2", first_name, &samples, last_count);
    }
}

fn report(query: &str, param: &str, samples: &[Duration], count: usize) {
    let mut sorted: Vec<u128> = samples.iter().map(|d| d.as_nanos()).collect();
    sorted.sort_unstable();
    let n = sorted.len();
    if n == 0 {
        return;
    }
    let min = sorted[0];
    let max = sorted[n - 1];
    let median = sorted[n / 2];
    let mean = sorted.iter().sum::<u128>() / n as u128;
    eprintln!(
        "  {query} param={param:<10} count={count:<5} \
         min={:>8.2}ms  med={:>8.2}ms  mean={:>8.2}ms  max={:>8.2}ms",
        min as f64 / 1e6,
        median as f64 / 1e6,
        mean as f64 / 1e6,
        max as f64 / 1e6,
    );
}
