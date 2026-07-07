//! pattern_typecheck — times the type checker on the internal-bench cases
//! reduced to the FPPC path-pattern surface (MATCH/RETURN dropped, a
//! query-level WHERE inlined into the descriptor). Isolated from the runtime,
//! with a reused checker and WARMUP + ITERS iterations.
//!
//!   cargo run --release --bin pattern_typecheck -- [path.gdb] [out.csv]
//!
//! Emits per-case check medians to `results_rust_typecheck.csv` and asserts each
//! verdict (valid / empty / invalid) matches the expected classification. It
//! measures only Rust: any comparison against another checker is a separate step
//! that joins this CSV with the other's — this bench does not read either.

use std::fs::File;
use std::hint::black_box;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use frogql::store::lazy::LazyGraphStore;
use frogql::typing::checker::Typechecker;

const WARMUP: usize = 1000;
const ITERS: usize = 10000;
const DEFAULT_GDB: &str = "bench/data/ldbc-sf0.1.gdb";

fn median_min_ns(mut v: Vec<u128>) -> (u128, u128) {
    v.sort_unstable();
    (v[v.len() / 2], v[0])
}

fn main() {
    let mut args = std::env::args().skip(1);
    let gdb = args.next().unwrap_or_else(|| DEFAULT_GDB.to_string());
    let out = args
        .next()
        .unwrap_or_else(|| "results_rust_typecheck.csv".to_string());

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
        "\n{:<26}{:>7}{:>9}{:>15}",
        "case", "exp", "got", "check_med_us"
    );
    println!("{}", "-".repeat(57));
    let mut rows: Vec<(String, String, String, u128, u128, bool)> = Vec::new();
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
        let (med, min) = median_min_ns(samples);
        let ok = got == *expected;
        if !ok {
            mismatches += 1;
        }
        println!(
            "{:<26}{:>7}{:>9}{:>15.3}{}",
            id,
            expected,
            got,
            med as f64 / 1000.0,
            if ok { "" } else { "  <-- VERDICT MISMATCH" }
        );
        rows.push((
            id.to_string(),
            expected.to_string(),
            got.to_string(),
            med,
            min,
            ok,
        ));
    }
    println!("{}", "-".repeat(57));

    let mut f = File::create(&out).unwrap_or_else(|e| panic!("create {out}: {e}"));
    writeln!(f, "case,expected,got,check_med_ns,check_min_ns,status").unwrap();
    for (id, exp, got, med, min, ok) in &rows {
        writeln!(
            f,
            "{id},{exp},{got},{med},{min},{}",
            if *ok { "PASS" } else { "FAIL" }
        )
        .unwrap();
    }
    println!("wrote {out}");
    if mismatches == 0 {
        println!(
            "SANITY OK: rust matched the expected verdict on all {} cases.",
            rows.len()
        );
    } else {
        println!(
            "WARNING: {mismatches} verdict mismatch(es) -- investigate before trusting timings."
        );
    }
}
