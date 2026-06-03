// Mini-bench: range-incremental repetition vs per-length legacy.
//
// Builds a synthetic dense graph deterministically (small LCG so the same
// numbers reproduce across runs and machines), then runs
// `(a)-[x]->{1,5}(b)` under both repetition implementations:
//   - legacy   (default): per-length walk in run_repetition_pattern
//   - incremental (env GQLITE_REPEAT_INCREMENTAL=1): single-pass over [lb,ub]
//
// The bench dispatches by setting the env var inside the same process before
// each measured run, so results come from one binary invocation.
//
// Usage:
//   cargo run --release --bin bench_repeat -- [--nodes N] [--avg-deg D] [--lb L] [--ub U] [--iters K]
// Defaults: nodes=30, avg-deg=6, lb=1, ub=5, iters=5.

use std::env;
use std::path::Path;
use std::time::Instant;

use gqlrust::compile;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::runtime::engine::Runtime;
use gqlrust::store::lazy::LazyGraphStore;

#[derive(Clone)]
struct BenchCfg {
    nodes: usize,
    avg_deg: usize,
    lb: usize,
    ub: usize,
    iters: usize,
    gdb: Option<String>,
    query: Option<String>,
}

fn parse_args() -> BenchCfg {
    let mut cfg = BenchCfg {
        nodes: 30,
        avg_deg: 6,
        lb: 1,
        ub: 5,
        iters: 5,
        gdb: None,
        query: None,
    };
    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--nodes" => {
                cfg.nodes = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--avg-deg" => {
                cfg.avg_deg = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--lb" => {
                cfg.lb = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--ub" => {
                cfg.ub = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--iters" => {
                cfg.iters = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--gdb" => {
                cfg.gdb = Some(args[i + 1].clone());
                i += 2;
            }
            "--query" => {
                cfg.query = Some(args[i + 1].clone());
                i += 2;
            }
            other => panic!("unknown arg: {other}"),
        }
    }
    cfg
}

// Tiny LCG for reproducible edge sampling. Constants are the Numerical
// Recipes ones — fine for a bench, not for cryptography.
struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.0 >> 16) as u32
    }
    fn gen_range(&mut self, n: u32) -> u32 {
        self.next_u32() % n
    }
}

