//! Criterion bench for the typechecker pipeline phases.
//!
//! Complements `src/bin/typecheck_bench.rs`:
//!
//! - `src/bin/typecheck_bench.rs` is a *practical* tool: takes multiple
//!   `--db` args, opens real `.gdb` files, runs the runtime, emits raw
//!   per-iter CSV. Better for sweeping data sizes and capturing the
//!   typechecker-vs-runtime ratio against real workloads.
//!
//! - This file is the *statistically rigorous* harness: criterion
//!   handles warmup, outlier detection, confidence intervals, and
//!   compares against a saved baseline. Better for catching regressions
//!   in the typechecker phases themselves at the microsecond scale,
//!   where ad-hoc `Instant::now()` jitter dominates.
//!
//! No data dependency — uses `Schema::star()` so it can run in any
//! checkout without first building a dataset.
//!
//! Usage:
//!   cargo bench --bench typecheck                   # run all
//!   cargo bench --bench typecheck -- valid_label    # filter
//!   cargo bench --bench typecheck -- --save-baseline v1
//!   cargo bench --bench typecheck -- --baseline v1  # compare
//!
//! See `bench/TYPECHECKER_BENCHMARK.md` for the corresponding
//! discussion + the headline finding (typechecker is never slower
//! than what it would have skipped).

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use gqlrust::elaborate;
use gqlrust::optimizer;
use gqlrust::parser;
use gqlrust::typing::checker::Typechecker;
use gqlrust::typing::variable_type::Schema;

/// Same case set as `src/bin/typecheck_bench.rs`. Categories follow the
/// same vocabulary so a regression here corresponds to one in the CSV
/// bench too.
const CASES: &[(&str, &str)] = &[
    ("valid_label_person", "MATCH (p: Person) RETURN p.firstName"),
    ("valid_label_user", "MATCH (u: User) RETURN u.name"),
    (
        "valid_chain_knows",
        "MATCH (p: Person)~[:knows]~(f: Person) RETURN f.firstName",
    ),
    (
        "valid_chain_wrote",
        "MATCH (u: User)-[:WROTE]->(r: Review) RETURN r.stars",
    ),
    (
        "valid_chain4",
        "MATCH (a: Person)~[:knows]~(b: Person)~[:knows]~(c: Person)~[:knows]~(d: Person) \
         RETURN d.firstName",
    ),
    (
        "valid_union_arm",
        "MATCH (p: Person)<-[:hasCreator]-(c: Comment) | \
         (p: Person)<-[:hasCreator]-(c: Post) RETURN c.id",
    ),
    (
        "valid_where_eq",
        "MATCH (p: Person) WHERE p.id = 933 RETURN p.firstName",
    ),
    ("empty_unknown_label", "MATCH (x: Wagumi) RETURN x.name"),
    (
        "empty_unknown_edge_lhs",
        "MATCH (x: Wagumi)-[:e]->(y: Frobnicate) RETURN y.name",
    ),
    (
        "empty_chained_unknown",
        "MATCH (a: Person)~[:knows]~(b: Person)-[:e]->(c: Wagumi) RETURN c.name",
    ),
    (
        "empty_unknown_mid_edge",
        "MATCH (a: Person)-[:noSuchEdge]->(b: Person) RETURN b.firstName",
    ),
    ("invalid_unbound", "MATCH (p) RETURN q.name"),
    ("invalid_unbound_chain", "MATCH (p)~[:e]~(f) RETURN q.name"),
    ("invalid_parse", "MATCH (p RETURN p.name"),
];

/// Bench just the parse phase. Microseconds territory; criterion's
/// statistical handling matters most here.
fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse");
    for (id, query) in CASES {
        group.bench_with_input(BenchmarkId::from_parameter(id), query, |b, q| {
            b.iter(|| {
                let _ = parser::parse_query(black_box(q));
            });
        });
    }
    group.finish();
}

/// Bench parse + elaborate. Skips the parse-failure case (it can't
/// elaborate). Diff against `parse` gives the elab phase cost.
fn bench_elaborate(c: &mut Criterion) {
    let mut group = c.benchmark_group("elaborate");
    for (id, query) in CASES {
        if *id == "invalid_parse" {
            continue;
        }
        group.bench_with_input(BenchmarkId::from_parameter(id), query, |b, q| {
            b.iter(|| {
                let ast = parser::parse_query(black_box(q)).unwrap();
                let _ = elaborate::elaborate_query(ast);
            });
        });
    }
    group.finish();
}

/// Bench parse + elaborate + typecheck. The full compile-side pipeline
/// without the optimizer. Diff against `elaborate` gives the
/// typecheck-phase cost — the headline number for the
/// "typechecker is faster than runtime" claim.
fn bench_typecheck(c: &mut Criterion) {
    let mut group = c.benchmark_group("typecheck");
    let schema = Schema::star();
    for (id, query) in CASES {
        if *id == "invalid_parse" {
            continue;
        }
        group.bench_with_input(BenchmarkId::from_parameter(id), query, |b, q| {
            b.iter(|| {
                let ast = parser::parse_query(black_box(q)).unwrap();
                let elab = elaborate::elaborate_query(ast);
                let mut tc = Typechecker::new(schema.clone());
                let _ = tc.check_query(&elab);
            });
        });
    }
    group.finish();
}

/// Full compile pipeline including optimize. Matches what the CSV
/// bench measures end-to-end on the checked path (sans runtime).
fn bench_compile_full(c: &mut Criterion) {
    let mut group = c.benchmark_group("compile_full");
    let schema = Schema::star();
    for (id, query) in CASES {
        if *id == "invalid_parse" {
            continue;
        }
        group.bench_with_input(BenchmarkId::from_parameter(id), query, |b, q| {
            b.iter(|| {
                let ast = parser::parse_query(black_box(q)).unwrap();
                let elab = elaborate::elaborate_query(ast);
                let mut tc = Typechecker::new(schema.clone());
                let _ = tc.check_query(&elab);
                let _ = optimizer::compile(elab.collapsed_pattern());
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_parse,
    bench_elaborate,
    bench_typecheck,
    bench_compile_full
);
criterion_main!(benches);
