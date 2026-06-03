//! ORDER BY benchmark — pdqsort vs driftsort (stable merge) vs top-k heap
//! vs btree-driven bucket sort.
//!
//! Builds a synthetic graph of `n` `:User` nodes with a unique `id`
//! property, varies `n`, distribution of `id`, and `LIMIT k`, and
//! times each implementation under both backends:
//!
//! - **memory** (`MemoryGraphStore`) — no secondary index, btree path falls back
//!   to pdqsort.
//! - **lazy** (`LazyGraphStore`) — auto-builds a btree on `(User, id)`
//!   because `id` is unique within the label, so the btree path is
//!   actually exercised.
//!
//! Output is CSV on stdout: `n,k,backend,distribution,impl,median_ms`.
//! No external bench deps; no RSS reporting yet (timing only).
//!
//! Usage:
//!
//!     cargo build --release --bin orderby_bench
//!     ./target/release/orderby_bench                # default (small n)
//!     ./target/release/orderby_bench --full         # adds 100k
//!     ./target/release/orderby_bench --iters 7      # bump iterations
//!
//! The `--full` flag adds n = 100_000 (slow on debug; release-only).

use std::env;
use std::sync::Arc;
use std::time::Instant;

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::ltj::triple_index::TripleIndex;
use gqlrust::store::lazy::LazyGraphStore;

const DEFAULT_ITERS: usize = 5;