fn build_graph_json(nodes: usize, avg_deg: usize) -> String {
    let mut nodes_str = String::from("[");
    for i in 0..nodes {
        if i > 0 {
            nodes_str.push(',');
        }
        nodes_str.push_str(&format!(r#"{{"id":"n{i}","labels":["N"],"props":{{}}}}"#));
    }
    nodes_str.push(']');

    // Erdős-Rényi-ish: pick `nodes * avg_deg` directed edges uniformly at random
    // (with replacement) — duplicates are allowed; that's part of "dense".
    let mut rng = Lcg::new(0xC0FFEE);
    let edge_count = nodes * avg_deg;
    let mut edges_str = String::from("[");
    for e in 0..edge_count {
        if e > 0 {
            edges_str.push(',');
        }
        let s = rng.gen_range(nodes as u32);
        let t = rng.gen_range(nodes as u32);
        edges_str.push_str(&format!(
            r#"{{"id":"e{e}","labels":["E"],"endpoints":["n{s}","n{t}"],"directionality":"->","props":{{}}}}"#
        ));
    }
    edges_str.push(']');

    format!(r#"{{"nodes":{nodes_str},"edges":{edges_str}}}"#)
}

fn run_one<G: GraphAccess>(graph: &G, query: &str, incremental: bool) -> (u128, usize) {
    if incremental {
        env::set_var("GQLITE_REPEAT_INCREMENTAL", "1");
    } else {
        env::remove_var("GQLITE_REPEAT_INCREMENTAL");
    }
    let pattern = compile(query).expect("compile failed");
    let rt = Runtime::new(graph);
    let t0 = Instant::now();
    let ir = rt.run(&pattern);
    let dt = t0.elapsed().as_micros();
    (dt, ir.rows.len())
}

fn median(mut xs: Vec<u128>) -> u128 {
    xs.sort_unstable();
    xs[xs.len() / 2]
}

fn bench<G: GraphAccess>(
    label: &str,
    graph: &G,
    query: &str,
    incremental: bool,
    iters: usize,
) -> (u128, usize) {
    // One warmup run, then `iters` measured. We report median to drop outliers.
    let (_, rows_warm) = run_one(graph, query, incremental);
    let mut samples = Vec::with_capacity(iters);
    let mut last_rows = rows_warm;
    for _ in 0..iters {
        let (us, rows) = run_one(graph, query, incremental);
        samples.push(us);
        last_rows = rows;
    }
    let med = median(samples.clone());
    let min = *samples.iter().min().unwrap();
    let max = *samples.iter().max().unwrap();
    println!(
        "  {label:14}  rows={last_rows:>10}  median={med:>10} us   min={min} us   max={max} us"
    );
    (med, last_rows)
}

fn run_gdb_mode(cfg: &BenchCfg) {
    let path = cfg.gdb.as_ref().unwrap();
    let query = cfg
        .query
        .as_ref()
        .expect("--gdb requires --query \"<full pattern>\"");

    eprintln!("Loading {} ...", path);
    let t0 = Instant::now();
    let store = LazyGraphStore::open(Path::new(path)).expect("open .gdb failed");
    eprintln!(
        "Loaded in {:.2}s ({} nodes, {} edges)",
        t0.elapsed().as_secs_f64(),
        store.node_count(),
        store.edge_count()
    );

    println!("Query: {query}\nIters: {} (median reported)\n", cfg.iters);
    let (legacy, rows_l) = bench("legacy", &store, query, false, cfg.iters);
    let (incr, rows_i) = bench("incremental", &store, query, true, cfg.iters);

    let parity = rows_l == rows_i;
    let speedup = if incr > 0 {
        legacy as f64 / incr as f64
    } else {
        f64::INFINITY
    };
    println!(
        "\nResult: parity={}  legacy={} us  incremental={} us  speedup={:.2}x",
        if parity { "OK" } else { "MISMATCH" },
        legacy,
        incr,
        speedup
    );
}

fn main() {
    let cfg = parse_args();

    if cfg.gdb.is_some() {
        run_gdb_mode(&cfg);
        return;
    }

    let json = build_graph_json(cfg.nodes, cfg.avg_deg);
    let graph = MemoryGraphStore::from_json_str(&json).expect("graph build");

    println!(
        "MemoryGraphStore: nodes={} directed-edges={} avg-out-deg≈{}",
        graph.node_count(),
        graph.edge_count(),
        cfg.avg_deg
    );

    let query = format!("(a)-[x]->{{{},{}}}(b)", cfg.lb, cfg.ub);
    println!("Query: {query}\nIters: {} (median reported)\n", cfg.iters);

    println!("--- {{{},{}}} ---", cfg.lb, cfg.ub);
    let (legacy, rows_legacy) = bench("legacy", &graph, &query, false, cfg.iters);
    let (incr, rows_incr) = bench("incremental", &graph, &query, true, cfg.iters);

    let parity = rows_legacy == rows_incr;
    let speedup = if incr > 0 {
        legacy as f64 / incr as f64
    } else {
        f64::INFINITY
    };

    println!(
        "\nResult: parity={}  legacy={} us  incremental={} us  speedup={:.2}x",
        if parity { "OK" } else { "MISMATCH" },
        legacy,
        incr,
        speedup
    );

    // Sweep over upper bound to show how the gap scales.
    println!("\n--- sweep ub from lb..=cfg.ub (lb={}) ---", cfg.lb);
    println!("  ub  legacy(us)   incr(us)  speedup  rows");
    for ub in cfg.lb..=cfg.ub {
        let q = format!("(a)-[x]->{{{},{}}}(b)", cfg.lb, ub);
        let (l, _) = run_one(&graph, &q, false);
        let (l2, _) = run_one(&graph, &q, false);
        let (l3, _) = run_one(&graph, &q, false);
        let (i1, ri) = run_one(&graph, &q, true);
        let (i2, _) = run_one(&graph, &q, true);
        let (i3, _) = run_one(&graph, &q, true);
        let lm = median(vec![l, l2, l3]);
        let im = median(vec![i1, i2, i3]);
        let sp = lm as f64 / im as f64;
        println!("  {ub:2}  {lm:>9}   {im:>8}   {sp:>5.2}x  {ri}");
    }
}
