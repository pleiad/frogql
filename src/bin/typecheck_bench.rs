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
//! second is this binary:
//!
//!     ./target/release/bench_setup
//!     ./target/release/typecheck_bench
//!
//! No flags needed for normal use. `--iters N` / `--warmup N`
//! available for someone tuning iteration count.
//!
//! An earlier version also auto-built an `ldbc-tiny.gdb` fixture
//! (head-50-of-every-CSV) and ran every case against both. Dropped:
//! the dataset-size axis it demonstrated (typecheck cost is schema-
//! bound, runtime cost data-bound) is theoretically obvious and the
//! truncation strategy was fragile (too aggressive a head dropped
//! sparse edge types from the inferred schema, breaking the cases
//! that referenced them). SF0.1 alone is sufficient for the headline.
//!
//! No `.gdb` paths accepted: this bench is paired with the LDBC
//! dataset by design. Schema-flexible "typechecker in isolation"
//! numbers aren't a thing we measure — without a runtime to compare
//! against, those numbers can't make the claim this bench exists to
//! make. (An earlier criterion harness tried; deleted.)
//!
//! See `bench/TYPECHECKER_BENCHMARK.md` for the discussion + reading
//! the result tables.

use std::env;
use std::path::Path;
use std::time::Instant;

use gqlrust::runtime::engine::Runtime;
use gqlrust::store::lazy::LazyGraphStore;

// ---------------------------------------------------------------------------
// Hardcoded dataset path. The bench is paired with the LDBC SF0.1
// dataset by design; accepting arbitrary `.gdb` was fake flexibility.

const SF01_GDB: &str = "bench/data/ldbc-sf0.1.gdb";

// Defaults. `LIMIT` caps how many result rows the runtime materializes
// per query — doesn't affect typecheck. 100 is enough for the valid
// controls to produce stable timings without spending bench wall time
// streaming millions of rows we don't read.
const DEFAULT_ITERS: usize = 30;
const DEFAULT_WARMUP: usize = 1;
const LIMIT: usize = 100;

// CSV stdout columns: db;category;case;phase;iter;ns;flags
//
// `phase` vocabulary — four rows per iter, two compile + two runtime:
//   - "compile_chk"  : gqlrust::compile_query_with_diagnostics_with()
//   - "rt_chk"       : runtime call on the checked-path Query
//                       (skipped if compile rejected or empty)
//   - "compile_unchk": gqlrust::compile_query_unchecked()
//   - "rt_unchk"     : runtime call on the unchecked-path Query
//                       (skipped only if parse itself failed)
//
// `flags` vocabulary:
//   - empty string  : phase ran successfully
//   - "skipped"     : phase was bypassed.
//                     - on `rt_chk`: compile rejected, or §10
//                       short-circuit fired on empty.
//                     - on `rt_unchk`: parse failure prevented
//                       the runtime from running at all.
//                     "skipped" never appears on the two compile
//                     phases — those always produce a measurement,
//                     including for parse failures (the parse-fail
//                     timing is the parse-only cost).
//   - "rows=N"      : on `rt_unchk` rows that actually ran, the
//                     result row count (N=0 means "ran and got 0
//                     rows", which is meaningful and distinct from
//                     "skipped"). Used for the empty-but-nonempty
//                     soundness check.

// ---------------------------------------------------------------------------

/// What the case author claims the typechecker should produce
/// against the LDBC schema. Drives both the per-DB summary
/// grouping (`cat` column) and the soundness check (an actual
/// outcome outside `expected_outcome()` is a regression).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    /// Control: typecheck-passes, runtime-runs. Used to show the
    /// typecheck overhead is negligible on success cases.
    Valid,
    /// Typechecker says guaranteed-empty under the active schema.
    /// On the unchecked path the runtime still runs and confirms
    /// zero rows after burning time.
    EmptyByTyping,
    /// Typechecker rejects: free var, type mismatch as error, or
    /// parse failure (we don't separate "rejected by parser" from
    /// "rejected by typechecker" at the outcome level — both are
    /// "the compile pipeline refused this query"). On the
    /// unchecked path the runtime runs to completion and produces
    /// wrong-but-not-empty results (e.g. NULL-joined rows).
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