fn main() {
    let args: Vec<String> = env::args().collect();
    let full = args.iter().any(|a| a == "--full");
    let iters = parse_flag(&args, "--iters").unwrap_or(DEFAULT_ITERS);

    let ns: Vec<usize> = if full {
        vec![1_000, 10_000, 100_000]
    } else {
        vec![1_000, 10_000]
    };
    let dists = ["random", "sorted_asc", "sorted_desc"];

    println!("n,k,backend,distribution,impl,median_ms");

    for &n in &ns {
        for dist in dists {
            // Single-node bench: ORDER BY x.id over MATCH (x: User).
            let g = build_user_graph(n, dist);
            let queries = make_queries(n);

            // Pre-warm the triple index for both backends so the bench
            // captures sort cost rather than per-iteration index build.
            let memory_idx: Arc<TripleIndex> = Runtime::new(&g).warm_triple_index();

            for (k_label, q) in &queries {
                for impl_name in ["pdqsort", "driftsort", "topk"] {
                    let med = bench_median(iters, || {
                        std::env::set_var("GQLITE_ORDERBY_FORCE", impl_name);
                        let rt = Runtime::with_triple_index(&g, memory_idx.clone());
                        let query = compile_query(q).expect("compile");
                        let t = Instant::now();
                        let _ = rt.run_query(&query, 0);
                        t.elapsed().as_secs_f64() * 1_000.0
                    });
                    println!("{n},{k_label},memory_node,{dist},{impl_name},{med:.3}");
                }
            }

            let tmp = std::env::temp_dir().join(format!("orderby_bench_{n}_{dist}.gdb"));
            let _ = std::fs::remove_file(&tmp);
            g.save(&tmp).expect("save graph to .gdb");
            let store = LazyGraphStore::open(&tmp).expect("open lazy store");
            let lazy_idx: Arc<TripleIndex> = Runtime::new(&store).warm_triple_index();

            for (k_label, q) in &queries {
                // btree-ltj-real precondition needs an edge — fails on
                // single-node queries by design (see bitácora 10 §13).
                // We still run it through the force path so the bench
                // reports the fallback cost (it routes to pdqsort).
                for impl_name in [
                    "pdqsort",
                    "driftsort",
                    "topk",
                    "btree-ltj",
                    "btree-ltj-real",
                ] {
                    let med = bench_median(iters, || {
                        std::env::set_var("GQLITE_ORDERBY_FORCE", impl_name);
                        let rt = Runtime::with_triple_index(&store, lazy_idx.clone());
                        let query = compile_query(q).expect("compile");
                        let t = Instant::now();
                        let _ = rt.run_query(&query, 0);
                        t.elapsed().as_secs_f64() * 1_000.0
                    });
                    println!("{n},{k_label},lazy_node,{dist},{impl_name},{med:.3}");
                }
            }
            let _ = std::fs::remove_file(&tmp);

            // Join bench: ORDER BY u.id over MATCH (u: User)-[:Knows]->(f: User).
            // Each user knows roughly `avg_degree` others. The btree-ltj-real
            // path is meant to win here because the LTJ actually fires.
            let gj = build_user_with_knows(n, dist, 4);
            let join_queries = make_join_queries(n);
            let memory_idx_j: Arc<TripleIndex> = Runtime::new(&gj).warm_triple_index();

            for (k_label, q) in &join_queries {
                for impl_name in ["pdqsort", "driftsort", "topk"] {
                    let med = bench_median(iters, || {
                        std::env::set_var("GQLITE_ORDERBY_FORCE", impl_name);
                        let rt = Runtime::with_triple_index(&gj, memory_idx_j.clone());
                        let query = compile_query(q).expect("compile");
                        let t = Instant::now();
                        let _ = rt.run_query(&query, 0);
                        t.elapsed().as_secs_f64() * 1_000.0
                    });
                    println!("{n},{k_label},memory_join,{dist},{impl_name},{med:.3}");
                }
            }

            let tmp_j = std::env::temp_dir().join(format!("orderby_bench_join_{n}_{dist}.gdb"));
            let _ = std::fs::remove_file(&tmp_j);
            gj.save(&tmp_j).expect("save");
            let store_j = LazyGraphStore::open(&tmp_j).expect("open");
            let lazy_idx_j: Arc<TripleIndex> = Runtime::new(&store_j).warm_triple_index();

            for (k_label, q) in &join_queries {
                for impl_name in [
                    "pdqsort",
                    "driftsort",
                    "topk",
                    "btree-ltj",
                    "btree-ltj-real",
                ] {
                    let med = bench_median(iters, || {
                        std::env::set_var("GQLITE_ORDERBY_FORCE", impl_name);
                        let rt = Runtime::with_triple_index(&store_j, lazy_idx_j.clone());
                        let query = compile_query(q).expect("compile");
                        let t = Instant::now();
                        let _ = rt.run_query(&query, 0);
                        t.elapsed().as_secs_f64() * 1_000.0
                    });
                    println!("{n},{k_label},lazy_join,{dist},{impl_name},{med:.3}");
                }
            }
            let _ = std::fs::remove_file(&tmp_j);
        }
    }
}

fn make_join_queries(n: usize) -> Vec<(String, String)> {
    // Sort by the OUTER variable `u` of `u-[:Knows]->f` — best case for
    // btree-ltj-real because we drive `u` from the btree and the LTJ
    // expands to (u, f) pairs per pinned u.
    let mut out = Vec::new();
    for k in [1usize, 10, 100, 1_000] {
        if k <= n {
            out.push((
                format!("join_k{k}"),
                format!(
                    "MATCH (u: User)-[:Knows]->(f: User) RETURN u.id, f.id \
                     ORDER BY u.id LIMIT {k}"
                ),
            ));
        }
    }
    out
}

