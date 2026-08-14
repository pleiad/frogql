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

    // The store's own resident cost, component by component. Reported
    // alongside the index because the two are what an open database is:
    // quoting one without the other has repeatedly produced numbers that
    // do not add up to the measured RSS.
    let report = store.heap_report();
    let store_total: usize = report.iter().map(|(_, b)| b).sum();
    println!("store heap (MiB)");
    for (name, bytes) in &report {
        if *bytes > 0 {
            println!("  {name:<26} {:>8.1}", mib(*bytes));
        }
    }
    println!("  {:<26} {:>8.1}", "TOTAL", mib(store_total));
    println!();

    // The string table is usually the biggest single store component, and
    // it holds four different populations. Attributing it matters: element
    // names are one per node and edge and unavoidable, while string
    // property values are the part a graph without them simply does not
    // pay.
    {
        let mut count = 0usize;
        let mut content = 0usize;
        let mut longest = 0usize;
        for st in store.iter_strings() {
            count += 1;
            content += st.len();
            longest = longest.max(st.len());
        }
        let elements = store.node_count() as usize + store.edge_count() as usize;
        println!("string table");
        println!("  {:<26} {:>8}", "entries", count);
        println!("  {:<26} {:>8}", "nodes + edges", elements);
        println!(
            "  {:<26} {:>8.1}",
            "avg bytes/entry",
            content as f64 / count.max(1) as f64
        );
        println!("  {:<26} {:>8}", "longest entry (bytes)", longest);
        println!("  {:<26} {:>8.1}", "content MiB", mib(content));

        // Where the bytes sit by entry length. The table mixes a handful
        // of labels and property keys (short, and read on every element
        // during matching) with every distinct string property value
        // (long, and read only when a predicate or projection touches
        // it), so the histogram is what separates them.
        const EDGES: [usize; 7] = [8, 16, 32, 64, 128, 512, usize::MAX];
        const NAMES: [&str; 7] = ["1-8", "9-16", "17-32", "33-64", "65-128", "129-512", "513+"];
        let mut counts = [0usize; 7];
        let mut bytes = [0usize; 7];
        let mut samples: [Vec<String>; 7] = Default::default();
        for st in store.iter_strings() {
            let b = EDGES.iter().position(|&e| st.len() <= e).unwrap_or(6);
            counts[b] += 1;
            bytes[b] += st.len();
            if samples[b].len() < 2 {
                let short: String = st.chars().take(46).collect();
                samples[b].push(short);
            }
        }
        println!();
        println!(
            "  {:<10} {:>10} {:>12} {:>10}",
            "bytes", "entries", "content MiB", "% content"
        );
        for i in 0..7 {
            if counts[i] == 0 {
                continue;
            }
            println!(
                "  {:<10} {:>10} {:>12.1} {:>9.1}%",
                NAMES[i],
                counts[i],
                mib(bytes[i]),
                100.0 * bytes[i] as f64 / content.max(1) as f64
            );
            for ex in &samples[i] {
                println!("  {:<10} {:?}", "", ex);
            }
        }
        println!(
            "  {:<26} {:>8.1}",
            "String headers MiB",
            mib(count * std::mem::size_of::<String>())
        );
        println!();
    }

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
