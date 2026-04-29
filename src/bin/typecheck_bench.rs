//! Typechecker benchmark.
//!
//! Three things this measures:
//!
//! 1. **Per-phase wall time of compile.** Parse / elaborate / typecheck
//!    are timed separately so it's visible where time goes inside the
//!    "typecheck" pass.
//!
//! 2. **Typecheck vs runtime.** For valid queries it shows the
//!    typechecker's overhead on top of runtime. For
//!    *guaranteed-empty* queries (rules.md §10 Theorem 6.5) and
//!    *invalid* queries it shows the typechecker rejecting before
//!    runtime would have done the work — the success bar the user
//!    asked for.
//!
//! 3. **Whether the short-circuit actually fires.** Per-query we report
//!    whether the typechecker flagged the query empty, so a regression
//!    that drops the short-circuit would show up as a "ran the runtime
//!    when it shouldn't have" line.
//!
//! Usage:
//!     typecheck_bench <db.gdb> [--iters N]
//!
//! Output: stdout CSV `category;query_id;phase;ns`, stderr summary.

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
use gqlrust::typing::variable_type::Schema;

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

const CASES: &[Case] = &[
    // --- Valid queries ---
    Case {
        category: Category::Valid,
        id: "v_simple",
        query: "MATCH (p: Person) RETURN p.firstName",
    },
    Case {
        category: Category::Valid,
        id: "v_chain2",
        query: "MATCH (p: Person)~[:knows]~(f: Person) RETURN f.firstName",
    },
    Case {
        category: Category::Valid,
        id: "v_chain3",
        query: "MATCH (p: Person)~[:knows]~(f: Person)<-[:hasCreator]-(c: Comment) \
                RETURN c.creationDate",
    },
    // --- Guaranteed empty by typing (label not in schema) ---
    // The DEFAULT schema is inferred from data on import; queries that
    // ask for a label the data doesn't carry refine to ⊥ at typecheck.
    Case {
        category: Category::EmptyByTyping,
        id: "e_unknown_label",
        query: "MATCH (x: Movie) RETURN x.title",
    },
    Case {
        category: Category::EmptyByTyping,
        id: "e_unknown_edge",
        query: "MATCH (p: Person)-[:directed]->(m: Movie) RETURN m.title",
    },
    Case {
        category: Category::EmptyByTyping,
        id: "e_chained_unknown",
        query: "MATCH (p: Person)~[:knows]~(f: Person)-[:directed]->(m: Movie) \
                RETURN m.title",
    },
    // --- Invalid: unbound variable in RETURN ---
    Case {
        category: Category::InvalidUnbound,
        id: "i_unbound",
        query: "MATCH (p: Person) RETURN q.firstName",
    },
    Case {
        category: Category::InvalidUnbound,
        id: "i_unbound_chain",
        query: "MATCH (p: Person)~[:knows]~(f: Person) RETURN q.firstName",
    },
    // --- Invalid: parse error (caught even before typecheck) ---
    Case {
        category: Category::InvalidParse,
        id: "i_parse",
        query: "MATCH (p: Person RETURN p.firstName",
    },
];

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <db.gdb> [--iters N]", args[0]);
        std::process::exit(1);
    }
    let db_path = &args[1];

    let mut iters: usize = 50;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--iters" => {
                iters = args[i + 1].parse().expect("invalid iters");
                i += 2;
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(1);
            }
        }
    }

    eprintln!("Loading {db_path}...");
    let t0 = Instant::now();
    let store = LazyGraphStore::open(Path::new(db_path)).expect("open .gdb");
    eprintln!(
        "  {} nodes / {} edges in {:.2}s",
        store.node_count(),
        store.edge_count(),
        t0.elapsed().as_secs_f64()
    );
    let active = store.catalog().active_schema();
    let rt = Runtime::new(&store);

    println!("category;case;phase;iter;ns;empty_flag");
    eprintln!();
    eprintln!(
        "{:<5} {:<22} {:>10} {:>10} {:>10} {:>10} {:>8} {:>8}",
        "cat", "case", "parse_us", "elab_us", "tc_us", "rt_ms", "empty?", "saved?"
    );
    eprintln!("{}", "-".repeat(90));

    for case in CASES {
        let mut parse_samples: Vec<u128> = Vec::with_capacity(iters);
        let mut elab_samples: Vec<u128> = Vec::with_capacity(iters);
        let mut tc_samples: Vec<u128> = Vec::with_capacity(iters);
        let mut rt_samples: Vec<u128> = Vec::with_capacity(iters);
        let mut empty_flag = false;
        let mut runtime_skipped = false;

        for n in 0..iters {
            // Parse.
            let t = Instant::now();
            let parsed = parser::parse_query(case.query);
            let parse_ns = t.elapsed().as_nanos();
            parse_samples.push(parse_ns);
            println!(
                "{};{};parse;{};{};",
                case.category.label(),
                case.id,
                n,
                parse_ns
            );

            let ast = match parsed {
                Ok(a) => a,
                Err(_) => {
                    // Parse error case — no further phases. Still emit
                    // zeroed lines for downstream analyzers.
                    println!("{};{};elab;{};0;", case.category.label(), case.id, n);
                    println!("{};{};tc;{};0;", case.category.label(), case.id, n);
                    println!("{};{};rt;{};0;", case.category.label(), case.id, n);
                    elab_samples.push(0);
                    tc_samples.push(0);
                    rt_samples.push(0);
                    runtime_skipped = true;
                    continue;
                }
            };

            // Elaborate.
            let t = Instant::now();
            let elab = elaborate::elaborate_query(ast);
            let elab_ns = t.elapsed().as_nanos();
            elab_samples.push(elab_ns);
            println!(
                "{};{};elab;{};{};",
                case.category.label(),
                case.id,
                n,
                elab_ns
            );

            // Typecheck (against active schema).
            let t = Instant::now();
            let mut tc = Typechecker::new(active.clone());
            let r = tc.check_query(&elab);
            let tc_ns = t.elapsed().as_nanos();
            tc_samples.push(tc_ns);
            println!("{};{};tc;{};{};", case.category.label(), case.id, n, tc_ns);

            empty_flag = r.empty;
            let typecheck_rejects = !r.ok;
            // Runtime — skip when typecheck says guaranteed-empty
            // (rules.md §10 Theorem 6.5) or has hard errors.
            if r.empty || typecheck_rejects {
                runtime_skipped = true;
                rt_samples.push(0);
                println!("{};{};rt;{};0;skipped", case.category.label(), case.id, n);
                continue;
            }

            // Optimize then run.
            let optimized = Query {
                pattern: optimizer::compile(elab.pattern.clone()),
                ..elab
            };
            let t = Instant::now();
            let _ = rt.run_query(&optimized, 100);
            let rt_ns = t.elapsed().as_nanos();
            rt_samples.push(rt_ns);
            println!("{};{};rt;{};{};", case.category.label(), case.id, n, rt_ns);
        }

        let parse_med = median(&parse_samples);
        let elab_med = median(&elab_samples);
        let tc_med = median(&tc_samples);
        let rt_med = median(&rt_samples);

        eprintln!(
            "{:<5} {:<22} {:>10.2} {:>10.2} {:>10.2} {:>10.2} {:>8} {:>8}",
            case.category.label(),
            case.id,
            parse_med as f64 / 1_000.0,
            elab_med as f64 / 1_000.0,
            tc_med as f64 / 1_000.0,
            rt_med as f64 / 1_000_000.0,
            if empty_flag { "yes" } else { "no" },
            if runtime_skipped { "yes" } else { "no" },
        );
    }
}

fn median(samples: &[u128]) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    s[s.len() / 2]
}
