//! typecheck_scaling — runtime-independent scaling microbenchmark of the
//! type checker: cost of `Typechecker::check_query` as a function of
//! schema size and query size (chain length). No runtime, storage, or
//! data involved.
//!
//!   cargo run --release --bin typecheck_scaling -- --schema-size S [out.csv]
//!
//! One invocation measures one schema size. Sweep S over powers of two
//! (16..1024) plus 36 — the entry count of the LDBC-inferred schema,
//! identical for SF0.1 and SF0.3 — and aggregate >=3 sweeps by medians,
//! interleaving sizes within each sweep: wall clock varies between
//! sessions, so only within-sweep comparisons are reliable.
//!
//! The schema family is synthetic because schema size is the controlled
//! variable (a real inferred schema is scale-invariant and cannot be
//! grown), and homogeneous so the axis varies only the entry count.
//! Unlike real inferred edge entries, the synthetic ones leave endpoints
//! unconstrained — this shifts constants, not scaling shape.
//!
//! The cases are internal_bench.rs queries (`v_chain_knows`,
//! `e_bad_edge_deep`, `e_label_only`), byte-identical after renaming
//! Person->L0, knows->e0, Wagumi->NoSuchLabel; the chain gains length k
//! by inserting intermediate nodes only. Scope: labeled patterns.
//!
//! The checker keeps no state across queries, so steady-state medians
//! equal per-query cost in any session regime; if cross-query caching
//! ever lands, cold and warm regimes must be measured separately.

use std::fs::File;
use std::hint::black_box;
use std::io::Write;
use std::time::Instant;

use frogql::typing::checker::Typechecker;
use frogql::typing::descriptor_type::DescriptorType;
use frogql::typing::label_type::LabelType;
use frogql::typing::property_type::PropertyType;
use frogql::typing::simple_type::SimpleType;
use frogql::typing::variable_type::{Schema, VariableType};

const ITERS: usize = 4000;
const CASE_BUDGET_NS: u128 = 200_000_000;

fn synthetic_schema(entries: usize) -> Schema {
    let n_nodes = (entries / 3).max(2);
    let n_edges = entries - n_nodes;
    let nodes = (0..n_nodes)
        .map(|i| {
            let props: std::collections::BTreeMap<String, SimpleType> = [
                ("id".to_string(), SimpleType::Z),
                ("firstName".to_string(), SimpleType::S),
            ]
            .into_iter()
            .collect();
            VariableType::Node(DescriptorType::new(
                LabelType::Label(format!("L{i}")),
                PropertyType::Open(props),
            ))
        })
        .collect();
    let edges = (0..n_edges)
        .map(|i| {
            VariableType::edge_non_directional(DescriptorType::new(
                LabelType::Label(format!("e{i}")),
                PropertyType::open_empty(),
            ))
        })
        .collect();
    Schema::from_parts(nodes, edges)
}

/// `v_chain_knows` under the renaming, extended to `hops` by inserting
/// intermediate nodes; hops=1 is the internal benchmark's query.
fn chain_case(hops: usize) -> String {
    let mut s = "MATCH (p: L0)".to_string();
    for i in 1..hops {
        s.push_str(&format!("~[:e0]~(m{i}: L0)"));
    }
    s.push_str("~[:e0]~(f: L0) RETURN f.firstName");
    s
}

fn median_min(mut v: Vec<u128>) -> (u128, u128) {
    v.sort_unstable();
    (v[v.len() / 2], v[0])
}

fn main() {
    let mut entries = 36usize; // default: the LDBC-inferred schema size
    let mut out = "typecheck_scaling.csv".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--schema-size" => {
                entries = args.next().and_then(|v| v.parse().ok()).unwrap_or(entries)
            }
            other => out = other.to_string(),
        }
    }
    let schema = synthetic_schema(entries);
    let n_nodes = schema.nodes.len();
    let n_edges = schema.edges.len();
    println!(
        "schema: {n_nodes} node entries + {n_edges} edge entries = {}",
        n_nodes + n_edges
    );

    let mut cases: Vec<(String, String, String)> = Vec::new();
    for hops in [1usize, 2, 4, 8, 16] {
        cases.push((
            format!("v_chain_knows_{hops}"),
            "valid".into(),
            chain_case(hops),
        ));
    }
    cases.push((
        "e_bad_edge_deep".into(),
        "empty".into(),
        "MATCH (a: L0)~[:e0]~(b: L0)-[:noSuchEdge]->(c: L0) RETURN c.firstName".into(),
    ));
    cases.push((
        "e_label_only".into(),
        "empty".into(),
        "MATCH (x: NoSuchLabel) RETURN x.id".into(),
    ));

    let mut f = File::create(&out).unwrap_or_else(|e| panic!("create {out}: {e}"));
    writeln!(
        f,
        "case,schema_entries,expected,got,check_med_ns,check_min_ns"
    )
    .unwrap();
    println!(
        "{:<18}{:>9}{:>9}{:>15}{:>15}",
        "case", "exp", "got", "check_med_ns", "check_min_ns"
    );
    let mut mismatches = 0usize;
    for (id, expected, q) in &cases {
        let parsed = frogql::parser::parse_query(q).expect("parse");
        let elaborated = frogql::elaborate::elaborate_query(parsed);
        let mut tc = Typechecker::new(schema.clone());
        let r = tc.check_query(&elaborated);
        let got = if !r.ok {
            "invalid"
        } else if r.empty {
            "empty"
        } else {
            "valid"
        };
        let t = Instant::now();
        for _ in 0..3 {
            black_box(tc.check_query(black_box(&elaborated)));
        }
        let est = (t.elapsed().as_nanos() / 3).max(1);
        let iters = ((CASE_BUDGET_NS / est) as usize).clamp(20, ITERS);
        for _ in 0..(iters / 10).max(3) {
            black_box(tc.check_query(black_box(&elaborated)));
        }
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let t = Instant::now();
            black_box(tc.check_query(black_box(&elaborated)));
            samples.push(t.elapsed().as_nanos());
        }
        let (med, min) = median_min(samples);
        if got != expected {
            mismatches += 1;
        }
        println!(
            "{:<18}{:>9}{:>9}{:>15}{:>15}{}",
            id,
            expected,
            got,
            med,
            min,
            if got == expected {
                ""
            } else {
                "  <-- VERDICT MISMATCH"
            }
        );
        writeln!(f, "{id},{},{expected},{got},{med},{min}", n_nodes + n_edges).unwrap();
    }
    println!("wrote {out}");
    if mismatches > 0 {
        println!("WARNING: {mismatches} verdict mismatch(es).");
    }
}
