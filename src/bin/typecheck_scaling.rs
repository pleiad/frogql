//! typecheck_scaling — runtime-independent scaling microbenchmark of the
//! type checker: the cost of `Typechecker::check_query` as a function of
//! (a) schema size and (b) query size. No runtime, storage, or data is
//! involved — only the query compiler's checking phase.
//!
//!   cargo run --release --bin typecheck_scaling -- --schema-size S [out.csv]
//!
//! This branch is based on the v0.2.0 release so the numbers track a
//! fixed, released baseline of the checker. The v0.2.0 checker keeps no
//! state across queries, so steady-state medians equal per-query cost in
//! any session regime.
//!
//! Axes:
//!   * Schema size: one invocation per size S; suggested ladder
//!     16, 32, 64, 128, 256, 512, 1024 (powers of two for clean plotting)
//!     plus 36 — the measured entry count of the schema inferred from
//!     LDBC SNB SF0.1 (11 node + 25 edge entries) — as the anchor for
//!     the fixed-schema query-size curve.
//!   * Query size: chain length k in {1, 2, 4, 8, 16} at a fixed S.
//!
//! The synthetic schema is a homogeneous family parameterized by S:
//! max(S/3, 2) node types (labels `L0..`, each with properties
//! {id: Int, firstName: Str}) and the rest undirected edge types
//! (labels `e0..`, no properties, unconstrained endpoints). The 1:2
//! node:edge ratio matches the one measured on the LDBC-inferred schema.
//! A synthetic family is required because schema size is the controlled
//! variable — a fixed real schema cannot be grown. Declared
//! simplification: real inferred edge entries record endpoint label
//! combinations; the synthetic ones use unconstrained endpoints, which
//! affects constants, not scaling shape.
//!
//! The cases come from the engine's internal benchmark
//! (`internal_bench.rs`): `v_chain_knows`, `e_bad_edge_deep` and
//! `e_label_only`, byte-identical after renaming the labels into the
//! synthetic universe (Person->L0, knows->e0, Wagumi->NoSuchLabel; the
//! projected property names id and firstName exist verbatim in every
//! synthetic node type). The chain is parameterized in k by inserting
//! intermediate nodes only (m1..m(k-1)); k=1 is the original query.
//! Scope: labeled patterns, the intended usage of a typed engine.
//!
//! Protocol: parse and elaboration run once, outside the timed loop; the
//! loop times `check_query` alone — median of up to 4000 iterations
//! under an adaptive 200 ms per-case budget, with warmup and black_box.
//! Every run asserts each case's verdict (valid/empty) as a self-check.
//! For robust aggregation run the full size sweep >=3 times,
//! interleaving sizes within each repetition, and take medians —
//! wall-clock varies between sessions on consumer hardware, so only
//! comparisons within an interleaved sweep are reliable.

use std::fs::File;
use std::hint::black_box;
use std::io::Write;
use std::time::Instant;

use gqlrust::typing::checker::Typechecker;
use gqlrust::typing::descriptor_type::DescriptorType;
use gqlrust::typing::label_type::LabelType;
use gqlrust::typing::property_type::PropertyType;
use gqlrust::typing::simple_type::SimpleType;
use gqlrust::typing::variable_type::{Schema, VariableType};

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

/// `MATCH (p: L0)~[:e0]~(m1: L0)~...~(f: L0) RETURN f.firstName` —
/// `v_chain_knows` under the three-label renaming; extending to k hops
/// only INSERTS intermediate nodes m1..m(k-1). k=1 is the internal
/// benchmark's query, byte-identical after the renaming.
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
    let mut entries = 36usize; // the measured LDBC SF0.1 schema size
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
    writeln!(f, "case,schema_entries,expected,got,check_med_ns,check_min_ns").unwrap();
    println!(
        "{:<18}{:>9}{:>9}{:>15}{:>15}",
        "case", "exp", "got", "check_med_ns", "check_min_ns"
    );
    let mut mismatches = 0usize;
    for (id, expected, q) in &cases {
        let parsed = gqlrust::parser::parse_query(q).expect("parse");
        let elaborated = gqlrust::elaborate::elaborate_query(parsed);
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
