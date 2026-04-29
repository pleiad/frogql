//! Typechecker benchmark.
//!
//! Compares typechecker decision time vs runtime work, per query, per
//! database. Self-contained: runs both the *checked* path (with
//! short-circuit on §10 Theorem 6.5 emptiness) and the *unchecked* path
//! (parse → elaborate → optimize → run, no typecheck) so the would-be-
//! runtime number is captured by the bench rather than measured by hand.
//!
//! Usage:
//!     typecheck_bench [--iters N] <db1.gdb> [<db2.gdb> ...]
//!
//! Multiple `.gdb` arguments are encouraged — typechecker decision
//! time is roughly schema-bound (refinement walks schema entries) and
//! data-independent, while runtime cost scales with data size, so the
//! ratio between the two is a meaningful axis to sweep. See
//! `bench/TYPECHECKER_BENCHMARK.md` for the full discussion.

use std::env;
use std::path::Path;
use std::time::Instant;

use gqlrust::elaborate;
use gqlrust::optimizer;
use gqlrust::parser;
use gqlrust::runtime::engine::Runtime;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::query::Query;
use gqlrust::typing::checker::Typechecker;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    Valid,
    EmptyByTyping,
    InvalidUnbound,
    InvalidParse,
}

impl Category {
    fn label(&self) -> &'static str {
        match self {
            Category::Valid => "valid",
            Category::EmptyByTyping => "empty",
            Category::InvalidUnbound => "invalid_unbound",
            Category::InvalidParse => "invalid_parse",
        }
    }
}

struct Case {
    category: Category,
    id: &'static str,
    query: &'static str,
}

/// Cases assume both LDBC and grandstack schemas: Person is in LDBC,
/// User is in grandstack; "Wagumi" is gibberish guaranteed to be in
/// neither, used for label-not-in-schema empty-by-typing tests.
/// Valid queries use generic shapes that hit each schema's labels;
/// the bench skips per-DB cases where the labels aren't present.
const CASES: &[Case] = &[
    // --- Valid (label-only, runs against any schema with that label) ---
    Case {
        category: Category::Valid,
        id: "v_label_person",
        query: "MATCH (p: Person) RETURN p.firstName",
    },
    Case {
        category: Category::Valid,
        id: "v_label_user",
        query: "MATCH (u: User) RETURN u.name",
    },
    Case {
        category: Category::Valid,
        id: "v_chain_knows",
        query: "MATCH (p: Person)~[:knows]~(f: Person) RETURN f.firstName",
    },
    Case {
        category: Category::Valid,
        id: "v_chain_wrote",
        query: "MATCH (u: User)-[:WROTE]->(r: Review) RETURN r.stars",
    },
    // --- Guaranteed empty by typing (label not in any real schema) ---
    Case {
        category: Category::EmptyByTyping,
        id: "e_unknown_label",
        query: "MATCH (x: Wagumi) RETURN x.name",
    },
    Case {
        category: Category::EmptyByTyping,
        id: "e_unknown_edge_lhs",
        query: "MATCH (x: Wagumi)-[:e]->(y: Frobnicate) RETURN y.name",
    },
    Case {
        category: Category::EmptyByTyping,
        id: "e_chained_unknown",
        query: "MATCH (a: Person)~[:knows]~(b: Person)-[:e]->(c: Wagumi) \
                RETURN c.name",
    },
    // --- Invalid: unbound variable in RETURN ---
    Case {
        category: Category::InvalidUnbound,
        id: "i_unbound",
        query: "MATCH (p) RETURN q.name",
    },
    Case {
        category: Category::InvalidUnbound,
        id: "i_unbound_chain",
        query: "MATCH (p)~[:e]~(f) RETURN q.name",
    },
    // --- Invalid: parse error (caught even before typecheck) ---
    Case {
        category: Category::InvalidParse,
        id: "i_parse",
        query: "MATCH (p RETURN p.name",
    },
];

