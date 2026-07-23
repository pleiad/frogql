//! pattern_typecheck — times the type checker in isolation.
//!
//! Two case populations:
//!   * `bench` — the internal-bench cases reduced to the FPPC path-pattern
//!     surface (MATCH/RETURN dropped, a query-level WHERE inlined into the
//!     descriptor), kept for comparability with earlier runs.
//!   * generated scaling families (`chain_N`, `chain_dir_N`, `anydir_N`,
//!     `union_W`, `repeat_bounded`, `subq`, `anon_unlabeled_N`) that expose
//!     how cost scales with pattern size against a real schema.
//!
//!   cargo run --release --bin pattern_typecheck -- [path.gdb] [out.csv] [--star-schema]
//!
//! `--star-schema` checks against `Schema::star()` instead of the file's
//! active schema — the small-schema control for separating scan cost
//! (O(schema)) from walk cost (O(pattern)).
//!
//! Per case it emits the timed median/min (hot loop, profiling off), one
//! counter snapshot (`typing::stats`), and one per-phase split
//! (`enable_profiling`), and asserts the verdict (valid / empty / invalid)
//! matches the expected classification. It measures only Rust: any
//! comparison against another checker is a separate step that joins this
//! CSV with the other's — this bench does not read either.

use std::fs::File;
use std::hint::black_box;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use frogql::store::lazy::LazyGraphStore;
use frogql::syntax::query::Query;
use frogql::typing::checker::Typechecker;
use frogql::typing::stats;
use frogql::typing::variable_type::Schema;

const WARMUP: usize = 1000;
const ITERS: usize = 10000;
/// Per-case timed-loop budget. Heavy cases (the pathological scaling
/// families can reach hundreds of ms per call) get proportionally fewer
/// iterations instead of stalling the whole run.
const CASE_BUDGET_NS: u128 = 300_000_000;
const MIN_ITERS: usize = 10;
/// Below this median the per-call QPC read is a visible fraction of the
/// measurement; re-time in batches and report per-call = batch / BATCH.
const BATCH_THRESHOLD_NS: u128 = 2000;
const BATCH: usize = 32;
const DEFAULT_GDB: &str = "bench/data/ldbc-sf0.1.gdb";

fn median_min_ns(mut v: Vec<u128>) -> (u128, u128) {
    v.sort_unstable();
    (v[v.len() / 2], v[0])
}

/// Hot-loop timing with the established warmup + median protocol; falls
/// back to batch timing when a case is too fast for per-call resolution.
fn time_case(tc: &mut Typechecker, q: &Query) -> (u128, u128) {
    // Pilot estimate → scale iteration count to the per-case budget.
    let t = Instant::now();
    for _ in 0..3 {
        black_box(tc.check_query(black_box(q)));
    }
    let est = (t.elapsed().as_nanos() / 3).max(1);
    let iters = ((CASE_BUDGET_NS / est) as usize).clamp(MIN_ITERS, ITERS);
    let warmup = (iters / 10).clamp(3, WARMUP);
    for _ in 0..warmup {
        black_box(tc.check_query(black_box(q)));
    }
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        black_box(tc.check_query(black_box(q)));
        samples.push(t.elapsed().as_nanos());
    }
    let (med, min) = median_min_ns(samples);
    if med >= BATCH_THRESHOLD_NS {
        return (med, min);
    }
    let mut samples = Vec::with_capacity(ITERS / BATCH);
    for _ in 0..ITERS / BATCH {
        let t = Instant::now();
        for _ in 0..BATCH {
            black_box(tc.check_query(black_box(q)));
        }
        samples.push(t.elapsed().as_nanos() / BATCH as u128);
    }
    median_min_ns(samples)
}

// --- Generated scaling families (all expected `valid` on the LDBC schema) ---

/// `(p0: Person)~[:knows]~(p1: Person)~...` — N undirected labeled hops.
fn chain_case(n: usize) -> String {
    let mut s = String::from("(p0: Person)");
    for i in 1..=n {
        s.push_str(&format!("~[:knows]~(p{i}: Person)"));
    }
    s
}

