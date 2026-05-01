//! Typechecker benchmark.
//!
//! Compares wall time of the LDBC SF0.1 case set with vs without the
//! typechecker. The typechecker can short-circuit doomed queries
//! (empty-by-typing, free variable, type mismatch) before the runtime
//! ever runs — see rules.md §10 Theorem 6.5 for the soundness
//! argument behind the guaranteed-empty short-circuit. This bench
//! measures both paths per case and reports the difference.
//!
//! Usage:
//!
//!     ./target/release/bench_setup       # one-time: download + build .gdb
//!     ./target/release/typecheck_bench   # run the bench
//!
//! Flags: `--iters N` (default 3) and `--warmup N` (default 1). No
//! dataset path arg — opens `bench/data/ldbc-sf0.1.gdb` from a
//! hardcoded path; case set references LDBC labels by name.
//!
//! See `bench/TYPECHECKER_BENCHMARK.md` for output format, case set
//! details, and stream cross-checking.

use std::env;
use std::path::Path;
use std::time::Instant;

use gqlrust::runtime::engine::Runtime;
use gqlrust::store::lazy::LazyGraphStore;

// ---------------------------------------------------------------------------

const SF01_GDB: &str = "bench/data/ldbc-sf0.1.gdb";

// Default of 3 matches the LDBC bench. The slowest doomed cases
// take ~50s/iter on the unchecked path, so 30 iters would be a
// ~1.5h run; 3 keeps a default invocation under ~15 min.
const DEFAULT_ITERS: usize = 3;
const DEFAULT_WARMUP: usize = 1;
// Result-row cap per runtime call.
const LIMIT: usize = 100;

// CSV stdout: db;category;case;phase;iter;ns;flags
// See bench/TYPECHECKER_BENCHMARK.md "Output" for the column and
// flag vocabulary.

// ---------------------------------------------------------------------------

/// Case author's claim about what the typechecker should produce
/// against the LDBC schema. Drives the `cat` column and the soundness
/// check (actual outcome != `expected_outcome()` ⇒ regression).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    /// typecheck passes, runtime runs.
    Valid,
    /// typechecker says guaranteed-empty.
    EmptyByTyping,
    /// compile pipeline rejects (free var, type mismatch, parse fail).
    InvalidRejected,
}

impl Category {
    fn label(&self) -> &'static str {
        match self {
            Category::Valid => "valid",
            Category::EmptyByTyping => "empty",
            Category::InvalidRejected => "invalid",
        }
    }
    fn expected_outcome(&self) -> Outcome {
        match self {
            Category::Valid => Outcome::Ok,
            Category::EmptyByTyping => Outcome::Empty,
            Category::InvalidRejected => Outcome::Rejected,
        }
    }
}

/// What actually happened in a run.
///
/// - `Ok`: typecheck passed, not statically empty.
/// - `Empty`: typecheck passed, statically empty (guaranteed-empty
///   short-circuit applies — runtime is skipped in production).
/// - `Rejected`: rejected by typechecker or by parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Ok,
    Empty,
    Rejected,
}

impl Outcome {
    fn label(&self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Empty => "empty",
            Outcome::Rejected => "rejected",
        }
    }
}

struct Case {
    category: Category,
    id: &'static str,
    query: &'static str,
}

