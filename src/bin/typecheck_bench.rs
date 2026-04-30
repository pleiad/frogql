//! Typechecker benchmark.
//!
//! The headline claim: when a query is doomed (statically empty, or
//! has a free variable, or has a type mismatch in a filter) the
//! typechecker rejects it in microseconds, while the runtime — given
//! the same query without the typechecker — burns milliseconds to
//! seconds enumerating doomed work before discovering the same fact.
//! This bench measures both numbers per case and prints the speedup.
//!
//! Setup is two commands. The first downloads + builds the LDBC SNB
//! SF0.1 dataset (~17 MiB CSV → `bench/data/ldbc-sf0.1.gdb`); the
//! second is this binary, which auto-builds an `ldbc-tiny.gdb`
//! fixture (head -50 of every CSV, ~840× smaller) on first run if
//! it isn't already there:
//!
//!     ./target/release/bench_setup
//!     ./target/release/typecheck_bench
//!
//! No flags needed for normal use. `--iters N` / `--warmup N`
//! available for someone tuning iteration count.
//!
//! No `.gdb` paths accepted: this bench is paired with the LDBC
//! dataset by design. If you want schema-flexible micro-bench
//! numbers on the typechecker in isolation, that's a different
//! tool — without a runtime to compare against, those numbers
//! can't make the claim this bench exists to make.
//!
//! See `bench/TYPECHECKER_BENCHMARK.md` for the discussion + reading
//! the result tables.

use std::env;
use std::fs;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use gqlrust::elaborate;
use gqlrust::optimizer;
use gqlrust::parser;
use gqlrust::runtime::engine::Runtime;
use gqlrust::store::lazy::LazyGraphStore;
use gqlrust::syntax::query::{MatchStatement, Query};
use gqlrust::typing::checker::Typechecker;

// ---------------------------------------------------------------------------
// Hardcoded dataset paths. We own the bench and we own the dataset; the
// flexibility to point this at arbitrary `.gdb` was fake. See module doc.

const SF01_GDB: &str = "bench/data/ldbc-sf0.1.gdb";
const TINY_GDB: &str = "bench/data/ldbc-tiny.gdb";
const SF01_CSV_DIR: &str =
    "bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter";
const TINY_CSV_DIR: &str =
    "bench/data/ldbc-tiny/social_network-sf0.1-CsvBasic-LongDateFormatter";

// Defaults. `LIMIT` caps the runtime row output; doesn't affect
// typecheck. Set high enough that the success cases don't artificially
// terminate early but low enough that empty cases don't pay catastrophic
// memory if the runtime allocates result buffers up front.
const DEFAULT_ITERS: usize = 30;
const DEFAULT_WARMUP: usize = 1;
const LIMIT: usize = 100;

// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    /// Control: typecheck-passes, runtime-runs. Used to show the
    /// typecheck overhead is negligible on success cases.
    Valid,
    /// Typechecker says guaranteed-empty under the active schema.
    /// On the unchecked path the runtime still runs and confirms
    /// zero rows after burning time.
    EmptyByTyping,
    /// Typechecker rejects (free var, type mismatch caught as error).
    /// On the unchecked path the runtime runs to completion and
    /// produces wrong-but-not-empty results (e.g. NULL-joined rows).
    InvalidUnbound,
    /// Parse error — caught before any later phase.
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

