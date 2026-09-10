//! Run a file of queries under every vector-search arm, opening the
//! database once.
//!
//! ```text
//! vec_sweep <db.gdb> <queries.gql> [options]
//!
//!   --arms <list>     comma-separated `strategy+source` pairs, or `all`
//!                     (default: the eight the study compares)
//!   --levels <list>   VEO levels for the in-LTJ arms, or `auto` for the
//!                     optimizer's own placement (default: 0)
//!   --iters <n>       runs per (arm, query); the median is reported (default: 3)
//!   --limit <n>       row cap per query, 0 for none (default: 0)
//!   --queries <list>  1-based indices to run, e.g. 1,4,17 (default: all)
//!   --csv <path>      write the CSV here as well as to stdout
//!   --no-recall       skip the exact reference run per query; the
//!                     `recall` column is then empty everywhere
//!   --timeout <secs>  abandon a query that runs longer, mark the row
//!                     `timeout`, and carry on with the next one
//!                     (default: 300s; `--timeout off` removes it)
//! ```
//!
//! # Why this exists
//!
//! The arms are selected by environment variables, which are read once per
//! process, so comparing seven of them from a shell means seven processes
//! and seven opens. On the RDF dump an open is ~220 s and the LTJ build is
//! another ~250 s, so the setup would dwarf the thing being measured and
//! the numbers would be dominated by page-cache state rather than by the
//! algorithm.
//!
//! Here the store, its indexes and the LTJ index are built once, before
//! any measurement, and `Runtime::set_vec_cfg` switches arms between
//! queries. Every timing is then the query and nothing else — the same
//! discipline `vec_bench` follows on synthetic data, applied to a real
//! database and real queries.
//!
//! # Reading the output
//!
//! One CSV row per (arm, level, query, iteration set):
//!
//! ```text
//! query,strategy,source,level,arm_actual,median_ms,min_ms,max_ms,rows,recall,truth_keys,nn_pops,nn_expanded,pattern_runs,ltj_visits,candidates,anchor_groups,fallback
//! ```
//!
//! **`recall` is what makes an `hnsw` row comparable to a `localsort`
//! one.** The exact sources return the true nearest matches; `hnsw`
//! navigates a proximity graph and can miss some. Comparing the two on
//! latency alone credits `hnsw` for work it skipped — a faster arm that
//! answered less is not a faster arm. `rows` does not catch it either: an
//! approximate walk can return the same *number* of rows and a different
//! *set*, swapping a true neighbour for the next one out.
//!
//! Ground truth is one exact run per query, `post+localsort`, taken
//! before the sweep. One suffices because every strategy under an exact
//! source returns identical answers — `tests/vector_strategy_equiv_test.rs`
//! is what pins that — so the reference does not have to match the arm.
//! Recall is `|arm ∩ truth| / |truth|` over whole projected rows taken as
//! a **set**, which is what `truth_keys` counts — and why it can sit well
//! below `rows`. A `NEAREST 5` in distinct-binding mode returns five
//! images and one row per way the pattern reaches each, so twenty-one
//! rows can carry five answers. Duplicates from the join say nothing
//! about whether the nearest were found. Recall is 1.0 for every exact
//! arm by construction: a value below 1.0 there is a bug, and below 1.0
//! on `hnsw` is the result being measured.
//!
//! A query whose reference did not finish inside the budget has no truth
//! to compare against, and its `recall` is left **empty** rather than
//! guessed. `--no-recall` skips the reference pass entirely, which is the
//! right call only when latency is all that is wanted.
//!
//! **`arm_actual` is the column to check first.** A strategy that meets a
//! shape it cannot hook into falls back, and a row reporting the requested
//! arm rather than the executed one is a lie. A *correlated* `NEAREST` —
//! one whose query vector names a pattern variable, like
//! `VECTOR(v10, 'hog')` — has one ranking per anchor rather than one for
//! the query. `post`, `interleave` and `memo` all have a correlated form
//! and report their own arm; `pre` does not and reports
//! `correlated+<source>`, the partition-and-rank plan, with the reason in
//! the `fallback` column.
//!
//! A **`timeout`** in the `fallback` column means the row is a partial
//! result and not a measurement: the budget ran out, the search returned
//! what it had, and the `rows` and counter columns describe an unfinished
//! walk. Read it as "this combination did not finish inside the budget",
//! never as a latency.
//!
//! There is a five-minute budget by default, because a bad combination
//! runs until it finishes and on a large corpus that can be days. Pass
//! `--timeout off` to remove it — which is the right call when the
//! question *is* how long a bad arm takes.
//!
//! Two things the budget does not promise. It is **cooperative and
//! partial in coverage**: honoured in the LTJ search, the ranking walks
//! and the post-/pre-filter and correlated paths, but the hash-join
//! fallback, the repetition enumerators and the shortest-path searches do
//! not consult it, so an arm that falls to one of those is not bounded by
//! it. And it is **coarse**: the clock is read once every 4096 asks, so a
//! run can overrun by that much work.
//!
//! `nn_pops` per accepted row is the headline number: with a selective
//! pattern the corpus-walking sources reach a candidate that also
//! satisfies the pattern only after a large fraction of the corpus, while
//! `localsort` never leaves the candidate set.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{Duration, Instant};

