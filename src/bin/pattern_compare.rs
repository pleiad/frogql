//! pattern_compare — times the type checker on the reduced LDBC path-pattern
//! cases and, optionally, compares against a reference CSV.
//!
//! Each case is an internal-bench query reduced to the FPPC path-pattern surface
//! (MATCH / RETURN dropped, a query-level WHERE inlined into the descriptor).
//! Rust times `check_query` on the reduced pattern — i.e. the path-pattern
//! typecheck only — isolated (no runtime), with a reused checker and
//! WARMUP + ITERS iterations.
//!
//!   cargo run --release --bin pattern_compare -- [reference.csv] [path.gdb]
//!
//! With no reference CSV it prints the Rust timings and the emptiness verdicts.
//! Given a reference CSV (header
//! `case,expected,got,parse_med_ns,parse_min_ns,check_med_ns,...`) it also prints
//! the per-case ratio and geomean. `got` must match `exp` — a sanity check that
//! the type checker classifies each case the same way as the reference.

use std::collections::HashMap;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

use frogql::store::lazy::LazyGraphStore;
use frogql::typing::checker::Typechecker;

const WARMUP: usize = 1000;
const ITERS: usize = 10000;
const DEFAULT_GDB: &str = "bench/data/ldbc-sf0.1.gdb";

fn median_us(mut v: Vec<u128>) -> f64 {
    v.sort_unstable();
    v[v.len() / 2] as f64 / 1000.0
}

/// case -> check_med (us), read from the reference CSV.
fn read_reference_csv(path: &str) -> HashMap<String, f64> {
    let mut m = HashMap::new();
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("could not read {path}: {e}"));
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 6 {
            continue;
        }
        if let Ok(ns) = f[5].parse::<f64>() {
            m.insert(f[0].to_string(), ns / 1000.0);
        }
    }
    m
}

fn main() {
    let mut args = std::env::args().skip(1);
    let ref_csv = args.next();
    let gdb = args.next().unwrap_or_else(|| DEFAULT_GDB.to_string());

    let reference: HashMap<String, f64> = ref_csv
        .as_deref()
        .map(read_reference_csv)
        .unwrap_or_default();

    let lazy = LazyGraphStore::open(Path::new(&gdb)).unwrap_or_else(|e| panic!("open {gdb}: {e}"));
    let schema = lazy.catalog().active_schema();

    // Reduced path-pattern queries, one per internal-bench case (undirected `~`
    // edges, query-level WHERE inlined; the IC and aggregate cases are dropped
    // because they are not path patterns). i_parse is omitted (a parse error has
    // no typecheck to time).
    let cases: &[(&str, &str, &str)] = &[
        ("v_label", "valid", "(p: Person)"),
        ("v_chain_knows", "valid", "(p: Person)~[:knows]~(f: Person)"),
        ("v_where", "valid", "(p: Person WHERE p.id = 933)"),
        (
            "v_empty_by_data",
            "valid",
            "(p: Person WHERE p.id = 1234567890)",
        ),
        (
            "e_chain4_bad_leaf",
            "empty",
            "(a: Person)~[:knows]~(b: Person)~[:knows]~(c: Person)~[:knows]~(d: Wagumi)",
        ),
        (
            "e_chain_mid_bad",
            "empty",
            "(a: Person)~[:knows]~(b: Wagumi)~[:knows]~(c: Person)",
        ),
        (
            "e_bad_edge_deep",
            "empty",
            "(a: Person)~[:knows]~(b: Person)-[:noSuchEdge]->(c: Person)",
        ),
        (
            "e_ic2_bad_msg",
            "empty",
            "(p: Person)~[:knows]~(friend: Person)<-[:hasCreator]-(m: Wagumi)",
        ),
        (
            "e_conflict_label_deep",
            "empty",
            "(a: Person)~[:knows]~(b: Person)-[:hasCreator]->(b: Comment)",
        ),
        (
            "e_type_mismatch_chain",
            "empty",
            "(a: Person)~[:knows]~(b: Person)~[:knows]~(c: Person WHERE c.firstName = 933)",
        ),
        (
            "e_union_all_bad",
            "empty",
            "(x: Wagumi)<-[:hasCreator]-(c: Comment) | (x: Wagumi)<-[:hasCreator]-(c: Post)",
        ),
        (
            "e_repeat_bad_leaf",
            "empty",
            "(p: Person)~[:knows]~{1,2}(f: Wagumi)",
        ),
        ("e_label_only", "empty", "(x: Wagumi)"),
        (
            "e_type_clash_arith",
            "empty",
            "(p: Person WHERE p.firstName + p.id > 0)",
        ),
        (
            "i_unbound_compound_where",
            "invalid",
            "(p: Person WHERE p.id = 933 AND q.id = 1)",
        ),
        (
            "i_unbound_in_where_chain",
            "invalid",
            "(a: Person)~[:knows]~(b: Person)~[:knows]~(c: Person WHERE q.id = 1)",
        ),
    ];

    println!(
        "\n{:<26}{:>6}{:>9}{:>12}{:>11}{:>10}",
        "case", "exp", "got", "rust_med", "ref(us)", "ref/rust"
    );
    println!("{}", "-".repeat(76));
    let mut logsum = 0.0f64;
    let mut n = 0usize;
    let mut mismatches = 0usize;
    for (id, expected, q) in cases {
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
        for _ in 0..WARMUP {
            black_box(tc.check_query(black_box(&elaborated)));
        }
        let mut samples = Vec::with_capacity(ITERS);
        for _ in 0..ITERS {
            let t = Instant::now();
            black_box(tc.check_query(black_box(&elaborated)));
            samples.push(t.elapsed().as_nanos());
        }
        let rust_med = median_us(samples);
        let bad = if got != *expected {
            mismatches += 1;
            " <-- VERDICT MISMATCH"
        } else {
            ""
        };
        match reference.get(*id) {
            Some(reference_us) => {
                let ratio = reference_us / rust_med;
                println!(
                    "{:<26}{:>6}{:>9}{:>12.2}{:>11.2}{:>10.2}{}",
                    id, expected, got, rust_med, reference_us, ratio, bad
                );
                logsum += ratio.ln();
                n += 1;
            }
            None => println!(
                "{:<26}{:>6}{:>9}{:>12.2}{:>11}{:>10}{}",
                id, expected, got, rust_med, "-", "-", bad
            ),
        }
    }
    println!("{}", "-".repeat(76));
    if n > 0 {
        println!(
            "geomean ref/rust over {n} cases = {:.2}x   (reference slower => rust faster)",
            (logsum / n as f64).exp()
        );
    }
    println!(
        "{}",
        if mismatches == 0 {
            "SANITY OK: rust matched the expected verdict on every case."
        } else {
            "WARNING: verdict mismatch -- investigate before trusting the timings."
        }
    );
}