/// Cases are weighted toward "doomed query, expensive would-be
/// runtime" — that's the case the typechecker most clearly justifies
/// itself on. Three valid controls suffice to confirm typecheck is
/// negligible overhead on success; the other ~14 cases are doomed
/// queries shaped like real LDBC patterns where the runtime, without
/// the typechecker, would do substantial work to confirm what the
/// typechecker rejects in microseconds.
const CASES: &[Case] = &[
    // -------------------------------------------------------------------
    // Valid controls (3): demonstrate typecheck overhead vs successful runtime.
    // -------------------------------------------------------------------
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
    // -------------------------------------------------------------------
    // Empty-by-typing on expensive realistic shapes (9). Each is a
    // multi-hop or otherwise non-trivial pattern with exactly one
    // element that makes the static analysis bottom out. The runtime
    // (without typecheck) doesn't necessarily detect the same fact
    // until it has done the enumeration.
    // -------------------------------------------------------------------

    // 4-hop chain ending in an unknown label. Runtime walks
    // friend-of-friend^3 before failing to find Wagumi.
    Case {
        category: Category::EmptyByTyping,
        id: "e_chain4_bad_leaf",
        query: "MATCH (a: Person)~[:knows]~(b: Person)\
                ~[:knows]~(c: Person)~[:knows]~(d: Wagumi) \
                RETURN d.id",
    },
    // Unknown label in the middle of a chain.
    Case {
        category: Category::EmptyByTyping,
        id: "e_chain_mid_bad",
        query: "MATCH (a: Person)~[:knows]~(b: Wagumi)~[:knows]~(c: Person) \
                RETURN c.firstName",
    },
    // Unknown edge label deep in a chain.
    Case {
        category: Category::EmptyByTyping,
        id: "e_bad_edge_deep",
        query: "MATCH (a: Person)~[:knows]~(b: Person)\
                -[:noSuchEdge]->(c: Person) \
                RETURN c.firstName",
    },
    // LDBC IC2 shape (friend-of-friend → message), but with the
    // message label set to garbage. Runtime enumerates the friend
    // graph + tries to follow hasCreator before rejecting on label.
    Case {
        category: Category::EmptyByTyping,
        id: "e_ic2_bad_msg",
        query: "MATCH (p: Person)~[:knows]~(friend: Person)\
                <-[:hasCreator]-(m: Wagumi) \
                RETURN m.id",
    },
    // Conflicting labels on the same variable, deep in a chain.
    // Person ∩ Comment = ⊥ at the LabelType level. Distinct from
    // unknown-label emptiness.
    Case {
        category: Category::EmptyByTyping,
        id: "e_conflict_label_deep",
        query: "MATCH (a: Person)~[:knows]~(b: Person)\
                -[:hasCreator]->(b: Comment) \
                RETURN b.id",
    },
    // Type-mismatch in WHERE on a 3-hop pattern. firstName is str in
    // LDBC; comparing to int makes the filter type ⊥.
    Case {
        category: Category::EmptyByTyping,
        id: "e_type_mismatch_chain",
        query: "MATCH (a: Person)~[:knows]~(b: Person)~[:knows]~(c: Person) \
                WHERE c.firstName = 933 \
                RETURN c.id",
    },
    // Path-pattern union where every arm is empty by typing.
    Case {
        category: Category::EmptyByTyping,
        id: "e_union_all_bad",
        query: "MATCH (x: Wagumi)<-[:hasCreator]-(c: Comment) | \
                (x: Wagumi)<-[:hasCreator]-(c: Post) \
                RETURN c.id",
    },
    // Bounded repetition ending in unknown label. Bound `{1,2}`
    // because `{1,3}` OOMs the unchecked runtime on SF0.1 today.
    Case {
        category: Category::EmptyByTyping,
        id: "e_repeat_bad_leaf",
        query: "MATCH (p: Person)~[:knows]~{1,2}(f: Wagumi) RETURN f.id",
    },
    // Single label not in schema — the simplest empty-by-typing
    // case, included as a small-pattern control inside the empty
    // bucket so we can see how the speedup varies with pattern size.
    Case {
        category: Category::EmptyByTyping,
        id: "e_label_only",
        query: "MATCH (x: Wagumi) RETURN x.id",
    },
    // -------------------------------------------------------------------
    // Invalid (unbound var) on expensive shapes (5).
    // The unchecked runtime runs the whole pattern, then projects a
    // free variable as NULL — wrong-but-not-empty. Typechecker
    // rejects in microseconds.
    // -------------------------------------------------------------------

    // Free var in RETURN after a 4-hop chain. Runtime does the
    // 4-hop join, projects q.id as NULL.
    Case {
        category: Category::InvalidUnbound,
        id: "i_unbound_after_chain4",
        query: "MATCH (a: Person)~[:knows]~(b: Person)\
                ~[:knows]~(c: Person)~[:knows]~(d: Person) \
                RETURN q.id",
    },
    // Free var in WHERE on a multi-hop pattern. Different code path
    // (filter expression typecheck vs RETURN-clause typecheck).
    Case {
        category: Category::InvalidUnbound,
        id: "i_unbound_in_where_chain",
        query: "MATCH (a: Person)~[:knows]~(b: Person)~[:knows]~(c: Person) \
                WHERE q.id = 1 \
                RETURN c.firstName",
    },
    // Free var in a union arm — the same wrong-NULL bug but
    // reachable from inside an alternation.
    Case {
        category: Category::InvalidUnbound,
        id: "i_unbound_in_union",
        query: "MATCH (a: Person)<-[:hasCreator]-(c: Comment) | \
                (a: Person)<-[:hasCreator]-(c: Post) \
                RETURN q.id",
    },
    // Free var alongside a valid filter via AND. Tests that the
    // free-var detection survives compound predicates.
    Case {
        category: Category::InvalidUnbound,
        id: "i_unbound_compound_where",
        query: "MATCH (p: Person) WHERE p.id = 933 AND q.id = 1 \
                RETURN p.firstName",
    },
    // Short reproduction of the original wrong-NULL bug — kept for
    // the per-pattern-size comparison. Cheap baseline within the
    // invalid_unbound bucket.
    Case {
        category: Category::InvalidUnbound,
        id: "i_unbound_simple",
        query: "MATCH (p) RETURN q.name",
    },
    // -------------------------------------------------------------------
    // Parse error (1) — caught before any later phase.
    // -------------------------------------------------------------------
    Case {
        category: Category::InvalidParse,
        id: "i_parse",
        query: "MATCH (p RETURN p.name",
    },
];