fn main() {
    let args: Vec<String> = env::args().collect();

    let mut iters: usize = 30;
    let mut warmup: usize = 1;
    let mut limit: usize = 100;
    let mut db_paths: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--iters" => {
                iters = args[i + 1].parse().expect("invalid iters");
                i += 2;
            }
            "--warmup" => {
                warmup = args[i + 1].parse().expect("invalid warmup");
                i += 2;
            }
            "--limit" => {
                limit = args[i + 1].parse().expect("invalid limit");
                i += 2;
            }
            arg if !arg.starts_with("--") => {
                db_paths.push(arg.to_string());
                i += 1;
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(1);
            }
        }
    }
    if db_paths.is_empty() {
        eprintln!(
            "Usage: {} [--iters N] [--warmup N] [--limit N] <db1.gdb> [<db2.gdb> ...]",
            args[0]
        );
        std::process::exit(1);
    }

    println!("db;category;case;phase;iter;ns;flags");

    for db_path in &db_paths {
        bench_db(db_path, iters, warmup, limit);
    }
}

fn bench_db(db_path: &str, iters: usize, warmup: usize, limit: usize) {
    eprintln!("\n=== {db_path} ===");
    let t0 = Instant::now();
    let store = LazyGraphStore::open(Path::new(db_path)).expect("open .gdb");
    let n_nodes = store.node_count();
    let n_edges = store.edge_count();
    eprintln!(
        "  {n_nodes} nodes / {n_edges} edges in {:.2}s",
        t0.elapsed().as_secs_f64()
    );
    let active = store.catalog().active_schema();
    let rt = Runtime::new(&store);

    eprintln!(
        "{:<6} {:<22} {:>9} {:>9} {:>9} {:>10} {:>11} {:>8} {:>10}",
        "cat",
        "case",
        "parse_us",
        "elab_us",
        "tc_us",
        "rt_chk_ms",
        "rt_unchk_ms",
        "empty?",
        "speedup"
    );
    eprintln!("{}", "-".repeat(110));

    for case in CASES {
        run_case(
            db_path, &active, &rt, &store, case, iters, warmup, limit, n_nodes, n_edges,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn run_case<'g>(
    db_path: &str,
    active: &gqlrust::typing::variable_type::Schema,
    rt: &Runtime<'g, LazyGraphStore>,
    _store: &LazyGraphStore,
    case: &Case,
    iters: usize,
    warmup: usize,
    limit: usize,
    _n_nodes: u32,
    _n_edges: u32,
) {
    let mut parse_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut elab_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut tc_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut rt_chk_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut rt_unchk_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut empty_flag = false;
    let mut typecheck_rejected;

    let total = iters + warmup;
    for n in 0..total {
        let is_warmup = n < warmup;

        // Parse.
        let t = Instant::now();
        let parsed = parser::parse_query(case.query);
        let parse_ns = t.elapsed().as_nanos();
        if !is_warmup {
            parse_samples.push(parse_ns);
            println!(
                "{};{};{};parse;{};{};",
                db_path,
                case.category.label(),
                case.id,
                n - warmup,
                parse_ns
            );
        }

        let ast = match parsed {
            Ok(a) => a,
            Err(_) => {
                // Parse error → no further phases possible. Both
                // checked and unchecked paths fail at the same point.
                if !is_warmup {
                    elab_samples.push(0);
                    tc_samples.push(0);
                    rt_chk_samples.push(0);
                    rt_unchk_samples.push(0);
                    println!(
                        "{};{};{};elab;{};0;skipped",
                        db_path,
                        case.category.label(),
                        case.id,
                        n - warmup
                    );
                    println!(
                        "{};{};{};tc;{};0;skipped",
                        db_path,
                        case.category.label(),
                        case.id,
                        n - warmup
                    );
                    println!(
                        "{};{};{};rt_chk;{};0;skipped",
                        db_path,
                        case.category.label(),
                        case.id,
                        n - warmup
                    );
                    println!(
                        "{};{};{};rt_unchk;{};0;skipped",
                        db_path,
                        case.category.label(),
                        case.id,
                        n - warmup
                    );
                }
                continue;
            }
        };

        // Elaborate.
        let t = Instant::now();
        let elab = elaborate::elaborate_query(ast);
        let elab_ns = t.elapsed().as_nanos();
        if !is_warmup {
            elab_samples.push(elab_ns);
            println!(
                "{};{};{};elab;{};{};",
                db_path,
                case.category.label(),
                case.id,
                n - warmup,
                elab_ns
            );
        }

        // Typecheck. The schema clone is intentionally OUTSIDE the
        // timed region — a Typechecker takes its schema by-value, so
        // counting the clone would charge the typecheck phase for
        // setup work that's irrelevant to its asymptotic cost. In
        // production, the schema is owned by the catalog and reused.
        let schema_for_tc = active.clone();
        let t = Instant::now();
        let mut tc = Typechecker::new(schema_for_tc);
        let r = tc.check_query(&elab);
        let tc_ns = t.elapsed().as_nanos();
        if !is_warmup {
            tc_samples.push(tc_ns);
            println!(
                "{};{};{};tc;{};{};",
                db_path,
                case.category.label(),
                case.id,
                n - warmup,
                tc_ns
            );
        }
        empty_flag = r.empty;
        typecheck_rejected = !r.ok;

        // Checked runtime path (skip if typecheck short-circuits).
        let rt_chk_ns = if r.empty || typecheck_rejected {
            0
        } else {
            let optimized = Query {
                pattern: optimizer::compile(elab.pattern.clone()),
                ..elab.clone()
            };
            let t = Instant::now();
            let _ = rt.run_query(&optimized, limit);
            t.elapsed().as_nanos()
        };
        if !is_warmup {
            rt_chk_samples.push(rt_chk_ns);
            println!(
                "{};{};{};rt_chk;{};{};{}",
                db_path,
                case.category.label(),
                case.id,
                n - warmup,
                rt_chk_ns,
                if r.empty || typecheck_rejected {
                    "skipped"
                } else {
                    ""
                },
            );
        }

        // Unchecked runtime path — always runs, captures what the
        // runtime would have done if the typechecker hadn't rejected.
        // This is the "would-be runtime" the success-bar comparison
        // claims; running it inline so the bench is self-contained.
        let optimized = Query {
            pattern: optimizer::compile(elab.pattern.clone()),
            ..elab
        };
        let t = Instant::now();
        let _ = rt.run_query(&optimized, limit);
        let rt_unchk_ns = t.elapsed().as_nanos();
        if !is_warmup {
            rt_unchk_samples.push(rt_unchk_ns);
            println!(
                "{};{};{};rt_unchk;{};{};",
                db_path,
                case.category.label(),
                case.id,
                n - warmup,
                rt_unchk_ns
            );
        }
    }

    let parse_med = median(&parse_samples);
    let elab_med = median(&elab_samples);
    let tc_med = median(&tc_samples);
    let rt_chk_med = median(&rt_chk_samples);
    let rt_unchk_med = median(&rt_unchk_samples);

    let total_tc_ns = parse_med + elab_med + tc_med;
    let speedup = if total_tc_ns > 0 && rt_unchk_med > 0 {
        rt_unchk_med as f64 / total_tc_ns as f64
    } else {
        0.0
    };
    let speedup_str = if speedup > 0.0 {
        format!("{:>8.1}x", speedup)
    } else {
        "—".to_string()
    };

    eprintln!(
        "{:<6} {:<22} {:>9.2} {:>9.2} {:>9.2} {:>10.2} {:>11.2} {:>8} {:>10}",
        case.category.label(),
        case.id,
        parse_med as f64 / 1_000.0,
        elab_med as f64 / 1_000.0,
        tc_med as f64 / 1_000.0,
        rt_chk_med as f64 / 1_000_000.0,
        rt_unchk_med as f64 / 1_000_000.0,
        if empty_flag { "yes" } else { "no" },
        speedup_str,
    );
}

fn median(samples: &[u128]) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    s[s.len() / 2]
}
