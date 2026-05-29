//! Benchmark: generate realistic graphs, run queries, report timing.
//! Run with: cargo test --test bench_test --release -- --nocapture

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use gqlrust::compile;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::runtime::engine::Runtime;
use gqlrust::store::disk::DiskGraphStore;
use gqlrust::store::lazy::LazyGraphStore;

// ============================================================================
// Tracking allocator — counts current heap usage
// ============================================================================

struct TrackingAllocator;

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let current = ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            // Update peak
            let mut peak = PEAK.load(Ordering::Relaxed);
            while current > peak {
                match PEAK.compare_exchange_weak(
                    peak,
                    current,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(p) => peak = p,
                }
            }
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        ALLOCATED.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static GLOBAL: TrackingAllocator = TrackingAllocator;

fn allocated_bytes() -> usize {
    ALLOCATED.load(Ordering::Relaxed)
}

// Memory-measurement helpers. Currently unused — wired in only through
// the (excluded-from-CI) memory-scaling benches.
#[allow(dead_code)]
fn reset_peak() {
    PEAK.store(ALLOCATED.load(Ordering::Relaxed), Ordering::Relaxed);
}

#[allow(dead_code)]
fn peak_bytes() -> usize {
    PEAK.load(Ordering::Relaxed)
}

fn fmt_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

// ============================================================================
// MemoryGraphStore generator — realistic social/financial network with:
//   - Multi-label nodes (Person & Employee, Account & Premium, etc.)
//   - Multiple typed properties (str, int, bool)
//   - Directed AND undirected edges
//   - Intentional cycles (A→B→C→A)
//   - Variable node degree (some hubs, some leaf nodes)
// ============================================================================

/// Simple deterministic pseudo-random (xorshift32) — no external deps needed.
struct Rng(u32);
impl Rng {
    fn new(seed: u32) -> Self {
        Self(seed)
    }
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
    fn usize(&mut self, max: usize) -> usize {
        self.next() as usize % max
    }
    fn bool(&mut self, pct: u32) -> bool {
        self.next() % 100 < pct
    }
}