use frogql::runtime::engine::Runtime;
use frogql::runtime::vsearch::{Strategy, VecCfg, VecSource};
use frogql::store::lazy::LazyGraphStore;

/// The arms of the study, in the order the write-up lists them.
const DEFAULT_ARMS: [(Strategy, VecSource); 8] = [
    // 1 — post-filter, no metric index
    (Strategy::PostFilter, VecSource::LocalSort),
    // 2 — post-filter, with index
    (Strategy::PostFilter, VecSource::Hnsw),
    // 3 — in-LTJ, with index
    (Strategy::Interleave, VecSource::Hnsw),
    // 4 — in-LTJ, no index
    (Strategy::Interleave, VecSource::LocalSort),
    // 5 — global pre-sort (a special case of 3, by its source)
    (Strategy::Interleave, VecSource::GlobalSort),
    // 6 — pre-filter: substitute each neighbour and re-run
    (Strategy::PreFilter, VecSource::Hnsw),
    // 7 — memo: consult the ranking once, globally
    (Strategy::Memo, VecSource::Hnsw),
    // 7b — memo against the source that has been winning. The set above
    // pairs `memo` with HNSW alone, which reads the comparison backwards:
    // `localsort` beats both corpus-walking sources in every in-LTJ row
    // measured so far, and `memo+localsort` holds the lowest `nn_pops` of
    // any arm (11 against `interleave+localsort`'s 1 050 at level 1). A
    // default set that omits it under-samples the source under suspicion.
    //
    // Note it only says something off level 0: one visit means nothing to
    // re-walk, so `memo` cannot win there and does not. Pass
    // `--levels 0,1` to give this row a level where it can.
    (Strategy::Memo, VecSource::LocalSort),
];

const ALL_ARMS: [(Strategy, VecSource); 11] = [
    (Strategy::PostFilter, VecSource::Hnsw),
    (Strategy::PostFilter, VecSource::LocalSort),
    (Strategy::PostFilter, VecSource::GlobalSort),
    (Strategy::PreFilter, VecSource::Hnsw),
    (Strategy::PreFilter, VecSource::GlobalSort),
    (Strategy::Interleave, VecSource::Hnsw),
    (Strategy::Interleave, VecSource::LocalSort),
    (Strategy::Interleave, VecSource::GlobalSort),
    (Strategy::Memo, VecSource::Hnsw),
    (Strategy::Memo, VecSource::LocalSort),
    (Strategy::Memo, VecSource::GlobalSort),
];

/// Wall-clock budget per query run when `--timeout` is not given.
///
/// A default rather than `None` because the failure it prevents is not a
/// slow sweep but an unbounded one: an arm that walks a corpus-wide
/// ranking for every visit of a deep level can run for days on a large
/// database, and the row it would eventually produce is one nobody is
/// waiting for. Five minutes is far above any arm that is working and far
/// below the ones that are not.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

struct Args {
    db: PathBuf,
    queries: PathBuf,
    arms: Vec<(Strategy, VecSource)>,
    levels: Vec<Option<usize>>,
    iters: usize,
    limit: usize,
    only: Option<Vec<usize>>,
    csv: Option<PathBuf>,
    /// Skip the exact reference run that `recall` is measured against.
    no_recall: bool,
    /// Wall-clock budget per query run. `None` leaves the sweep
    /// unbounded, which is what a bad arm needs days of.
    timeout: Option<Duration>,
}

