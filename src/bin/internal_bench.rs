//! Internal bench — gqlite-only diagnostics.
//!
//! Compares wall time of the LDBC SF0.1 case set with vs without the
//! typechecker, across lazy and disk backends. The typechecker can
//! short-circuit doomed queries (empty-by-typing, free variable, type
//! mismatch) before the runtime ever runs — see rules.md §10
//! Theorem 6.5 for the soundness argument behind the guaranteed-empty
//! short-circuit. This bench measures both paths per case and reports
//! the difference.
//!
//! Usage:
//!
//!     ./target/release/bench_setup       # one-time: download + build .gdb
//!     ./target/release/internal_bench    # run the bench
//!
//! Flags: `--iters N` (default 3), `--warmup N` (default 1), and
//! `--sf 0.1|0.3` (default 0.1). No dataset path arg — opens
//! `bench/data/ldbc-sf<SF>.gdb` from a hardcoded path; case set
//! references LDBC labels by name.
//!
//! See `bench/INTERNAL_BENCHMARK.md` for output format, case set
//! details, and stream cross-checking.

use std::env;
use std::path::Path;
use std::time::Instant;

use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};

use gqlrust::model::graph_access::GraphAccess;
use gqlrust::runtime::engine::Runtime;
use gqlrust::store::disk::DiskGraphStore;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::typing::variable_type::Schema;

// ---------------------------------------------------------------------------

const SF01_GDB: &str = "bench/data/ldbc-sf0.1.gdb";
const SF03_GDB: &str = "bench/data/ldbc-sf0.3.gdb";

// 5 gives a real median and a usable p95 from a sorted-sample
// estimator. The slowest doomed cases (`e_chain4_bad_leaf`,
// `e_repeat_bad_leaf`) take ~40s/iter unchecked, so 5 iters lands
// the default invocation around ~40 min on SF0.1; iters=10 doubles
// that. Pass --iters 3 for a quicker dev-iteration loop.
const DEFAULT_ITERS: usize = 5;
const DEFAULT_WARMUP: usize = 1;
// Result-row cap per runtime call.
const LIMIT: usize = 100;

// CSV stdout: backend;db;category;case;phase;iter;ns;flags
// See bench/INTERNAL_BENCHMARK.md "Output" for the column and
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