fn generate_graph(
    num_nodes: usize,
    num_directed: usize,
    num_undirected: usize,
) -> serde_json::Value {
    let mut rng = Rng::new(42);

    // --- Node labels (some nodes get multiple) ---
    let primary_labels = ["Person", "Company", "Account", "Product", "City"];
    let secondary_labels = ["Premium", "Employee", "Verified", "Active", "Featured"];
    let names = [
        "Alice", "Bob", "Carol", "Dave", "Eve", "Frank", "Grace", "Hank", "Ivy", "Jay",
    ];
    let cities = [
        "NYC", "London", "Tokyo", "Berlin", "Sydney", "Toronto", "Seoul", "Paris",
    ];

    let mut nodes = Vec::new();
    for i in 0..num_nodes {
        let primary = primary_labels[rng.usize(primary_labels.len())];
        let mut labels = vec![primary.to_string()];
        // 30% of nodes get a second label
        if rng.bool(30) {
            labels.push(secondary_labels[rng.usize(secondary_labels.len())].to_string());
        }
        // 5% get a third label
        if rng.bool(5) {
            let third = secondary_labels[rng.usize(secondary_labels.len())];
            if !labels.contains(&third.to_string()) {
                labels.push(third.to_string());
            }
        }

        let name = names[rng.usize(names.len())];
        let city = cities[rng.usize(cities.len())];

        nodes.push(serde_json::json!({
            "id": format!("n{i}"),
            "labels": labels,
            "props": {
                "name": format!("{name}_{i}"),
                "city": city,
                "age": 18 + (rng.next() % 60) as i64,
                "score": (rng.next() % 10000) as i64,
                "active": rng.bool(70),
                "verified": rng.bool(40),
            }
        }));
    }

    // --- Directed edges with cycles ---
    let dir_labels = ["Transfer", "Follows", "Manages", "Bought", "Reviewed"];
    let mut edges = Vec::new();
    let mut edge_id = 0;

    // Create explicit cycles: chains of length 3-7 that loop back
    let num_cycles = num_directed / 20; // ~5% of edges form explicit cycles
    for _ in 0..num_cycles {
        let cycle_len = 3 + rng.usize(5); // 3 to 7 nodes per cycle
        let start = rng.usize(num_nodes);
        let mut prev = start;
        for step in 0..cycle_len {
            let next = if step == cycle_len - 1 {
                start // close the cycle
            } else {
                rng.usize(num_nodes)
            };
            if prev != next {
                let label = dir_labels[rng.usize(dir_labels.len())];
                edges.push(serde_json::json!({
                    "id": format!("e{edge_id}"),
                    "labels": [label],
                    "props": {
                        "amount": (rng.next() % 1000000) as i64,
                        "timestamp": 1700000000i64 + (rng.next() % 31536000) as i64,
                        "flagged": rng.bool(10),
                    },
                    "endpoints": [format!("n{prev}"), format!("n{next}")],
                    "directionality": "->"
                }));
                edge_id += 1;
            }
            prev = next;
        }
    }

    // Fill remaining directed edges with power-law-ish distribution (some hubs)
    while edge_id < num_directed {
        // 20% of edges come from "hub" nodes (first 10% of nodes)
        let src = if rng.bool(20) {
            rng.usize(num_nodes / 10)
        } else {
            rng.usize(num_nodes)
        };
        let tgt = rng.usize(num_nodes);
        if src == tgt {
            continue;
        }

        let label = dir_labels[rng.usize(dir_labels.len())];
        // 15% of edges get a second label
        let mut elabels = vec![label.to_string()];
        if rng.bool(15) {
            let second = dir_labels[rng.usize(dir_labels.len())];
            if second != label {
                elabels.push(second.to_string());
            }
        }

        edges.push(serde_json::json!({
            "id": format!("e{edge_id}"),
            "labels": elabels,
            "props": {
                "amount": (rng.next() % 1000000) as i64,
                "timestamp": 1700000000i64 + (rng.next() % 31536000) as i64,
                "flagged": rng.bool(10),
            },
            "endpoints": [format!("n{src}"), format!("n{tgt}")],
            "directionality": "->"
        }));
        edge_id += 1;
    }

    // --- Undirected edges (friendship, similarity) ---
    let undir_labels = ["FriendOf", "SimilarTo", "ColocatedWith"];
    for _ in 0..num_undirected {
        let a = rng.usize(num_nodes);
        let b = rng.usize(num_nodes);
        if a == b {
            continue;
        }

        let label = undir_labels[rng.usize(undir_labels.len())];
        edges.push(serde_json::json!({
            "id": format!("e{edge_id}"),
            "labels": [label],
            "props": {
                "strength": (rng.next() % 100) as i64,
                "mutual": rng.bool(60),
            },
            "endpoints": [format!("n{a}"), format!("n{b}")],
            "directionality": "~~"
        }));
        edge_id += 1;
    }

    serde_json::json!({ "nodes": nodes, "edges": edges })
}

// ============================================================================
// Benchmark harness
// ============================================================================

fn bench_query<G: GraphAccess>(graph: &G, query: &str, label: &str) -> (usize, f64) {
    let pattern = compile(query).unwrap();
    let rt = Runtime::new(graph);

    // Warmup
    let _ = rt.run(&pattern);

    let iterations = 5;
    let start = Instant::now();
    let mut total_rows = 0;
    for _ in 0..iterations {
        total_rows = rt.run(&pattern).rows.len();
    }
    let elapsed = start.elapsed();
    let ms = elapsed.as_secs_f64() * 1000.0 / iterations as f64;

    println!("  {:50} {:>8} rows  {:>8.2}ms", label, total_rows, ms);
    (total_rows, ms)
}

