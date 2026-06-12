//! `dml_bench` — DML micro-benchmark (ISWC review R1.3 / R2.3).
//!
//! Measures the two costs reviewers asked about for the overlay-based
//! data-modification design:
//!
//!   1. Mutation throughput — INSERT / SET / DETACH DELETE statements
//!      per second against a loaded LDBC graph (mutations stage in the
//!      in-RAM overlay; the on-disk .gdb is never touched — no .save).
//!   2. Time-to-first-query-after-DML — every successful DML
//!      invalidates the cached LTJ TripleIndex (and EXISTS memo), so
//!      the next query pays a rebuild against base+overlay. This is
//!      the documented trade-off of the overlay design; here it is
//!      quantified instead of described.
//!
//! Usage:
//!   dml_bench <db.gdb> [--n 1000] [--probe-iters 5]
//!
//! Output: human-readable summary on stderr + `metric;value;unit` CSV
//! on stdout (machine-readable for the paper table).

use std::env;
use std::process::exit;
use std::time::Instant;

use gqlrust::parser::parse_statement;
use gqlrust::runtime::dm::run_dm;
use gqlrust::runtime::engine::Runtime;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::statement::Statement;

fn fail(msg: &str) -> ! {
    eprintln!("dml_bench: {msg}");
    exit(1);
}

/// Parse + run one DML statement, mirroring the REPL dispatch
/// (no GRAPH TYPE active → no G2000 validation). Returns wall seconds.
fn run_dml(store: &LazyGraphStore, rt: &Runtime<LazyGraphStore>, text: &str) -> f64 {
    let stmt = parse_statement(text).unwrap_or_else(|e| fail(&format!("parse {text:?}: {e}")));
    let dm = match stmt {
        Statement::DataModification(dm) => dm,
        _ => fail(&format!("not a DML statement: {text:?}")),
    };
    let t = Instant::now();
    run_dm(store, &dm, None).unwrap_or_else(|e| fail(&format!("run {text:?}: {e}")));
    let dt = t.elapsed().as_secs_f64();
    // The REPL invalidates runtime caches after every successful DML so
    // the next query sees post-mutation state. Faithfully reproduce it:
    // the rebuild cost lands on the next query, which is exactly what
    // the post-DML probe measures.
    rt.invalidate_caches();
    dt
}

/// Run the probe query once, return wall seconds.
fn run_probe(rt: &Runtime<LazyGraphStore>, query: &gqlrust::syntax::query::Query) -> f64 {
    let t = Instant::now();
    let _ = rt.run_query(query, 10);
    t.elapsed().as_secs_f64()
}