/// 25 cases: 3 valid controls, 3 LDBC-IC valid, 3 valid extras
/// (empty-by-data, aggregate, variable-length), 10 empty-by-typing,
/// 6 invalid (rejected). See `bench/INTERNAL_BENCHMARK.md` for the
/// case-set description.
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
    // ---- LDBC IC valid cases (the centerpieces — spec-faithful + ORDER BY teardown) ----
    // v_ic2_spec: full LDBC IC2 (anonymous start, COALESCE, ORDER BY).
    // The slowdown vs v_ic2_noorder is what ORDER BY costs us.
    Case {
        category: Category::Valid,
        id: "v_ic2_spec",
        query: "MATCH (:Person {id: 19791209300143})~[:knows]~(friend: Person)<-[:hasCreator]-(message: Comment | Post) \
                WHERE message.creationDate <= 1354060800000 \
                RETURN friend.id AS personId, friend.firstName AS personFirstName, \
                       friend.lastName AS personLastName, message.id AS postOrCommentId, \
                       COALESCE(message.content, message.imageFile) AS postOrCommentContent, \
                       message.creationDate AS postOrCommentCreationDate \
                ORDER BY postOrCommentCreationDate DESC, postOrCommentId ASC LIMIT 20",
    },
    // v_ic2_noorder: same logical query as v_ic2_spec WITHOUT ORDER BY.
    // LIMIT can short-circuit at the first 20 matched messages instead
    // of materializing + sorting the full result set. The ratio
    // v_ic2_spec / v_ic2_noorder isolates the runtime cost of the
    // spec's ORDER BY tie-break (no top-N heap optimization yet).
    Case {
        category: Category::Valid,
        id: "v_ic2_noorder",
        query: "MATCH (:Person {id: 19791209300143})~[:knows]~(friend: Person)<-[:hasCreator]-(message: Comment | Post) \
                WHERE message.creationDate <= 1354060800000 \
                RETURN friend.id AS personId, friend.firstName AS personFirstName, \
                       friend.lastName AS personLastName, message.id AS postOrCommentId, \
                       COALESCE(message.content, message.imageFile) AS postOrCommentContent, \
                       message.creationDate AS postOrCommentCreationDate \
                LIMIT 20",
    },
    // v_ic8_spec: LDBC IC8 spec-faithful — multi-hop chain through replyOf.
    // Different shape than IC2 (longer chain, single label-disjunction
    // hop in the middle).
    Case {
        category: Category::Valid,
        id: "v_ic8_spec",
        query: "MATCH (start: Person {id: 2199023256816})<-[:hasCreator]-(:Comment | Post)<-[:replyOf]-(comment: Comment)-[:hasCreator]->(person: Person) \
                RETURN person.id AS personId, person.firstName AS personFirstName, \
                       person.lastName AS personLastName, comment.creationDate AS commentCreationDate, \
                       comment.id AS commentId, comment.content AS commentContent \
                ORDER BY commentCreationDate DESC, commentId ASC LIMIT 20",
    },
    // v_empty_by_data: type-checks fine; the runtime lookup misses
    // because the id literally isn't in the data. Pairs vs every
    // `e_*` case to show the typechecker's *limit* — it can short-
    // circuit type-driven emptiness, but data-driven emptiness has
    // to go through the runtime.
    Case {
        category: Category::Valid,
        id: "v_empty_by_data",
        query: "MATCH (p: Person {id: 1234567890}) RETURN p.firstName",
    },
    // v_count_friends: aggregate. Different runtime cost profile from
    // straight projection — forces full materialization, no LIMIT
    // short-circuit. Bare-node aggregate (`COUNT(f)`) doesn't parse
    // in gqlite today; we count `f.id` instead.
    Case {
        category: Category::Valid,
        id: "v_count_friends",
        query: "MATCH (p: Person {id: 933})~[:knows]~(f: Person) RETURN COUNT(f.id)",
    },
    // v_repeat: variable-length match (friends-of-friends, 1 or 2
    // hops). Exercises gqlite's repetition runtime which IC2/IC8
    // chains don't cover.
    Case {
        category: Category::Valid,
        id: "v_repeat",
        query: "MATCH (p: Person {id: 933})~[:knows]~{1,2}(f: Person) RETURN f.firstName",
    },
    // ---- Empty by typing (10) ----
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
    // e_type_clash_arith: arithmetic between mismatched types
    // (`p.firstName + p.id` is `string + int`). The schema knows
    // both types; the typechecker should mark this guaranteed-empty
    // (no value satisfies the predicate after the type clash).
    // Different surface than e_type_mismatch_chain which uses
    // equality (`c.firstName = 933`) — this one is in arithmetic
    // followed by a comparison.
    Case {
        category: Category::EmptyByTyping,
        id: "e_type_clash_arith",
        query: "MATCH (p: Person) WHERE p.firstName + p.id > 0 RETURN p.id",
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
        "Usage: {prog} [--iters N] [--warmup N] [--sf 0.1|0.3]\n\
         \n\
         Defaults: --iters {DEFAULT_ITERS}  --warmup {DEFAULT_WARMUP}  --sf 0.1\n\
         \n\
         Datasets: bench/data/ldbc-sf{{0.1,0.3}}.gdb. SF0.1 is auto-built\n\
         by bench_setup; SF0.3 must be built separately."
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut iters = DEFAULT_ITERS;
    let mut warmup = DEFAULT_WARMUP;
    let mut sf = "0.1".to_string();
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
            "--sf" => {
                sf = need_value(i, "--sf").to_string();
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

    let gdb_path: &str = match sf.as_str() {
        "0.1" => SF01_GDB,
        "0.3" => SF03_GDB,
        other => {
            eprintln!("--sf must be 0.1 or 0.3, got {other:?}");
            std::process::exit(1);
        }
    };

    if sf == "0.1" {
        ensure_dataset();
    } else if !Path::new(gdb_path).exists() {
        eprintln!("missing {gdb_path} for SF{sf}; build it first via bench_setup or import.");
        std::process::exit(1);
    }

    println!("backend;db;category;case;phase;iter;ns;flags");
    let db_path = Path::new(gdb_path);
    let path_str = db_path.display().to_string();

    // RSS sampler. Snapshot baseline now (pre-open) so deltas
    // reflect engine + DB state, not Rust runtime + sysinfo overhead.
    let mut sys = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::new().with_memory()),
    );
    let rss_baseline = rss_mb(&mut sys);
    let db_bytes = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    let db_size_mib = db_bytes as f64 / (1024.0 * 1024.0);
    eprintln!(
        "Setup: db_path = {}\n       db_size = {:.1} MiB ({} bytes)\n       rss_baseline = {:.1} MiB",
        path_str, db_size_mib, db_bytes, rss_baseline,
    );

    // ---- lazy backend ----
    eprintln!("\n=== lazy: {path_str} ===");
    let t0 = Instant::now();
    let lazy = LazyGraphStore::open(db_path).unwrap_or_else(|e| {
        eprintln!("failed to open .gdb {path_str}: {e}");
        std::process::exit(1);
    });
    let lazy_open_secs = t0.elapsed().as_secs_f64();
    let rss_after_lazy_open = rss_mb(&mut sys);
    eprintln!(
        "  open_time     = {:.2}s\n  graph         = {} nodes / {} edges\n  rss_after_open = {:.1} MiB (+{:.1} over baseline)",
        lazy_open_secs,
        lazy.node_count(),
        lazy.edge_count(),
        rss_after_lazy_open,
        rss_after_lazy_open - rss_baseline,
    );
    let active = lazy.catalog().active_schema().clone();
    let rt_lazy = Runtime::new(&lazy);
    let mut peak_rss_lazy = rss_after_lazy_open;
    bench_db(
        "lazy",
        &path_str,
        &active,
        &rt_lazy,
        iters,
        warmup,
        &mut sys,
        &mut peak_rss_lazy,
    );
    eprintln!(
        "  peak_rss_loop = {:.1} MiB (+{:.1} over baseline)",
        peak_rss_lazy,
        peak_rss_lazy - rss_baseline,
    );

    // ---- disk backend ----
    drop(rt_lazy);
    drop(lazy);
    eprintln!("\n=== disk: {path_str} ===");
    let t0 = Instant::now();
    let disk = DiskGraphStore::open(db_path).unwrap_or_else(|e| {
        eprintln!("failed to open .gdb {path_str} (disk backend): {e}");
        std::process::exit(1);
    });
    let disk_open_secs = t0.elapsed().as_secs_f64();
    let rss_after_disk_open = rss_mb(&mut sys);
    eprintln!(
        "  open_time     = {:.2}s\n  rss_after_open = {:.1} MiB (+{:.1} over baseline)",
        disk_open_secs,
        rss_after_disk_open,
        rss_after_disk_open - rss_baseline,
    );
    let rt_disk = Runtime::new(&disk);
    let mut peak_rss_disk = rss_after_disk_open;
    bench_db(
        "disk",
        &path_str,
        &active,
        &rt_disk,
        iters,
        warmup,
        &mut sys,
        &mut peak_rss_disk,
    );
    eprintln!(
        "  peak_rss_loop = {:.1} MiB (+{:.1} over baseline)",
        peak_rss_disk,
        peak_rss_disk - rss_baseline,
    );
}