// Currently unused — utility for cross-backend comparison benches.
#[allow(dead_code)]
fn bench_compare<G1: GraphAccess, G2: GraphAccess>(
    g1: &G1,
    g1_name: &str,
    g2: &G2,
    g2_name: &str,
    query: &str,
    label: &str,
) {
    let pattern = compile(query).unwrap();
    let iterations = 5;

    // Bench g1
    let rt1 = Runtime::new(g1);
    let _ = rt1.run(&pattern);
    let start = Instant::now();
    let mut rows1 = 0;
    for _ in 0..iterations {
        rows1 = rt1.run(&pattern).rows.len();
    }
    let ms1 = start.elapsed().as_secs_f64() * 1000.0 / iterations as f64;

    // Bench g2
    let rt2 = Runtime::new(g2);
    let _ = rt2.run(&pattern);
    let start = Instant::now();
    let mut rows2 = 0;
    for _ in 0..iterations {
        rows2 = rt2.run(&pattern).rows.len();
    }
    let ms2 = start.elapsed().as_secs_f64() * 1000.0 / iterations as f64;

    assert_eq!(rows1, rows2, "result mismatch for '{query}'");

    let ratio = if ms1 > 0.01 { ms2 / ms1 } else { 0.0 };
    println!(
        "  {:42} {:>7} rows  {:>6}:{:.1}ms  {:>6}:{:.1}ms  {:>5.1}x",
        label, rows1, g1_name, ms1, g2_name, ms2, ratio
    );
}

fn print_graph_stats(graph: &MemoryGraphStore) {
    let nc = graph.node_count();
    let ec = graph.edge_count();
    let dc = graph.edge_directed.iter().filter(|&&d| d).count();
    let uc = ec - dc;
    println!("  Nodes: {}, Directed: {}, Undirected: {}", nc, dc, uc);

    let multi_label = graph
        .node_labels
        .iter()
        .filter(|lt| !matches!(lt, gqlrust::typing::label_type::LabelType::Label(_)))
        .count();
    println!(
        "  Multi-label nodes: {} ({:.1}%)",
        multi_label,
        100.0 * multi_label as f64 / nc as f64
    );
}

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("gqlrust_bench");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}

// ============================================================================
// Benchmarks
// ============================================================================