fn usage() -> ! {
    eprintln!(
        "usage: vec_sweep <db.gdb> <queries.gql> [options]\n\
         \n\
         options:\n  \
           --arms <list>     `strategy+source` pairs, or `all` (default: the study's eight)\n  \
           --levels <list>   VEO levels for in-LTJ arms, or `auto` (default: 0)\n  \
           --iters <n>       runs per (arm, query); median reported (default: 3)\n  \
           --limit <n>       row cap per query, 0 for none (default: 0)\n  \
           --queries <list>  1-based query indices to run (default: all)\n  \
           --csv <path>      also write the CSV to this file\n  \
           --no-recall       skip the exact reference run; recall stays empty\n  \
           --timeout <secs>  abandon a query that runs longer; the row is\n  \
           \x20                marked `timeout` and the sweep continues\n  \
           \x20                (default: 300; `off` removes the budget)"
    );
    process::exit(2)
}

fn parse_arm(s: &str) -> (Strategy, VecSource) {
    let (a, b) = match s.split_once('+') {
        Some(p) => p,
        None => {
            eprintln!("error: `{s}` is not `strategy+source`, e.g. `post+hnsw`");
            usage()
        }
    };
    let strategy = Strategy::parse(a).unwrap_or_else(|| {
        eprintln!("error: unknown strategy `{a}` (post|pre|interleave|memo)");
        usage()
    });
    let source = VecSource::parse(b).unwrap_or_else(|| {
        eprintln!("error: unknown source `{b}` (hnsw|localsort|globalsort)");
        usage()
    });
    (strategy, source)
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut arms = DEFAULT_ARMS.to_vec();
    let mut levels: Vec<Option<usize>> = vec![Some(0)];
    let mut iters = 3usize;
    let mut limit = 0usize;
    let mut only: Option<Vec<usize>> = None;
    let mut csv = None;
    let mut no_recall = false;
    let mut timeout = Some(DEFAULT_TIMEOUT);

    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].clone();
        let mut value = |name: &str| -> String {
            i += 1;
            argv.get(i).cloned().unwrap_or_else(|| {
                eprintln!("error: {name} needs a value");
                usage()
            })
        };
        match a.as_str() {
            "--arms" => {
                let v = value("--arms");
                arms = if v == "all" {
                    ALL_ARMS.to_vec()
                } else {
                    v.split(',').map(parse_arm).collect()
                };
            }
            "--levels" => {
                // `auto` is the un-pinned placement: the optimizer puts
                // the search variable where its ordering heuristic wants
                // it, which is the only setting an adaptive VEO can serve
                // and the only one that is not partly measuring how much
                // that heuristic was worth.
                levels = value("--levels")
                    .split(',')
                    .map(|s| {
                        let t = s.trim();
                        if t.eq_ignore_ascii_case("auto") || t.eq_ignore_ascii_case("free") {
                            None
                        } else {
                            Some(t.parse().unwrap_or(0))
                        }
                    })
                    .collect()
            }
            "--iters" => iters = value("--iters").parse().unwrap_or(3).max(1),
            "--limit" => limit = value("--limit").parse().unwrap_or(0),
            "--queries" => {
                only = Some(
                    value("--queries")
                        .split(',')
                        .filter_map(|s| s.trim().parse().ok())
                        .collect(),
                )
            }
            "--csv" => csv = Some(PathBuf::from(value("--csv"))),
            "--no-recall" => no_recall = true,
            "--timeout" => {
                let v = value("--timeout");
                timeout = match v.trim() {
                    // Opting out has to be spellable: a sweep meant to
                    // find out how long a bad arm actually takes needs no
                    // budget, and the default would silently truncate it.
                    "0" | "off" | "none" => None,
                    other => match other.parse::<f64>() {
                        Ok(secs) if secs > 0.0 => Some(Duration::from_secs_f64(secs)),
                        _ => {
                            eprintln!(
                                "error: --timeout wants a positive number of seconds, or `off`"
                            );
                            usage()
                        }
                    },
                };
            }
            "-h" | "--help" => usage(),
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag `{other}`");
                usage()
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    if positional.len() != 2 {
        eprintln!("error: expected a database and a query file");
        usage();
    }
    Args {
        db: PathBuf::from(&positional[0]),
        queries: PathBuf::from(&positional[1]),
        arms,
        levels,
        iters,
        limit,
        only,
        csv,
        no_recall,
        timeout,
    }
}

/// One statement per non-blank line, trailing `;` optional. That is the
/// shape `sparql_to_gql.py --batch --one-line` produces and the shape the
/// REPL accepts, so the same file works in both.
fn read_queries(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("error: cannot read {}: {e}", path.display());
        process::exit(1);
    });
    text.lines()
        .map(|l| l.trim().trim_end_matches(';').trim())
        .filter(|l| !l.is_empty() && !l.starts_with("--"))
        .map(|l| l.to_string())
        .collect()
}