// ---------------------------------------------------------------------------
// Setup: dataset detection and tiny-fixture auto-build.

fn ensure_datasets() -> Vec<PathBuf> {
    let mut dbs: Vec<PathBuf> = Vec::new();

    if !Path::new(SF01_GDB).exists() {
        eprintln!(
            "Required dataset missing: {SF01_GDB}\n\
             Run `./target/release/bench_setup` first to download and build it."
        );
        std::process::exit(1);
    }

    if !Path::new(TINY_GDB).exists() {
        eprintln!("=== One-time setup: building {TINY_GDB} ===");
        if !Path::new(SF01_CSV_DIR).exists() {
            eprintln!(
                "  ! SF0.1 source CSVs not found at {SF01_CSV_DIR}.\n\
                 ! `bench_setup` must have extracted them; re-run it if needed."
            );
            std::process::exit(1);
        }
        build_tiny_fixture().unwrap_or_else(|e| {
            eprintln!("  ! tiny-fixture build failed: {e}");
            std::process::exit(1);
        });
    }

    // Order matters: tiny first (fast feedback), SF0.1 second.
    dbs.push(PathBuf::from(TINY_GDB));
    dbs.push(PathBuf::from(SF01_GDB));
    dbs
}

/// Auto-truncate every CSV in SF0.1's tree to `head -50` (header +
/// 49 data rows) and import the result via the gqlite binary. One-
/// time operation — caches as `ldbc-tiny.gdb` on disk. Same loader
/// path as SF0.1, so the resulting schema is a strict subset.
fn build_tiny_fixture() -> io::Result<()> {
    let head_n = 50usize;

    fs::create_dir_all(format!("{TINY_CSV_DIR}/dynamic"))?;
    fs::create_dir_all(format!("{TINY_CSV_DIR}/static"))?;

    // Loader auxiliaries (e.g. spanner_import_config.json) live at
    // the dataset root; copy them verbatim. The loader needs them.
    for entry in fs::read_dir(SF01_CSV_DIR)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) != Some("csv") {
            let dst = PathBuf::from(TINY_CSV_DIR).join(path.file_name().unwrap());
            fs::copy(&path, dst)?;
        }
    }

    let mut total_src_lines = 0usize;
    let mut total_dst_lines = 0usize;
    for sub in &["dynamic", "static"] {
        let src_dir = format!("{SF01_CSV_DIR}/{sub}");
        let dst_dir = format!("{TINY_CSV_DIR}/{sub}");
        for entry in fs::read_dir(&src_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("csv") {
                continue;
            }
            let name = path.file_name().unwrap();
            let dst = PathBuf::from(&dst_dir).join(name);

            let src_file = fs::File::open(&path)?;
            let reader = io::BufReader::new(src_file);
            let dst_file = fs::File::create(&dst)?;
            let mut writer = BufWriter::new(dst_file);
            let mut line_count = 0usize;
            let mut full_count = 0usize;
            for line in reader.lines() {
                let line = line?;
                full_count += 1;
                if line_count < head_n {
                    writeln!(writer, "{line}")?;
                    line_count += 1;
                }
            }
            total_src_lines += full_count;
            total_dst_lines += line_count;
        }
    }

    eprintln!(
        "  truncated CSVs: {} → {} lines total (~{:.0}× smaller)",
        total_src_lines,
        total_dst_lines,
        total_src_lines as f64 / total_dst_lines.max(1) as f64,
    );

    // Build the .gdb via the gqlite binary. We could call into the
    // import API directly but shelling out keeps the loader's behavior
    // identical to a normal user import.
    eprintln!("  importing into {TINY_GDB}...");
    let status = Command::new("./target/release/gqlite")
        .args([
            TINY_GDB,
            "--import-ldbc-csv",
            TINY_CSV_DIR,
            "--no-typecheck",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "gqlite import returned non-zero status: {status}",
        )));
    }
    eprintln!("  ✓ {TINY_GDB} built");
    Ok(())
}