/// Directed labeled chain alternating `-[:likes]->` / `-[:hasCreator]->`
/// (Person → Comment → Person → ...). Exercises the edge-scan arm without
/// the Union PathTypes that `~` produces.
fn chain_dir_case(n: usize) -> String {
    let mut s = String::from("(n0: Person)");
    for i in 1..=n {
        if i % 2 == 1 {
            s.push_str(&format!("-[:likes]->(n{i}: Comment)"));
        } else {
            s.push_str(&format!("-[:hasCreator]->(n{i}: Person)"));
        }
    }
    s
}

/// `(p0: Person)-[e1:knows]-(p1: Person)-...` — any-direction hops: each
/// one refines twice (forward + undirected) and unions the PathTypes.
fn anydir_case(n: usize) -> String {
    let mut s = String::from("(p0: Person)");
    for i in 1..=n {
        s.push_str(&format!("-[e{i}:knows]-(p{i}: Person)"));
    }
    s
}

/// `()-[]->()-[]->...` — star descriptors everywhere: the control where
/// label-driven pruning cannot help and every position scans everything.
fn anon_case(n: usize) -> String {
    let mut s = String::from("()");
    for _ in 0..n {
        s.push_str("-[]->()");
    }
    s
}

/// W top-level `|` arms, alternating two valid shapes over shared vars.
fn union_case(w: usize) -> String {
    (0..w)
        .map(|i| {
            if i % 2 == 0 {
                "(a: Person)~[:knows]~(b: Person)"
            } else {
                "(b: Comment)-[:hasCreator]->(a: Person)"
            }
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn main() {
    let mut gdb: Option<String> = None;
    let mut out: Option<String> = None;
    let mut star_schema = false;
    for arg in std::env::args().skip(1) {
        if arg == "--star-schema" {
            star_schema = true;
        } else if gdb.is_none() {
            gdb = Some(arg);
        } else if out.is_none() {
            out = Some(arg);
        } else {
            panic!("unexpected argument: {arg}");
        }
    }
    let gdb = gdb.unwrap_or_else(|| DEFAULT_GDB.to_string());
    let out = out.unwrap_or_else(|| "results_rust_typecheck.csv".to_string());

    let schema = if star_schema {
        Schema::star()
    } else {
        let lazy =
            LazyGraphStore::open(Path::new(&gdb)).unwrap_or_else(|e| panic!("open {gdb}: {e}"));
        let schema = lazy.catalog().active_schema();
        schema
    };
    println!(
        "schema: {} ({} node entries, {} edge entries)",
        if star_schema { "star" } else { gdb.as_str() },
        schema.nodes.len(),
        schema.edges.len()
    );

    // Reduced path-pattern queries, one per internal-bench case (undirected `~`
    // edges, query-level WHERE inlined; the IC and aggregate cases are dropped
    // because they are not path patterns). i_parse is omitted (a parse error has
    // no typecheck to time).
    let bench_cases: &[(&str, &str, &str)] = &[
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

    // (id, category, expected, query)
    let mut cases: Vec<(String, String, String, String)> = bench_cases
        .iter()
        .map(|(id, exp, q)| {
            (
                id.to_string(),
                "bench".into(),
                exp.to_string(),
                q.to_string(),
            )
        })
        .collect();
    for n in [1usize, 2, 4, 8, 16] {
        cases.push((
            format!("chain_{n}"),
            "chain".into(),
            "valid".into(),
            chain_case(n),
        ));
    }
    for n in [1usize, 2, 4, 8, 16] {
        cases.push((
            format!("chain_dir_{n}"),
            "chain_dir".into(),
            "valid".into(),
            chain_dir_case(n),
        ));
    }
    for n in [1usize, 2, 4, 8] {
        cases.push((
            format!("anydir_{n}"),
            "anydir".into(),
            "valid".into(),
            anydir_case(n),
        ));
    }
    for w in [2usize, 4, 8] {
        cases.push((
            format!("union_{w}"),
            "union".into(),
            "valid".into(),
            union_case(w),
        ));
    }
    for n in [1usize, 2, 4, 8, 16] {
        cases.push((
            format!("anon_unlabeled_{n}"),
            "anon".into(),
            "valid".into(),
            anon_case(n),
        ));
    }
    cases.push((
        "repeat_1_3".into(),
        "repeat".into(),
        "valid".into(),
        "(p: Person)~[:knows]~{1,3}(f: Person)".into(),
    ));
    cases.push((
        "repeat_2_4".into(),
        "repeat".into(),
        "valid".into(),
        "(p: Person)~[:knows]~{2,4}(f: Person)".into(),
    ));
    cases.push((
        "subq_exists".into(),
        "subq".into(),
        "valid".into(),
        "MATCH (p: Person)~[:knows]~(f: Person) \
         WHERE EXISTS { MATCH (f)<-[:hasCreator]-(c: Comment) } RETURN p.id"
            .into(),
    ));
    cases.push((
        "multi_optional".into(),
        "subq".into(),
        "valid".into(),
        "MATCH (p: Person)~[:knows]~(f: Person) \
         OPTIONAL MATCH (f)<-[:hasCreator]-(c: Comment) \
         OPTIONAL MATCH (f)<-[:hasCreator]-(m: Post) RETURN p.id"
            .into(),
    ));

    struct Row {
        id: String,
        category: String,
        expected: String,
        got: String,
        med: u128,
        min: u128,
        st: stats::TcStats,
        phases: [u128; 5],
        ok: bool,
    }

    println!(
        "\n{:<26}{:<11}{:>7}{:>9}{:>13}{:>9}{:>11}",
        "case", "category", "exp", "got", "check_med_us", "refines", "edge_scan"
    );
    println!("{}", "-".repeat(86));
    let mut rows: Vec<Row> = Vec::new();
    let mut mismatches = 0usize;
    for (id, category, expected, q) in &cases {
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

        let (med, min) = time_case(&mut tc, &elaborated);

        // One un-timed run for the counter shape, one profiled run for the
        // phase split. Both after the timed loop so they cannot pollute it.
        stats::reset();
        black_box(tc.check_query(black_box(&elaborated)));
        let st = stats::snapshot();
        tc.enable_profiling();
        black_box(tc.check_query(black_box(&elaborated)));
        let p = tc.last_profile().copied().unwrap_or_default();
        let phases = [
            p.pattern_ns,
            p.rep_checks_ns,
            p.group_by_ns,
            p.returns_ns,
            p.order_by_ns,
        ];

        let ok = got == *expected;
        if !ok {
            mismatches += 1;
        }
        println!(
            "{:<26}{:<11}{:>7}{:>9}{:>13.3}{:>9}{:>11}{}",
            id,
            category,
            expected,
            got,
            med as f64 / 1000.0,
            st.refine_calls,
            st.edge_entries_scanned,
            if ok { "" } else { "  <-- VERDICT MISMATCH" }
        );
        rows.push(Row {
            id: id.clone(),
            category: category.clone(),
            expected: expected.clone(),
            got: got.to_string(),
            med,
            min,
            st,
            phases,
            ok,
        });
    }
    println!("{}", "-".repeat(86));

    let mut f = File::create(&out).unwrap_or_else(|e| panic!("create {out}: {e}"));
    writeln!(
        f,
        "case,category,expected,got,check_med_ns,check_min_ns,\
         refine_calls,node_scanned,edge_scanned,refine_to_nodes,\
         env_meets,env_unions,env_outer_joins,env_to_groups,pt_meets,\
         phase_pattern_ns,phase_rep_ns,phase_group_by_ns,phase_returns_ns,phase_order_by_ns,\
         status"
    )
    .unwrap();
    for r in &rows {
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            r.id,
            r.category,
            r.expected,
            r.got,
            r.med,
            r.min,
            r.st.refine_calls,
            r.st.node_entries_scanned,
            r.st.edge_entries_scanned,
            r.st.refine_to_nodes_calls,
            r.st.env_meet_calls,
            r.st.env_union_calls,
            r.st.env_outer_join_calls,
            r.st.env_to_group_calls,
            r.st.pathtype_meet_calls,
            r.phases[0],
            r.phases[1],
            r.phases[2],
            r.phases[3],
            r.phases[4],
            if r.ok { "PASS" } else { "FAIL" }
        )
        .unwrap();
    }
    println!("wrote {out}");
    if star_schema {
        println!(
            "note: star-schema mode — expected verdicts assume the LDBC schema, so \
             schema-driven `empty` cases legitimately read `valid` here ({mismatches} such)."
        );
    } else if mismatches == 0 {
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