#[test]
fn bench_medium_graph() {
    println!("\n============================================================");
    println!("  MEDIUM GRAPH: 10K nodes, 50K directed, 5K undirected");
    println!("============================================================");

    let json = generate_graph(10_000, 50_000, 5_000);
    let start = Instant::now();
    let graph = MemoryGraphStore::from_json_value(&json).unwrap();
    println!(
        "  Load time: {:.1}ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
    print_graph_stats(&graph);

    println!("\n  --- Scans ---");
    bench_query(&graph, "()", "All nodes");
    bench_query(&graph, "(x: Person)", "By label: Person");
    bench_query(
        &graph,
        "(x: Person & Employee)",
        "Multi-label: Person & Employee",
    );
    bench_query(&graph, "(x: Premium)", "Secondary label: Premium");
    bench_query(&graph, "-[]->", "All directed edges");
    bench_query(&graph, "-[:Transfer]->", "Directed by label: Transfer");
    bench_query(&graph, "~~", "All undirected edges");
    bench_query(&graph, "~[:FriendOf]~", "Undirected by label: FriendOf");

    println!("\n  --- Filters ---");
    bench_query(&graph, "(x WHERE x.active = true)", "Filter: active=true");
    bench_query(&graph, "(x WHERE x.age > 50)", "Filter: age > 50");
    bench_query(
        &graph,
        "(x WHERE x.active = true and x.verified = true)",
        "Filter: active AND verified",
    );
    bench_query(
        &graph,
        "(x: Person WHERE x.age > 30 and x.active = true)",
        "Label + filter combo",
    );

    println!("\n  --- 1-hop traversal ---");
    bench_query(
        &graph,
        "(x: Person)-[:Transfer]->(y)",
        "Person -Transfer-> y",
    );
    bench_query(
        &graph,
        "(x: Person)-[:Follows]->(y: Person)",
        "Person -Follows-> Person",
    );
    bench_query(
        &graph,
        "(x)-[:Transfer]->(y WHERE y.active = true)",
        "Transfer to active node",
    );
    bench_query(
        &graph,
        "(x: Company)<-[:Manages]-(y)",
        "Company <-Manages- y",
    );
    bench_query(&graph, "(x)~[:FriendOf]~(y)", "Undirected: x ~FriendOf~ y");

    println!("\n  --- 2-hop traversal ---");
    bench_query(
        &graph,
        "(x: Person)-[:Transfer]->(y)-[:Transfer]->(z)",
        "Person -Transfer-> y -Transfer-> z",
    );
    bench_query(
        &graph,
        "(x: Person)-[:Follows]->(y)-[:Bought]->(z: Product)",
        "Person -Follows-> y -Bought-> Product",
    );
    bench_query(&graph, "(x)-[]->(y)-[]->(z)", "Any 2-hop: x -> y -> z");

    println!("\n  --- Multi-direction ---");
    bench_query(
        &graph,
        "(x: Person)-[:Transfer]->(y)~[:FriendOf]~(z)",
        "Directed then undirected",
    );
    bench_query(&graph, "(x)-", "Any direction edge (-)");

    println!("\n  --- Repetition ---");
    bench_query(&graph, "(-[:Transfer]->){1,2}", "Transfer repeat {{1,2}}");
    bench_query(&graph, "(-[:Transfer]->){1,3}", "Transfer repeat {{1,3}}");
    bench_query(&graph, "(-[]->{1,2})", "Any edge repeat {{1,2}}");

    println!("\n  --- Union ---");
    bench_query(
        &graph,
        "(x: Person) | (x: Company)",
        "Union: Person | Company",
    );
    bench_query(
        &graph,
        "(x: Person)-[:Transfer]->(y) | (x: Person)-[:Follows]->(y)",
        "Union of 1-hop patterns",
    );

    println!("\n  --- Complex (cycles, multi-label, property filters) ---");
    bench_query(
        &graph,
        "((x: Person)-[:Transfer]->(y) WHERE x.active = true and y.verified = true)",
        "Active Person transfers to Verified",
    );
    bench_query(
        &graph,
        "(x: Person & Verified)-[:Transfer]->(y)-[:Transfer]->(z: Account)",
        "Multi-label source, 2-hop to Account",
    );
}

#[test]
fn bench_large_graph() {
    println!("\n============================================================");
    println!("  LARGE GRAPH: 50K nodes, 250K directed, 25K undirected");
    println!("============================================================");

    let json = generate_graph(50_000, 250_000, 25_000);
    let start = Instant::now();
    let graph = MemoryGraphStore::from_json_value(&json).unwrap();
    println!(
        "  Load time: {:.1}ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
    print_graph_stats(&graph);

    println!("\n  --- Key queries at scale ---");
    bench_query(&graph, "(x: Person)", "Scan: Person nodes");
    bench_query(&graph, "-[:Transfer]->", "Scan: Transfer edges");
    bench_query(
        &graph,
        "(x: Person)-[:Transfer]->(y)",
        "1-hop: Person -Transfer-> y",
    );
    bench_query(
        &graph,
        "(x: Person)-[:Transfer]->(y)-[:Transfer]->(z)",
        "2-hop: Transfer chain",
    );
    bench_query(
        &graph,
        "((x: Person)-[:Transfer]->(y) WHERE x.active = true)",
        "1-hop + filter",
    );
    bench_query(&graph, "(-[:Transfer]->){1,2}", "Transfer repeat {{1,2}}");
    bench_query(&graph, "(x)~[:FriendOf]~(y)", "Undirected: FriendOf");
}

#[test]
fn bench_graph_vs_lazy() {
    println!("\n============================================================");
    println!("  COMPARISON: MemoryGraphStore (in-memory) vs LazyGraphStore (page cache)");
    println!("  10K nodes, 50K directed, 5K undirected");
    println!("============================================================");

    let db_path = temp_path("bench_compare.gql");
    cleanup(&db_path);

    // Generate and save
    let json = generate_graph(10_000, 50_000, 5_000);
    let graph_tmp = MemoryGraphStore::from_json_value(&json).unwrap();
    graph_tmp.save(&db_path).unwrap();
    drop(graph_tmp);
    drop(json);

    // --- Measure memory for all three ---
    let before = allocated_bytes();
    let g_mem = MemoryGraphStore::open(&db_path).unwrap();
    let mem_graph = allocated_bytes() - before;

    let before = allocated_bytes();
    let g_lazy = LazyGraphStore::open(&db_path).unwrap();
    let mem_lazy = allocated_bytes() - before;

    let before = allocated_bytes();
    let g_disk = DiskGraphStore::open(&db_path).unwrap();
    let mem_disk = allocated_bytes() - before;

    print_graph_stats(&g_mem);
    println!("\n  Memory:");
    println!("    MemoryGraphStore:  {}", fmt_bytes(mem_graph));
    println!(
        "    Lazy:   {} ({:.1}x less)",
        fmt_bytes(mem_lazy),
        mem_graph as f64 / mem_lazy.max(1) as f64
    );
    println!(
        "    Disk:   {} ({:.1}x less)",
        fmt_bytes(mem_disk),
        mem_graph as f64 / mem_disk.max(1) as f64
    );

    println!(
        "\n  {:35} {:>7}  {:>8}  {:>8}  {:>8}",
        "Query", "Rows", "MemoryGraphStore", "Lazy", "Disk"
    );
    println!(
        "  {:35} {:>7}  {:>8}  {:>8}  {:>8}",
        "-----", "----", "-----", "----", "----"
    );

    let queries: Vec<(&str, &str)> = vec![
        ("()", "All nodes"),
        ("(x: Person)", "Label: Person"),
        ("-[:Transfer]->", "Edge: Transfer"),
        ("(x WHERE x.active = true)", "Filter: active=true"),
        ("(x: Person)-[:Transfer]->(y)", "1-hop traversal"),
        (
            "(x: Person)-[:Transfer]->(y)-[:Transfer]->(z)",
            "2-hop chain",
        ),
        ("(-[:Transfer]->){1,2}", "Repeat {1,2}"),
        (
            "((x: Person)-[:Transfer]->(y) WHERE x.active = true and y.active = true)",
            "Complex",
        ),
    ];

    for (query, label) in &queries {
        let pattern = compile(query).unwrap();
        let iters = 3;

        let rt = Runtime::new(&g_mem);
        let _ = rt.run(&pattern);
        let start = Instant::now();
        let mut rows = 0;
        for _ in 0..iters {
            rows = rt.run(&pattern).rows.len();
        }
        let ms_graph = start.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        let rt = Runtime::new(&g_lazy);
        let _ = rt.run(&pattern);
        let start = Instant::now();
        for _ in 0..iters {
            rt.run(&pattern);
        }
        let ms_lazy = start.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        let rt = Runtime::new(&g_disk);
        let _ = rt.run(&pattern);
        let start = Instant::now();
        for _ in 0..iters {
            rt.run(&pattern);
        }
        let ms_disk = start.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        println!(
            "  {:35} {:>7}  {:>6.1}ms  {:>6.1}ms  {:>6.1}ms",
            label, rows, ms_graph, ms_lazy, ms_disk
        );
    }

    cleanup(&db_path);
}

#[test]
fn bench_memory_scaling() {
    println!("\n============================================================");
    println!("  MEMORY SCALING: MemoryGraphStore vs Lazy vs Disk at different sizes");
    println!("============================================================");
    println!(
        "  {:>8} {:>8} {:>12} {:>12} {:>12}",
        "Nodes", "Edges", "MemoryGraphStore", "Lazy", "Disk"
    );
    println!(
        "  {:>8} {:>8} {:>12} {:>12} {:>12}",
        "-----", "-----", "-----", "----", "----"
    );

    for &(nodes, edges, undirected) in &[
        (1_000, 5_000, 500),
        (10_000, 50_000, 5_000),
        (50_000, 250_000, 25_000),
        (100_000, 500_000, 50_000),
    ] {
        let db_path = temp_path(&format!("bench_scale3_{}_{}.gql", nodes, edges));
        cleanup(&db_path);

        let json = generate_graph(nodes, edges, undirected);
        let g = MemoryGraphStore::from_json_value(&json).unwrap();
        g.save(&db_path).unwrap();
        drop(g);
        drop(json);

        // Measure MemoryGraphStore memory
        let before = allocated_bytes();
        let g = MemoryGraphStore::open(&db_path).unwrap();
        let graph_mem = allocated_bytes() - before;
        drop(g);

        // Measure Lazy memory
        let before = allocated_bytes();
        let l = LazyGraphStore::open(&db_path).unwrap();
        let lazy_mem = allocated_bytes() - before;
        drop(l);

        // Measure Disk memory
        let before = allocated_bytes();
        let d = DiskGraphStore::open(&db_path).unwrap();
        let disk_mem = allocated_bytes() - before;
        drop(d);

        println!(
            "  {:>8} {:>8} {:>12} {:>12} {:>12}",
            nodes,
            edges + undirected,
            fmt_bytes(graph_mem),
            fmt_bytes(lazy_mem),
            fmt_bytes(disk_mem)
        );

        cleanup(&db_path);
    }
}