fn build_user_with_knows(n: usize, dist: &str, avg_degree: usize) -> MemoryGraphStore {
    let ids: Vec<usize> = match dist {
        "sorted_asc" => (0..n).collect(),
        "sorted_desc" => (0..n).rev().collect(),
        "random" => deterministic_shuffle(n, 0xdeadbeef),
        _ => panic!("unknown dist: {dist}"),
    };
    let mut nodes = String::with_capacity(n * 64);
    nodes.push('[');
    for (idx, id_val) in ids.iter().enumerate() {
        if idx > 0 {
            nodes.push(',');
        }
        nodes.push_str(&format!(
            "{{\"id\":\"u{idx}\",\"labels\":[\"User\"],\"props\":{{\"id\":{id_val}}}}}"
        ));
    }
    nodes.push(']');

    // Deterministic Knows edges: each user u_i connects to (i+1, i+2, ..., i+avg_degree) mod n.
    // Each edge gets a unique id "k{i}_{j}".
    let mut edges = String::with_capacity(n * avg_degree * 80);
    edges.push('[');
    let mut first = true;
    for i in 0..n {
        for j in 1..=avg_degree {
            if !first {
                edges.push(',');
            }
            first = false;
            let tgt = (i + j) % n;
            edges.push_str(&format!(
                "{{\"id\":\"k{i}_{j}\",\"labels\":[\"Knows\"],\"props\":{{}},\
                  \"endpoints\":[\"u{i}\",\"u{tgt}\"],\"directionality\":\"->\"}}"
            ));
        }
    }
    edges.push(']');

    let json = format!("{{\"nodes\":{nodes},\"edges\":{edges}}}");
    MemoryGraphStore::from_json_str(&json).expect("parse synthetic join graph")
}

fn make_queries(n: usize) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let half = n / 2;
    for k in [1usize, 10, 100, 1_000] {
        if k <= n {
            out.push((
                k.to_string(),
                format!("MATCH (x: User) RETURN x.id ORDER BY x.id LIMIT {k}"),
            ));
        }
    }
    if half > 0
        && !out
            .iter()
            .any(|(label, _)| label.parse::<usize>() == Ok(half))
    {
        out.push((
            format!("n_half_{half}"),
            format!("MATCH (x: User) RETURN x.id ORDER BY x.id LIMIT {half}"),
        ));
    }
    out.push((
        format!("full_{n}"),
        format!("MATCH (x: User) RETURN x.id ORDER BY x.id LIMIT {n}"),
    ));
    out
}

fn bench_median<F: FnMut() -> f64>(iters: usize, mut f: F) -> f64 {
    // 1 warmup ignored, then `iters` measured.
    let _ = f();
    let mut times: Vec<f64> = (0..iters).map(|_| f()).collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times[times.len() / 2]
}

fn build_user_graph(n: usize, dist: &str) -> MemoryGraphStore {
    // The `id` column drives the sort. Distribution controls how
    // input row order relates to sort order — pdqsort branches on
    // already-sorted input via the pdq dist-detection, so this
    // matters.
    let ids: Vec<usize> = match dist {
        "sorted_asc" => (0..n).collect(),
        "sorted_desc" => (0..n).rev().collect(),
        "random" => deterministic_shuffle(n, 0xdeadbeef),
        _ => panic!("unknown dist: {dist}"),
    };
    let mut nodes = String::with_capacity(n * 64);
    nodes.push('[');
    for (idx, id_val) in ids.iter().enumerate() {
        if idx > 0 {
            nodes.push(',');
        }
        nodes.push_str(&format!(
            "{{\"id\":\"u{idx}\",\"labels\":[\"User\"],\"props\":{{\"id\":{id_val}}}}}"
        ));
    }
    nodes.push(']');
    let json = format!("{{\"nodes\":{nodes},\"edges\":[]}}");
    MemoryGraphStore::from_json_str(&json).expect("parse synthetic graph")
}

fn deterministic_shuffle(n: usize, seed: u64) -> Vec<usize> {
    let mut v: Vec<usize> = (0..n).collect();
    let mut state = seed;
    for i in (1..n).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let j = ((state >> 33) as usize) % (i + 1);
        v.swap(i, j);
    }
    v
}

fn parse_flag(args: &[String], name: &str) -> Option<usize> {
    let pos = args.iter().position(|a| a == name)?;
    args.get(pos + 1).and_then(|v| v.parse().ok())
}
