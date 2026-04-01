//! Benchmark: generate a large graph, run queries, report timing.
//! Run with: cargo test --test bench_test --release -- --nocapture

use std::time::Instant;

use gqlrust::model::graph::Graph;
use gqlrust::compile;
use gqlrust::runtime::engine::Runtime;

/// Generate a graph with N nodes and ~E edges as a JSON value.
fn generate_graph(num_nodes: usize, num_edges: usize) -> serde_json::Value {
    let labels = ["Person", "Company", "Product", "Location", "Event"];
    let edge_labels = ["Knows", "WorksAt", "Bought", "LocatedIn", "Attended"];

    let mut nodes = Vec::new();
    for i in 0..num_nodes {
        let label = labels[i % labels.len()];
        nodes.push(serde_json::json!({
            "id": format!("n{i}"),
            "labels": [label],
            "props": {
                "name": format!("entity_{i}"),
                "value": i as i64,
                "active": i % 3 == 0,
            }
        }));
    }

    let mut edges = Vec::new();
    for i in 0..num_edges {
        let src = i % num_nodes;
        let tgt = (i * 7 + 13) % num_nodes; // pseudo-random but deterministic
        if src == tgt { continue; }
        let label = edge_labels[i % edge_labels.len()];
        edges.push(serde_json::json!({
            "id": format!("e{i}"),
            "labels": [label],
            "props": { "weight": (i % 100) as i64 },
            "endpoints": [format!("n{src}"), format!("n{tgt}")],
            "directionality": "->"
        }));
    }

    serde_json::json!({ "nodes": nodes, "edges": edges })
}

fn bench_query(graph: &Graph, query: &str, label: &str) {
    let pattern = compile(query).unwrap();
    let rt = Runtime::new(graph);

    // Warmup
    let _ = rt.run(&pattern);

    let start = Instant::now();
    let iterations = 10;
    let mut total_rows = 0;
    for _ in 0..iterations {
        total_rows = rt.run(&pattern).rows.len();
    }
    let elapsed = start.elapsed();

    println!(
        "  {label}: {total_rows} rows, {:.2}ms avg ({iterations} iterations)",
        elapsed.as_secs_f64() * 1000.0 / iterations as f64
    );
}

#[test]
fn bench_large_graph() {
    println!("\n=== Generating graph: 10K nodes, 50K edges ===");
    let json = generate_graph(10_000, 50_000);
    let graph = Graph::from_json_value(&json).unwrap();
    println!(
        "  Nodes: {}, Directed edges: {}, Undirected edges: {}",
        graph.nodes.len(),
        graph.edges_d.len(),
        graph.edges_u.len()
    );

    println!("\n=== Queries (with label index + adjacency optimization) ===");

    bench_query(&graph, "()", "All nodes");
    bench_query(&graph, "(x: Person)", "Nodes by label (Person)");
    bench_query(&graph, "-[]->", "All directed edges");
    bench_query(&graph, "-[x: Knows]->", "Edges by label (Knows)");
    bench_query(&graph, "(x: Person)-[:Knows]->(y)", "1-hop: Person-[:Knows]->y");
    bench_query(
        &graph,
        "(x: Person)-[:Knows]->(y)-[:WorksAt]->(z)",
        "2-hop: Person-[:Knows]->y-[:WorksAt]->z",
    );
    bench_query(
        &graph,
        "(x: Person)-[:Knows]->(y)-[]->(z)",
        "2-hop: Person-[:Knows]->y-[]->z",
    );
    bench_query(
        &graph,
        "(x WHERE x.active=true)-[:Knows]->(y)",
        "Filter + 1-hop",
    );
    bench_query(&graph, "(x: Person) | (y: Company)", "Union: Person | Company");
}

#[test]
fn bench_small_graph_repetition() {
    println!("\n=== Repetition benchmark (fraud graph) ===");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
    let graph = Graph::from_file(&path).unwrap();

    bench_query(&graph, "-->{1,3}", "Repeat -->{{1,3}}");
    bench_query(&graph, "-[x]->{2,4}", "Repeat -[x]->{{2,4}}");
    bench_query(&graph, "(-[x]->{1,2}){2,3}", "Nested repeat");
}