fn rss_mb(sys: &mut System) -> f64 {
    let pid = Pid::from_u32(std::process::id());
    sys.refresh_process_specifics(pid, ProcessRefreshKind::new().with_memory());
    sys.process(pid)
        .map(|p| p.memory() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0)
}

// Threading 8 args is fine here: this fn has one call site (per-backend
// dispatch in main) and lifting them into a struct adds ceremony
// without clarity. Bench-internal only.
#[allow(clippy::too_many_arguments)]
fn bench_db<G: GraphAccess>(
    backend: &str,
    path_str: &str,
    active: &Schema,
    rt: &Runtime<'_, G>,
    iters: usize,
    warmup: usize,
    sys: &mut System,
    peak_rss: &mut f64,
) {
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
        if run_case(backend, path_str, active, rt, case, iters, warmup) {
            warned_count += 1;
        }
        // Sample RSS per case (cheap; once per ~iters runs). Tracks
        // the high-water mark for the per-backend peak emit.
        let cur = rss_mb(sys);
        if cur > *peak_rss {
            *peak_rss = cur;
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
fn run_case<G: GraphAccess>(
    backend: &str,
    db_path: &str,
    active: &Schema,
    rt: &Runtime<'_, G>,
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
            csv_row(
                backend,
                db_path,
                case,
                "compile_chk",
                n - warmup,
                compile_chk_ns,
                "",
            );
        }

        // ---- Unchecked compile + runtime ----
        let t = Instant::now();
        let unchk_compile = gqlrust::compile_query_unchecked(case.query);
        let compile_unchk_ns = t.elapsed().as_nanos();
        if !is_warmup {
            compile_unchk_samples.push(compile_unchk_ns);
            csv_row(
                backend,
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
            csv_row(
                backend,
                db_path,
                case,
                "rt_unchk",
                n - warmup,
                rt_unchk_ns,
                &flag,
            );
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

fn csv_row(backend: &str, db: &str, case: &Case, phase: &str, iter: usize, ns: u128, flag: &str) {
    println!(
        "{};{};{};{};{};{};{};{}",
        backend,
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