/// What actually happened in a run. Three buckets, derived from the
/// stored typechecker state — no round-trip through display strings.
///
/// - `Ok`: !empty, !rejected, !parse_failed
/// - `Empty`: empty, but not rejected (and parse succeeded)
/// - `Rejected`: rejected by typechecker OR by parser. "rejected AND
///   empty" (sometimes called "both") collapses here because a
///   rejected query is invalid regardless of whether the residual
///   pattern is also unsatisfiable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Ok,
    Empty,
    Rejected,
}

impl Outcome {
    fn classify(empty: bool, rejected: bool, parse_failed: bool) -> Self {
        if rejected || parse_failed {
            Outcome::Rejected
        } else if empty {
            Outcome::Empty
        } else {
            Outcome::Ok
        }
    }
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

/// Cases are weighted toward "doomed query, expensive would-be
/// runtime" — that's the case the typechecker most clearly justifies
/// itself on. Three valid controls suffice to confirm typecheck is
/// negligible overhead on success; the other 15 cases are doomed
/// queries (9 empty / 6 rejected) shaped like real LDBC patterns
/// where the runtime, without the typechecker, would do substantial
/// work to confirm what the typechecker rejects in microseconds.
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
    // Empty-by-typing (9). Mostly multi-hop / realistic-shape queries
    // where one element makes the static analysis bottom out — the
    // runtime, without typecheck, has to do the enumeration to find
    // out. One small-pattern control (`e_label_only`) at the end of
    // the bucket gives a same-bucket reference for cases where the
    // runtime can rule out emptiness cheaply, so we can compare
    // small-pattern speedup against the deep-pattern speedups.
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
        category: Category::InvalidRejected,
        id: "i_unbound_after_chain4",
        query: "MATCH (a: Person)~[:knows]~(b: Person)\
                ~[:knows]~(c: Person)~[:knows]~(d: Person) \
                RETURN q.id",
    },
    // Free var in WHERE on a multi-hop pattern. Different code path
    // (filter expression typecheck vs RETURN-clause typecheck).
    Case {
        category: Category::InvalidRejected,
        id: "i_unbound_in_where_chain",
        query: "MATCH (a: Person)~[:knows]~(b: Person)~[:knows]~(c: Person) \
                WHERE q.id = 1 \
                RETURN c.firstName",
    },
    // Free var in a union arm — the same wrong-NULL bug but
    // reachable from inside an alternation.
    Case {
        category: Category::InvalidRejected,
        id: "i_unbound_in_union",
        query: "MATCH (a: Person)<-[:hasCreator]-(c: Comment) | \
                (a: Person)<-[:hasCreator]-(c: Post) \
                RETURN q.id",
    },
    // Free var alongside a valid filter via AND. Tests that the
    // free-var detection survives compound predicates.
    Case {
        category: Category::InvalidRejected,
        id: "i_unbound_compound_where",
        query: "MATCH (p: Person) WHERE p.id = 933 AND q.id = 1 \
                RETURN p.firstName",
    },
    // Short reproduction of the original wrong-NULL bug — kept for
    // the per-pattern-size comparison. Cheap baseline within the
    // invalid_unbound bucket.
    Case {
        category: Category::InvalidRejected,
        id: "i_unbound_simple",
        query: "MATCH (p) RETURN q.name",
    },
    // -------------------------------------------------------------------
    // Parse error (1) — caught before any later phase.
    // -------------------------------------------------------------------
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

    // Column widths: 10 (cat) + 28 (case) + 13 (compile_chk_us) +
    // 15 (compile_unchk_us) + 10 (rt_chk_ms) + 11 (rt_unchk_ms) +
    // 9 (outcome) + 11 (tc_impact) = 107, plus 7 separator spaces
    // = 114.
    const TABLE_WIDTH: usize = 114;
    eprintln!(
        "{:<10} {:<28} {:>13} {:>15} {:>10} {:>11} {:>9} {:>11}",
        "cat", "case",
        "compile_chk_us", "compile_unchk_us",
        "rt_chk_ms", "rt_unchk_ms",
        "outcome", "tc_impact",
    );
    // tc_impact dual format — inline legend so the table is
    // self-documenting without flipping to the doc:
    eprintln!(
        "  (compile_chk = production checked-pipeline cost; compile_unchk = same minus the typechecker)"
    );
    eprintln!(
        "  (tc_impact: Yx on empty/rejected = rt_unchk / compile_chk speedup; \
         ±X% on ok = (compile_chk+rt_chk - compile_unchk-rt_unchk) / (compile_unchk+rt_unchk); — for parse fail)"
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
    // Two compile-pipeline samples, two runtime samples. We invoke
    // production's entry points wholesale (`compile_query_with_diagnostics_with`
    // and `compile_query_unchecked`) rather than reimplementing the
    // pipeline phase-by-phase: the entry points already encode the
    // correct semantics — including "bail before optimize when the
    // typechecker rejects" — and aligning with them prevents the
    // bench from drifting from production cost as the pipeline
    // evolves.
    //
    // Per-phase breakdown (parse / elab / tc / opt as separate columns)
    // is intentionally gone. If you need to attribute compile cost to
    // a specific phase, derive it from the per-iter CSV: on `ok` rows,
    // `compile_chk - compile_unchk` ≈ tc cost (parse / elab / opt
    // cancel between the two pipelines).
    let mut compile_chk_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut compile_unchk_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut rt_chk_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut rt_unchk_samples: Vec<u128> = Vec::with_capacity(iters);
    let mut empty_flag = false;
    let mut typecheck_rejected = false;
    let mut parse_failed = false;
    let mut soundness_violations = 0usize;
    let mut max_violation_rows = 0usize;

    for n in 0..(iters + warmup) {
        let is_warmup = n < warmup;

        // ---- Checked path: compile + (maybe) runtime ----
        // Production's REPL/Python flow goes through this exact entry
        // point with the active schema; we time the whole thing.
        let t = Instant::now();
        let chk_compile = gqlrust::compile_query_with_diagnostics_with(active, case.query);
        let compile_chk_ns = t.elapsed().as_nanos();

        // Re-derive the booleans every iter from the structured
        // result; deterministic, but kept fresh so a per-iter
        // soundness check has the right state.
        empty_flag = false;
        typecheck_rejected = false;
        parse_failed = false;
        let chk_query = match chk_compile {
            Ok(c) => {
                empty_flag = c.guaranteed_empty;
                Some(c.query)
            }
            Err(gqlrust::CompileError::Parse(_)) => {
                parse_failed = true;
                None
            }
            Err(gqlrust::CompileError::Type(_)) => {
                typecheck_rejected = true;
                None
            }
        };
        if !is_warmup {
            compile_chk_samples.push(compile_chk_ns);
            csv_row(db_path, case, "compile_chk", n - warmup, compile_chk_ns, "");
        }

        // Runtime on the checked path. Skipped if the compile
        // pipeline either rejected the query (no Query to run) or
        // told the caller it's guaranteed-empty (§10 short-circuit).
        let short_circuit = chk_query.is_none() || empty_flag;
        let rt_chk_ns = if short_circuit {
            0
        } else {
            let q = chk_query.as_ref().unwrap();
            let t = Instant::now();
            let _ = rt.run_query(q, LIMIT);
            t.elapsed().as_nanos()
        };
        if !is_warmup {
            rt_chk_samples.push(rt_chk_ns);
            let flag = if short_circuit { "skipped" } else { "" };
            csv_row(db_path, case, "rt_chk", n - warmup, rt_chk_ns, flag);
        }

        // ---- Unchecked path: compile_unchecked + runtime ----
        // The "what production would do without the typechecker"
        // baseline. compile_query_unchecked runs parse + elab + opt
        // (no tc), so its time naturally excludes the typechecker.
        let t = Instant::now();
        let unchk_compile = gqlrust::compile_query_unchecked(case.query);
        let compile_unchk_ns = t.elapsed().as_nanos();
        if !is_warmup {
            compile_unchk_samples.push(compile_unchk_ns);
            csv_row(db_path, case, "compile_unchk", n - warmup, compile_unchk_ns, "");
        }

        // Runtime on the unchecked path. Always runs unless parse
        // itself failed (in which case neither path has a query).
        // We capture row_count() to cross-check empty soundness:
        // typechecker said empty + runtime returned rows = unsound.
        // CSV flag: "rows=N" when the runtime actually ran (truthful
        // count, including 0 on legitimately-empty results), or
        // "skipped" when parse failure prevented the runtime from
        // running at all (so a downstream CSV reader doesn't confuse
        // "ran and got 0 rows" with "never ran").
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
            if empty_flag && rows != 0 {
                soundness_violations += 1;
                max_violation_rows = max_violation_rows.max(rows);
            }
            csv_row(db_path, case, "rt_unchk", n - warmup, rt_unchk_ns, &flag);
        }
    }

    let compile_chk_med = median(&compile_chk_samples);
    let compile_unchk_med = median(&compile_unchk_samples);
    let rt_chk_med = median(&rt_chk_samples);
    let rt_unchk_med = median(&rt_unchk_samples);

    let outcome = Outcome::classify(empty_flag, typecheck_rejected, parse_failed);
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

    // tc_impact: split-format column.
    //
    //   - Empty / Rejected: SPEEDUP MULTIPLIER `rt_unchk / compile_chk`.
    //     The numerator is the runtime work the §10 short-circuit
    //     spared; the denominator is the cost of the production
    //     checked-path compile (which already correctly excludes
    //     optimize when the typechecker rejects, because we time the
    //     entry-point fn and it bails before opt on rejection).
    //
    //   - Ok: SIGNED PERCENTAGE of total wall time delta. Compares
    //     `compile_chk + rt_chk` (with-typechecker total) to
    //     `compile_unchk + rt_unchk` (without-typechecker total).
    //     `+X%` is overhead the typechecker added; `-X%` is only
    //     reachable from runtime variance.
    //
    //   - Parse failure: dash (no compile pipeline to compare).
    let impact_str = if parse_failed {
        "—".to_string()
    } else {
        match outcome {
            Outcome::Empty | Outcome::Rejected => {
                if compile_chk_med > 0 && rt_unchk_med > 0 {
                    let speedup = rt_unchk_med as f64 / compile_chk_med as f64;
                    format!("{speedup:.1}x")
                } else {
                    "—".to_string()
                }
            }
            Outcome::Ok => {
                let chk_total = compile_chk_med as i128 + rt_chk_med as i128;
                let unchk_total = compile_unchk_med as i128 + rt_unchk_med as i128;
                if unchk_total > 0 {
                    let pct =
                        (chk_total - unchk_total) as f64 * 100.0 / unchk_total as f64;
                    format!("{pct:+.2}%")
                } else {
                    "—".to_string()
                }
            }
        }
    };

    eprintln!(
        "{:<10} {:<28} {:>13} {:>15} {:>10} {:>11} {:>9} {:>11}",
        case.category.label(),
        case.id,
        us(compile_chk_med),
        us(compile_unchk_med),
        ms(rt_chk_med),
        ms(rt_unchk_med),
        outcome.label(),
        impact_str,
    );

    // Two flavors of soundness anomaly, surfaced under one warning
    // umbrella so a reader scanning the table sees ⚠ once per
    // problematic case:
    //
    //   1. Outcome mismatch — `outcome != expected`. On an
    //      LDBC-paired case set every case has a single expected
    //      outcome; a mismatch is a regression somewhere in the
    //      compile pipeline (typechecker, parser, schema inference).
    //
    //   2. Empty-but-nonempty — typechecker asserted `Outcome::Empty`
    //      but the unchecked runtime returned rows. Straightforward
    //      typechecker unsoundness — the §10 short-circuit would
    //      have discarded real results.
    //
    // Naming "SOUNDNESS" is the strict label for case 2; case 1 is
    // strictly speaking a "regression" rather than an unsoundness,
    // but we use a single umbrella so the tail count is one number.
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