// ---------------------------------------------------------------------------
// Main + per-DB driver.

fn print_usage(prog: &str) {
    eprintln!(
        "Usage: {prog} [--iters N] [--warmup N]\n\
         \n\
         Defaults: --iters {DEFAULT_ITERS}  --warmup {DEFAULT_WARMUP}\n\
         \n\
         Datasets are auto-discovered at bench/data/ldbc-sf0.1.gdb\n\
         and bench/data/ldbc-tiny.gdb (auto-built on first run).\n\
         Run ./target/release/bench_setup first if SF0.1 is missing."
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

    let dbs = ensure_datasets();

    println!("db;category;case;phase;iter;ns;flags");

    for db in &dbs {
        bench_db(db, iters, warmup);
    }
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

    eprintln!(
        "{:<16} {:<28} {:>9} {:>9} {:>9} {:>9} {:>10} {:>11} {:>9} {:>10}",
        "cat", "case", "parse_us", "elab_us", "tc_us", "opt_us",
        "rt_chk_ms", "rt_unchk_ms", "verdict", "speedup",
    );
    eprintln!("{}", "-".repeat(132));

    for case in CASES {
        run_case(&path_str, &active, &rt, case, iters, warmup);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_case<'g>(
    db_path: &str,
    active: &gqlrust::typing::variable_type::Schema,
    rt: &Runtime<'g, LazyGraphStore>,
    case: &Case,
    iters: usize,
    warmup: usize,
) {
    let mut parse_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut elab_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut tc_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut opt_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut rt_chk_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut rt_unchk_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut empty_flag = false;
    let mut typecheck_rejected = false;
    let mut parse_failed = false;
    let mut soundness_violations = 0usize;
    let mut max_violation_rows = 0usize;

    for n in 0..(iters + warmup) {
        let is_warmup = n < warmup;

        // Parse.
        let t = Instant::now();
        let parsed = parser::parse_query(case.query);
        let parse_ns = t.elapsed().as_nanos();
        if !is_warmup {
            parse_samples.push(parse_ns);
            csv_row(db_path, case, "parse", n - warmup, parse_ns, "");
        }

        let ast = match parsed {
            Ok(a) => a,
            Err(_) => {
                parse_failed = true;
                if !is_warmup {
                    elab_samples.push(0);
                    tc_samples.push(0);
                    opt_samples.push(0);
                    rt_chk_samples.push(0);
                    rt_unchk_samples.push(0);
                    for phase in ["elab", "tc", "opt", "rt_chk", "rt_unchk"] {
                        csv_row(db_path, case, phase, n - warmup, 0, "skipped");
                    }
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
            csv_row(db_path, case, "elab", n - warmup, elab_ns, "");
        }

        // Typecheck. Schema clone is INSIDE the timed region:
        // production (`compile_query_with_diagnostics_with`) does the
        // same per-query clone, so the bench's per-query cost matches
        // what real callers pay.
        let t = Instant::now();
        let mut tc = Typechecker::new(active.clone());
        let r = tc.check_query(&elab);
        let tc_ns = t.elapsed().as_nanos();
        if !is_warmup {
            tc_samples.push(tc_ns);
            csv_row(db_path, case, "tc", n - warmup, tc_ns, "");
        }
        empty_flag = r.empty;
        typecheck_rejected = !r.ok;

        // Optimize. Runs unconditionally (both checked and unchecked
        // paths optimize before running).
        let t = Instant::now();
        let optimized_pattern = optimizer::compile(elab.collapsed_pattern());
        let opt_ns = t.elapsed().as_nanos();
        if !is_warmup {
            opt_samples.push(opt_ns);
            csv_row(db_path, case, "opt", n - warmup, opt_ns, "");
        }

        let optimized = Query {
            matches: vec![MatchStatement::Simple {
                pattern: optimized_pattern,
            }],
            ..elab
        };

        // Checked runtime: §10 short-circuit honored.
        let rt_chk_ns = if r.empty || typecheck_rejected {
            0
        } else {
            let t = Instant::now();
            let _ = rt.run_query(&optimized, LIMIT);
            t.elapsed().as_nanos()
        };
        if !is_warmup {
            rt_chk_samples.push(rt_chk_ns);
            let flag = if r.empty || typecheck_rejected { "skipped" } else { "" };
            csv_row(db_path, case, "rt_chk", n - warmup, rt_chk_ns, flag);
        }

        // Unchecked runtime: the comparison baseline. Always runs.
        // We cross-check row_count() on iter to catch the case where
        // the typechecker says empty but the runtime would have
        // returned rows (a soundness bug we'd otherwise quietly
        // count as a great speedup).
        let t = Instant::now();
        let result = rt.run_query(&optimized, LIMIT);
        let rt_unchk_ns = t.elapsed().as_nanos();
        let rows = result.row_count();
        if !is_warmup {
            rt_unchk_samples.push(rt_unchk_ns);
            if r.empty && rows != 0 {
                soundness_violations += 1;
                max_violation_rows = max_violation_rows.max(rows);
            }
            let flag = format!("rows={rows}");
            csv_row(db_path, case, "rt_unchk", n - warmup, rt_unchk_ns, &flag);
        }
    }

    let parse_med = median(&parse_samples);
    let elab_med = median(&elab_samples);
    let tc_med = median(&tc_samples);
    let opt_med = median(&opt_samples);
    let rt_chk_med = median(&rt_chk_samples);
    let rt_unchk_med = median(&rt_unchk_samples);

    let total_compile_ns = parse_med + elab_med + tc_med + opt_med;
    let speedup = if total_compile_ns > 0 && rt_unchk_med > 0 {
        rt_unchk_med as f64 / total_compile_ns as f64
    } else {
        0.0
    };

    let verdict = if parse_failed {
        "parse"
    } else {
        match (empty_flag, typecheck_rejected) {
            (false, false) => "ok",
            (true, false) => "empty",
            (false, true) => "rejected",
            (true, true) => "both",
        }
    };

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
    let speedup_str = if speedup > 0.0 {
        format!("{speedup:>8.1}x")
    } else {
        "—".to_string()
    };

    eprintln!(
        "{:<16} {:<28} {:>9.2} {:>9} {:>9} {:>9} {:>10} {:>11} {:>9} {:>10}",
        case.category.label(),
        case.id,
        parse_med as f64 / 1_000.0,
        us(elab_med),
        us(tc_med),
        us(opt_med),
        ms(rt_chk_med),
        ms(rt_unchk_med),
        verdict,
        speedup_str,
    );

    if soundness_violations > 0 {
        eprintln!(
            "  ⚠  SOUNDNESS: case {} verdict=empty but rt_unchk returned \
             up to {} rows ({} of {} iters violated). Investigate.",
            case.id, max_violation_rows, soundness_violations, iters,
        );
    }
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
