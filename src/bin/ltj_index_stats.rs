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

    const ORDER_NAMES: [&str; 6] = ["SPO", "SOP", "POS", "PSO", "OSP", "OPS"];

    #[allow(clippy::type_complexity)]
    let mut results: Vec<(&str, f64, usize, usize, [usize; 6], usize)> = Vec::new();
    for (name, env) in [("array", None), ("compact", Some("1"))] {
        match env {
            Some(v) => std::env::set_var("FROGQL_LTJ_COMPACT", v),
            None => std::env::remove_var("FROGQL_LTJ_COMPACT"),
        }
        let t0 = Instant::now();
        let idx = TripleIndex::from_graph(&store);
        let secs = t0.elapsed().as_secs_f64();
        let (per, shared) = idx.heap_breakdown();
        results.push((name, secs, idx.heap_bytes(), idx.len(), per, shared));
    }
    std::env::remove_var("FROGQL_LTJ_COMPACT");

    println!("index    build_s   heap_MiB   triples");
    for (name, secs, bytes, len, _, _) in &results {
        println!("{name:<8} {secs:>7.2}   {:>8.1}   {len}", mib(*bytes));
    }
    let (a, c) = (results[0].2, results[1].2);
    println!("compact/array size ratio: {:.2}×", a as f64 / c as f64);

    // Per-ordering split. In array mode each ordering is a full copy of the
    // triples, so the six are identical by construction; in compact mode
    // each trie is a distinct LOUDS structure and the eid side table is
    // shared, so the six differ and do not sum to the total.
    println!();
    println!("per-ordering heap (MiB)");
    print!("{:<8}", "");
    for n in ORDER_NAMES {
        print!("{n:>9}");
    }
    println!("{:>12}", "shared");
    for (name, _, _, len, per, shared) in &results {
        print!("{name:<8}");
        for b in per {
            print!("{:>9.1}", mib(*b));
        }
        println!("{:>12.1}", mib(*shared));
        let bytes_per_triple = |b: usize| b as f64 / *len as f64;
        print!("{:<8}", "  B/triple");
        for b in per {
            print!("{:>9.1}", bytes_per_triple(*b));
        }
        println!("{:>12.1}", bytes_per_triple(*shared));
    }
}