fn median(xs: &mut [f64]) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if xs.is_empty() {
        return f64::NAN;
    }
    xs[xs.len() / 2]
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args[1] == "-h" || args[1] == "--help" {
        eprintln!("Usage: {} <db.gdb> [--n 1000] [--probe-iters 5]", args[0]);
        exit(if args.len() < 2 { 1 } else { 0 });
    }
    let db_path = &args[1];
    let mut n: usize = 1000;
    let mut probe_iters: usize = 5;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--n" => {
                n = args
                    .get(i + 1)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| fail("--n needs an integer"));
                i += 2;
            }
            "--probe-iters" => {
                probe_iters = args
                    .get(i + 1)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| fail("--probe-iters needs an integer"));
                i += 2;
            }
            other => fail(&format!("unknown arg: {other}")),
        }
    }

    eprintln!("opening {db_path}...");
    let t_open = Instant::now();
    let store = LazyGraphStore::open_or_create(std::path::Path::new(db_path))
        .unwrap_or_else(|e| fail(&format!("open {db_path}: {e}")));
    let rt = Runtime::new(&store);
    rt.warm_triple_index();
    let open_s = t_open.elapsed().as_secs_f64();
    eprintln!(
        "  opened + warmed TripleIndex in {open_s:.3}s ({} nodes / {} edges)",
        store.node_count(),
        store.edge_count()
    );

    // Probe query: counts a small neighborhood through the TripleIndex,
    // so its first post-DML run pays the index rebuild. Compiled
    // unchecked: the bench graph has no GRAPH TYPE active.
    let probe_text = "MATCH (a:Person)-[:knows]->(b:Person) RETURN COUNT(b) AS c";
    let probe = gqlrust::compile_query_unchecked(probe_text)
        .unwrap_or_else(|e| fail(&format!("compile probe: {e}")));

    // Steady-state probe latency (index warm, no DML in between).
    let mut steady: Vec<f64> = Vec::new();
    let _ = run_probe(&rt, &probe); // absorb first-run noise
    for _ in 0..probe_iters {
        steady.push(run_probe(&rt, &probe));
    }
    let steady_med = median(&mut steady);
    eprintln!("steady-state probe: {:.1} ms median", steady_med * 1e3);

    println!("metric;value;unit");
    println!("open_plus_warm;{open_s:.3};s");
    println!("probe_steady_median;{:.4};s", steady_med);

    // --- 1. INSERT throughput -----------------------------------------
    let t = Instant::now();
    for k in 0..n {
        run_dml(&store, &rt, &format!("INSERT (:DmBench {{bid: {k}}})"));
    }
    let insert_s = t.elapsed().as_secs_f64();
    eprintln!(
        "INSERT × {n}: {insert_s:.3}s total, {:.0} stmts/s",
        n as f64 / insert_s
    );
    println!("insert_total;{insert_s:.3};s");
    println!("insert_throughput;{:.1};stmts_per_s", n as f64 / insert_s);

    // --- 2. Post-DML first-query penalty --------------------------------
    // One DML, then query: first run rebuilds the TripleIndex from
    // base+overlay, second run is steady again. Median over probe_iters.
    let mut first: Vec<f64> = Vec::new();
    let mut second: Vec<f64> = Vec::new();
    for k in 0..probe_iters {
        run_dml(
            &store,
            &rt,
            &format!("INSERT (:DmBench {{bid: {}}})", n + k),
        );
        first.push(run_probe(&rt, &probe));
        second.push(run_probe(&rt, &probe));
    }
    let first_med = median(&mut first);
    let second_med = median(&mut second);
    eprintln!(
        "post-DML probe: first {:.1} ms (incl. TripleIndex rebuild), second {:.1} ms",
        first_med * 1e3,
        second_med * 1e3
    );
    println!("probe_first_after_dml_median;{first_med:.4};s");
    println!("probe_second_after_dml_median;{second_med:.4};s");
    println!(
        "rebuild_penalty_median;{:.4};s",
        (first_med - second_med).max(0.0)
    );

    // --- 3. SET throughput ----------------------------------------------
    let t = Instant::now();
    for k in 0..n {
        run_dml(
            &store,
            &rt,
            &format!("MATCH (x:DmBench {{bid: {k}}}) SET x.v = {k}"),
        );
    }
    let set_s = t.elapsed().as_secs_f64();
    eprintln!(
        "MATCH+SET × {n}: {set_s:.3}s total, {:.0} stmts/s",
        n as f64 / set_s
    );
    println!("set_total;{set_s:.3};s");
    println!("set_throughput;{:.1};stmts_per_s", n as f64 / set_s);

    // --- 4. DETACH DELETE throughput -------------------------------------
    let t = Instant::now();
    for k in 0..n {
        run_dml(
            &store,
            &rt,
            &format!("MATCH (x:DmBench {{bid: {k}}}) DETACH DELETE x"),
        );
    }
    let delete_s = t.elapsed().as_secs_f64();
    eprintln!(
        "MATCH+DETACH DELETE × {n}: {delete_s:.3}s total, {:.0} stmts/s",
        n as f64 / delete_s
    );
    println!("delete_total;{delete_s:.3};s");
    println!("delete_throughput;{:.1};stmts_per_s", n as f64 / delete_s);

    eprintln!("done (overlay only; the .gdb on disk was not modified)");
}