/// 18 cases: 3 valid controls + 9 empty-by-typing + 6 invalid (rejected).
/// See `bench/TYPECHECKER_BENCHMARK.md` for the case-set description.
const CASES: &[Case] = &[
    // ---- Valid controls (3) ----
    Case {
        category: Category::Valid,
        id: "v_label",
        query: "MATCH (p: Person) RETURN p.firstName",
    },
    Case {
        category: Category::Valid,
        id: "v_chain_knows",
        query: "MATCH (p: Person)~[:knows]~(f: Person) RETURN f.firstName",
    },
    Case {
        category: Category::Valid,
        id: "v_where",
        query: "MATCH (p: Person) WHERE p.id = 933 RETURN p.firstName",
    },
    // ---- Empty by typing (9) ----
    Case {
        category: Category::EmptyByTyping,
        id: "e_chain4_bad_leaf",
        query: "MATCH (a: Person)~[:knows]~(b: Person)\
                ~[:knows]~(c: Person)~[:knows]~(d: Wagumi) \
                RETURN d.id",
    },
    Case {
        category: Category::EmptyByTyping,
        id: "e_chain_mid_bad",
        query: "MATCH (a: Person)~[:knows]~(b: Wagumi)~[:knows]~(c: Person) \
                RETURN c.firstName",
    },
    Case {
        category: Category::EmptyByTyping,
        id: "e_bad_edge_deep",
        query: "MATCH (a: Person)~[:knows]~(b: Person)\
                -[:noSuchEdge]->(c: Person) \
                RETURN c.firstName",
    },
    Case {
        category: Category::EmptyByTyping,
        id: "e_ic2_bad_msg",
        query: "MATCH (p: Person)~[:knows]~(friend: Person)\
                <-[:hasCreator]-(m: Wagumi) \
                RETURN m.id",
    },
    Case {
        category: Category::EmptyByTyping,
        id: "e_conflict_label_deep",
        query: "MATCH (a: Person)~[:knows]~(b: Person)\
                -[:hasCreator]->(b: Comment) \
                RETURN b.id",
    },
    Case {
        category: Category::EmptyByTyping,
        id: "e_type_mismatch_chain",
        query: "MATCH (a: Person)~[:knows]~(b: Person)~[:knows]~(c: Person) \
                WHERE c.firstName = 933 \
                RETURN c.id",
    },
    Case {
        category: Category::EmptyByTyping,
        id: "e_union_all_bad",
        query: "MATCH (x: Wagumi)<-[:hasCreator]-(c: Comment) | \
                (x: Wagumi)<-[:hasCreator]-(c: Post) \
                RETURN c.id",
    },
    // {1,2} not the spec-faithful {1,3}: latter OOMs the unchecked runtime on SF0.1.
    Case {
        category: Category::EmptyByTyping,
        id: "e_repeat_bad_leaf",
        query: "MATCH (p: Person)~[:knows]~{1,2}(f: Wagumi) RETURN f.id",
    },
    Case {
        category: Category::EmptyByTyping,
        id: "e_label_only",
        query: "MATCH (x: Wagumi) RETURN x.id",
    },
    // ---- Invalid: rejected by the compile pipeline (6) ----
    Case {
        category: Category::InvalidRejected,
        id: "i_unbound_after_chain4",
        query: "MATCH (a: Person)~[:knows]~(b: Person)\
                ~[:knows]~(c: Person)~[:knows]~(d: Person) \
                RETURN q.id",
    },
    Case {
        category: Category::InvalidRejected,
        id: "i_unbound_in_where_chain",
        query: "MATCH (a: Person)~[:knows]~(b: Person)~[:knows]~(c: Person) \
                WHERE q.id = 1 \
                RETURN c.firstName",
    },
    Case {
        category: Category::InvalidRejected,
        id: "i_unbound_in_union",
        query: "MATCH (a: Person)<-[:hasCreator]-(c: Comment) | \
                (a: Person)<-[:hasCreator]-(c: Post) \
                RETURN q.id",
    },
    Case {
        category: Category::InvalidRejected,
        id: "i_unbound_compound_where",
        query: "MATCH (p: Person) WHERE p.id = 933 AND q.id = 1 \
                RETURN p.firstName",
    },
    Case {
        category: Category::InvalidRejected,
        id: "i_unbound_simple",
        query: "MATCH (p) RETURN q.name",
    },
    Case {
        category: Category::InvalidRejected,
        id: "i_parse",
        query: "MATCH (p RETURN p.name",
    },
];