/// Every projected row of a result, formatted — the unit recall is
/// measured in.
///
/// The whole row and not its first column: two arms can agree on which
/// images they found and disagree on what they joined them to, and a
/// recall that could not see the difference would report agreement that
/// is not there.
fn key_set(r: &frogql::runtime::result::QueryResult) -> std::collections::HashSet<String> {
    use frogql::runtime::result::QueryResult;
    match r {
        QueryResult::Projected(rows) => rows.iter().map(|row| format!("{row:?}")).collect(),
        QueryResult::Raw(ir) => ir
            .rows
            .iter()
            .map(|row| format!("{:?}", row.paths))
            .collect(),
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn main() {
    let args = parse_args();

    // Everything expensive happens here, once, before any timing: the
    // store, the auto-built secondary indexes, and the LTJ index (read
    // from `<db>.ltj` when `ltj_build` has written one).
    let t = Instant::now();
    let store = LazyGraphStore::open(&args.db).unwrap_or_else(|e| {
        eprintln!("error: cannot open {}: {e}", args.db.display());
        process::exit(1);
    });
    let (nodes, edges) = {
        use frogql::model::graph_access::GraphAccess;
        store
            .index_sidecar_key()
            .map(|(_, n, e)| (n, e))
            .unwrap_or((0, 0))
    };
    eprintln!(
        "opened {} ({nodes} nodes, {edges} edges) in {:.1}s",
        args.db.display(),
        t.elapsed().as_secs_f64()
    );

    let rt = Runtime::new(&store);
    rt.set_query_budget(args.timeout);
    let t = Instant::now();
    let index = rt.warm_triple_index();
    eprintln!(
        "LTJ index ready ({} triples) in {:.1}s",
        index.len(),
        t.elapsed().as_secs_f64()
    );

    let raw = read_queries(&args.queries);
    // Compile once. A compile error is fatal rather than skipped: a sweep
    // that quietly drops a query reports a mean over a set nobody chose.
    let mut queries = Vec::with_capacity(raw.len());
    for (i, q) in raw.iter().enumerate() {
        match frogql::compile_query(q) {
            Ok(c) => queries.push((i + 1, c)),
            Err(e) => {
                eprintln!("error: query {} does not compile: {e}", i + 1);
                process::exit(1);
            }
        }
    }
    if let Some(only) = &args.only {
        queries.retain(|(i, _)| only.contains(i));
    }
    eprintln!(
        "{} queries x {} arms x {} level(s) x {} iters, budget {}",
        queries.len(),
        args.arms.len(),
        args.levels.len(),
        args.iters,
        // The budget is on by default, so it has to be visible: a reader
        // who does not know a row was cut short reads a partial walk as a
        // latency. The `timeout` column says it per row; this says it once.
        match args.timeout {
            Some(d) => format!("{:.0}s", d.as_secs_f64()),
            None => "off".to_string(),
        }
    );

    // Ground truth, once per query, before anything is measured. One
    // exact run suffices for every arm: under an exact source all the
    // strategies return identical answers, which is what
    // `tests/vector_strategy_equiv_test.rs` exists to pin.
    //
    // `post+localsort` is the reference because it is the arm with the
    // fewest ways to decline: it runs the pattern through the ordinary
    // path and ranks what came out, so it needs no decomposition into
    // triples and no legal level to occupy.
    let truth: Vec<Option<std::collections::HashSet<String>>> = if args.no_recall {
        vec![None; queries.len()]
    } else {
        let t = Instant::now();
        rt.set_vec_cfg(VecCfg {
            strategy: Strategy::PostFilter,
            source: VecSource::LocalSort,
            level: None,
            ..VecCfg::default()
        });
        let truth: Vec<Option<std::collections::HashSet<String>>> = queries
            .iter()
            .map(|(_, q)| {
                let r = rt.run_query(q, args.limit);
                // A reference that ran out of budget describes an
                // unfinished walk, so it is not truth. Left absent rather
                // than used, which would score every arm against a
                // partial answer and read as poor recall everywhere.
                if rt.query_timed_out() {
                    None
                } else {
                    Some(key_set(&r))
                }
            })
            .collect();
        let missing = truth.iter().filter(|t| t.is_none()).count();
        eprintln!(
            "ground truth: {} of {} queries in {:.1}s{}",
            queries.len() - missing,
            queries.len(),
            t.elapsed().as_secs_f64(),
            if missing > 0 {
                format!(" ({missing} did not finish; their recall stays empty)")
            } else {
                String::new()
            }
        );
        truth
    };

    let header = "query,strategy,source,level,arm_actual,median_ms,min_ms,max_ms,\
                  rows,recall,truth_keys,nn_pops,nn_expanded,pattern_runs,ltj_visits,\
                  candidates,anchor_groups,fallback";
    println!("{header}");
    let mut csv_out = args.csv.as_ref().map(|p| {
        let mut f = std::fs::File::create(p).unwrap_or_else(|e| {
            eprintln!("error: cannot create {}: {e}", p.display());
            process::exit(1);
        });
        let _ = writeln!(f, "{header}");
        f
    });

    for (strategy, source) in &args.arms {
        // Only the in-LTJ arms read the level; running the others once
        // per level would repeat identical work and pad the output.
        let levels: &[Option<usize>] = if strategy.is_in_ltj() {
            &args.levels
        } else {
            &args.levels[..1]
        };
        for &level in levels {
            // `auto` in the level column, so a row cannot be read as
            // level 0 when nothing was pinned at all.
            let level_label = match level {
                Some(n) => n.to_string(),
                None => "auto".to_string(),
            };
            rt.set_vec_cfg(VecCfg {
                strategy: *strategy,
                source: *source,
                level,
                // The one knob the sweep does not own. `memo`'s walk cuts
                // are an optimization with a kill switch, and A/Bing them
                // over a real database is what the switch is for, so it
                // is read from the environment rather than pinned to the
                // default here.
                memo_cuts: std::env::var("FROGQL_DISABLE_MEMO_CUTS").is_err(),
                ..VecCfg::default()
            });
            for (qslot, (qi, query)) in queries.iter().enumerate() {
                let mut times = Vec::with_capacity(args.iters);
                let mut rows = 0usize;
                let mut timed_out = false;
                let mut last = None;
                for _ in 0..args.iters {
                    let t = Instant::now();
                    let result = rt.run_query(query, args.limit);
                    times.push(t.elapsed().as_secs_f64() * 1000.0);
                    rows = result.row_count();
                    last = Some(result);
                    // One expired iteration condemns the row: the rest
                    // measure an unfinished walk just as much.
                    timed_out |= rt.query_timed_out();
                    if timed_out {
                        break;
                    }
                }
                // Formatting the rows is not part of the measurement, so
                // it happens here rather than inside the timed loop.
                let (recall, truth_keys) = match (&truth[qslot], &last) {
                    // A partial walk is not a recall of anything: the row
                    // already says `timeout`, and scoring it would read as
                    // a bad index rather than an unfinished run.
                    (Some(t), Some(r)) if !timed_out => {
                        let got = key_set(r);
                        let hit = t.iter().filter(|k| got.contains(*k)).count();
                        let recall = if t.is_empty() {
                            1.0
                        } else {
                            hit as f64 / t.len() as f64
                        };
                        (format!("{recall:.4}"), t.len().to_string())
                    }
                    (Some(t), _) => (String::new(), t.len().to_string()),
                    _ => (String::new(), String::new()),
                };
                let s = rt.last_vec_stats();
                let lo = times.iter().cloned().fold(f64::INFINITY, f64::min);
                let hi = times.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let line = format!(
                    "{qi},{},{},{level_label},{},{:.3},{:.3},{:.3},{rows},\
                     {recall},{truth_keys},{},{},{},{},{},{},{}",
                    strategy.name(),
                    source.name(),
                    s.arm,
                    median(times),
                    lo,
                    hi,
                    s.nn_pops,
                    s.nn_expanded,
                    s.pattern_runs,
                    s.ltj_visits,
                    s.candidates_hashed,
                    s.anchor_groups,
                    // Commas would break the column; the reason is prose.
                    // `timeout` wins over any fallback text: it says the
                    // numbers on this row describe an unfinished walk, and
                    // that is the first thing a reader has to know.
                    if timed_out {
                        "timeout".to_string()
                    } else {
                        s.fallback_reason.as_deref().unwrap_or("").replace(',', ";")
                    }
                );
                println!("{line}");
                if let Some(f) = csv_out.as_mut() {
                    let _ = writeln!(f, "{line}");
                }
            }
            eprintln!(
                "  done {}+{} level {level_label}",
                strategy.name(),
                source.name()
            );
        }
    }

    if let Some(p) = &args.csv {
        eprintln!("wrote {}", p.display());
    }
    if args.timeout.is_none() {
        eprintln!(
            "note: the budget was turned off, so every row ran to completion; \
             a combination that cannot finish would have hung the sweep"
        );
    }
}
