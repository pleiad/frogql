//! Diagnostic: build the LTJ TripleIndex in both representations (array vs
//! compact CLTJ, issue #66) against a .gdb and report build time and index
//! heap footprint. The store itself dominates RSS, so `heap_bytes()` is the
//! honest per-repr comparison.
//!
//! Usage: ltj_index_stats <db.gdb>

use std::time::Instant;

use frogql::runtime::ltj::triple_index::TripleIndex;
use frogql::store::lazy::LazyGraphStore;

fn main() {
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: ltj_index_stats <db.gdb>");
            std::process::exit(2);
        }
    };
    let store = LazyGraphStore::open(std::path::Path::new(&path)).unwrap_or_else(|e| {
        eprintln!("failed to open {path}: {e}");
        std::process::exit(1);
    });

    let mib = |b: usize| b as f64 / (1024.0 * 1024.0);

    let mut results: Vec<(&str, f64, usize, usize)> = Vec::new();
    for (name, env) in [("array", None), ("compact", Some("1"))] {
        match env {
            Some(v) => std::env::set_var("GQLITE_LTJ_COMPACT", v),
            None => std::env::remove_var("GQLITE_LTJ_COMPACT"),
        }
        let t0 = Instant::now();
        let idx = TripleIndex::from_graph(&store);
        let secs = t0.elapsed().as_secs_f64();
        results.push((name, secs, idx.heap_bytes(), idx.len()));
    }
    std::env::remove_var("GQLITE_LTJ_COMPACT");

    println!("index    build_s   heap_MiB   triples");
    for (name, secs, bytes, len) in &results {
        println!("{name:<8} {secs:>7.2}   {:>8.1}   {len}", mib(*bytes));
    }
    let (a, c) = (results[0].2, results[1].2);
    println!("compact/array size ratio: {:.2}×", a as f64 / c as f64);
}