// ---------------------------------------------------------------------------
// Setup + main.

fn ensure_dataset() {
    if !Path::new(SF01_GDB).exists() {
        eprintln!(
            "Required dataset missing: {SF01_GDB}\n\
             Run `./target/release/bench_setup` first to download and build it."
        );
        std::process::exit(1);
    }
}

fn print_usage(prog: &str) {
    eprintln!(
        "Usage: {prog} [--iters N] [--warmup N]\n\
         \n\
         Defaults: --iters {DEFAULT_ITERS}  --warmup {DEFAULT_WARMUP}\n\
         \n\
         Dataset: bench/data/ldbc-sf0.1.gdb (run ./target/release/bench_setup\n\
         once to build it)."
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut iters = DEFAULT_ITERS;
    let mut warmup = DEFAULT_WARMUP;
    let mut i = 1;
    let need_value = |i: usize, flag: &str| -> &str {
        if i + 1 >= args.len() {
            eprintln!("{flag} requires a value");
            std::process::exit(1);
        }
        &args[i + 1]
    };
    while i < args.len() {
        match args[i].as_str() {
            "--iters" => {
                iters = need_value(i, "--iters").parse().unwrap_or_else(|e| {
                    eprintln!("invalid --iters: {e}");
                    std::process::exit(1);
                });
                i += 2;
            }
            "--warmup" => {
                warmup = need_value(i, "--warmup").parse().unwrap_or_else(|e| {
                    eprintln!("invalid --warmup: {e}");
                    std::process::exit(1);
                });
                i += 2;
            }
            "-h" | "--help" => {
                print_usage(&args[0]);
                return;
            }
            other => {
                eprintln!("unknown arg: {other}");
                print_usage(&args[0]);
                std::process::exit(1);
            }
        }
    }
    if iters == 0 {
        eprintln!("--iters must be >= 1");
        std::process::exit(1);
    }

    ensure_dataset();

    println!("db;category;case;phase;iter;ns;flags");
    bench_db(Path::new(SF01_GDB), iters, warmup);
}

fn bench_db(db_path: &Path, iters: usize, warmup: usize) {
    let path_str = db_path.display().to_string();
    eprintln!("\n=== {path_str} ===");
    let t0 = Instant::now();
    let store = LazyGraphStore::open(db_path).unwrap_or_else(|e| {
        eprintln!("failed to open .gdb {path_str}: {e}");
        std::process::exit(1);
    });
    eprintln!(
        "  {} nodes / {} edges in {:.2}s",
        store.node_count(),
        store.edge_count(),
        t0.elapsed().as_secs_f64()
    );
    let active = store.catalog().active_schema();
    let rt = Runtime::new(&store);

    const TABLE_WIDTH: usize = 103; // sum of the format widths below + separators
    eprintln!(
        "{:<10} {:<28} {:>13} {:>15} {:>11} {:>9} {:>11}",
        "cat", "case", "compile_chk_us", "compile_unchk_us", "rt_unchk_ms", "outcome", "tc_impact",
    );
    eprintln!(
        "  (tc_impact: Yx on empty/rejected = (compile_unchk + rt_unchk) / compile_chk; \
         ±X% on ok = (compile_chk - compile_unchk) / (compile_unchk + rt_unchk); — for parse fail)"
    );
    eprintln!("{}", "-".repeat(TABLE_WIDTH));

    let mut warned_count = 0usize;
    for case in CASES {
        if run_case(&path_str, &active, &rt, case, iters, warmup) {
            warned_count += 1;
        }
    }
    eprintln!("{}", "-".repeat(TABLE_WIDTH));
    if warned_count == 0 {
        eprintln!(
            "Soundness: clean. {}/{} cases produced their expected outcome and no \
             empty case had a non-zero rt_unchk row count.",
            CASES.len(),
            CASES.len(),
        );
    } else {
        eprintln!(
            "⚠  Soundness: {}/{} cases tripped a SOUNDNESS warning (see ⚠ lines above). \
             Each is a regression on the LDBC-paired case set (typechecker, parser, \
             or schema inference) — investigate before trusting the rest of the table.",
            warned_count,
            CASES.len(),
        );
    }
}

/// Run a single case for `iters + warmup` iterations, emitting per-iter
/// CSV rows + a human summary line. Returns `true` if the case tripped
/// a soundness warning (outcome ≠ expected, OR `Outcome::Empty` with a
/// non-zero rt_unchk row count). Caller tallies these for the per-DB
/// summary line at the bottom of the table.
fn run_case(
    db_path: &str,
    active: &gqlrust::typing::variable_type::Schema,
    rt: &Runtime<'_, LazyGraphStore>,
    case: &Case,
    iters: usize,
    warmup: usize,
) -> bool {
    // Times two compile-pipeline calls + one runtime call per iter.
    // The runtime is only invoked on the unchecked path: on the
    // checked path the runtime is either skipped by the
    // guaranteed-empty short-circuit (rules.md §10 Theorem 6.5) when
    // the typechecker proves the result is empty, or would execute
    // identical work to rt_unchk on a valid query — measuring it
    // twice would just add cache-warming variance without signal.
    // To derive tc cost on `ok` rows from the CSV:
    // `compile_chk - compile_unchk` ≈ tc (parse + elab + opt cancel).
    let mut compile_chk_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut compile_unchk_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut rt_unchk_samples: Vec<u128> = Vec::with_capacity(iters);
    // Outcome of the LAST iter's checked compile. The compile is
    // deterministic so this equals every iter's outcome; we just
    // need one final value for the summary line + soundness check.
    let mut outcome = Outcome::Ok;
    // parse_failed is tracked separately from outcome because it
    // also gates the table's display formatting (— vs numeric) for
    // the no-real-measurement columns, and the iter loop's CSV emit
    // for rt_unchk.
    let mut parse_failed = false;
    let mut soundness_violations = 0usize;
    let mut max_violation_rows = 0usize;

    for n in 0..(iters + warmup) {
        let is_warmup = n < warmup;

        // ---- Checked compile (no runtime call) ----
        let t = Instant::now();
        let chk_compile = gqlrust::compile_query_with_diagnostics_with(active, case.query);
        let compile_chk_ns = t.elapsed().as_nanos();

        parse_failed = false;
        outcome = match &chk_compile {
            Ok(c) if c.guaranteed_empty => Outcome::Empty,
            Ok(_) => Outcome::Ok,
            Err(gqlrust::CompileError::Parse(_)) => {
                parse_failed = true;
                Outcome::Rejected
            }
            Err(gqlrust::CompileError::Type(_)) => Outcome::Rejected,
        };
        if !is_warmup {
            compile_chk_samples.push(compile_chk_ns);
            csv_row(db_path, case, "compile_chk", n - warmup, compile_chk_ns, "");
        }

        // ---- Unchecked compile + runtime ----
        let t = Instant::now();
        let unchk_compile = gqlrust::compile_query_unchecked(case.query);
        let compile_unchk_ns = t.elapsed().as_nanos();
        if !is_warmup {
            compile_unchk_samples.push(compile_unchk_ns);
            csv_row(
                db_path,
                case,
                "compile_unchk",
                n - warmup,
                compile_unchk_ns,
                "",
            );
        }

        // Runtime on the unchecked path. Skipped only if parse failed
        // (no Query). row_count() drives the empty-but-nonempty
        // soundness check.
        let (rt_unchk_ns, flag, rows) = match unchk_compile {
            Ok(q) => {
                let t = Instant::now();
                let result = rt.run_query(&q, LIMIT);
                let ns = t.elapsed().as_nanos();
                let r = result.row_count();
                (ns, format!("rows={r}"), r)
            }
            Err(_) => (0, "skipped".to_string(), 0),
        };
        if !is_warmup {
            rt_unchk_samples.push(rt_unchk_ns);
            if outcome == Outcome::Empty && rows != 0 {
                soundness_violations += 1;
                max_violation_rows = max_violation_rows.max(rows);
            }
            csv_row(db_path, case, "rt_unchk", n - warmup, rt_unchk_ns, &flag);
        }
    }

    let compile_chk_med = median(&compile_chk_samples);
    let compile_unchk_med = median(&compile_unchk_samples);
    let rt_unchk_med = median(&rt_unchk_samples);

    let expected = case.category.expected_outcome();

    let us = |med: u128| -> String {
        if parse_failed {
            "—".to_string()
        } else {
            format!("{:.2}", med as f64 / 1_000.0)
        }
    };
    let ms = |med: u128| -> String {
        if parse_failed {
            "—".to_string()
        } else {
            format!("{:.2}", med as f64 / 1_000_000.0)
        }
    };

    // tc_impact column. Empty/Rejected: ratio of total wall time
    // without typechecker to total wall time with typechecker,
    // `(compile_unchk + rt_unchk) / compile_chk` — the user pays
    // only compile_chk on the checked path because runtime is
    // skipped (guaranteed-empty short-circuit fires, or compile
    // rejected before reaching the runtime). Ok: typecheck overhead
    // as fraction of total wall time without typechecker,
    // `(compile_chk - compile_unchk) / (compile_unchk + rt_unchk)`.
    // Parse failure: dash.
    let impact_str = if parse_failed {
        "—".to_string()
    } else {
        match outcome {
            Outcome::Empty | Outcome::Rejected => {
                let unchk_total = compile_unchk_med + rt_unchk_med;
                if compile_chk_med > 0 && unchk_total > 0 {
                    let speedup = unchk_total as f64 / compile_chk_med as f64;
                    format!("{speedup:.1}x")
                } else {
                    "—".to_string()
                }
            }
            Outcome::Ok => {
                let tc_cost = compile_chk_med as i128 - compile_unchk_med as i128;
                let unchk_total = compile_unchk_med as i128 + rt_unchk_med as i128;
                if unchk_total > 0 {
                    let pct = tc_cost as f64 * 100.0 / unchk_total as f64;
                    format!("{pct:+.2}%")
                } else {
                    "—".to_string()
                }
            }
        }
    };

    eprintln!(
        "{:<10} {:<28} {:>13} {:>15} {:>11} {:>9} {:>11}",
        case.category.label(),
        case.id,
        us(compile_chk_med),
        us(compile_unchk_med),
        ms(rt_unchk_med),
        outcome.label(),
        impact_str,
    );

    // Two flavors of soundness anomaly under one warning umbrella:
    // outcome mismatch (case author's expected category vs actual)
    // and empty-but-nonempty (claimed empty but unchecked runtime
    // returned rows).
    let outcome_mismatch = outcome != expected;
    let empty_unsound = soundness_violations > 0;

    if outcome_mismatch {
        eprintln!(
            "  ⚠  SOUNDNESS: case {} outcome={} but expected {}. Likely a \
             typechecker regression, parser change, or schema shift.",
            case.id,
            outcome.label(),
            expected.label(),
        );
    }
    if empty_unsound {
        eprintln!(
            "  ⚠  SOUNDNESS: case {} outcome=empty but rt_unchk returned \
             up to {} rows ({} of {} iters violated). Typechecker is \
             unsound on this case.",
            case.id, max_violation_rows, soundness_violations, iters,
        );
    }

    outcome_mismatch || empty_unsound
}

fn csv_row(db: &str, case: &Case, phase: &str, iter: usize, ns: u128, flag: &str) {
    println!(
        "{};{};{};{};{};{};{}",
        db,
        case.category.label(),
        case.id,
        phase,
        iter,
        ns,
        flag,
    );
}

fn median(samples: &[u128]) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2
    }
}
