//! scenario_bench — usage-scenario benchmark for the typechecker's
//! cost/benefit in the sessions users actually run, not in isolation.
//!
//!   cargo run --release --features bench --bin scenario_bench -- \
//!       [path.gdb] [--scenario name] [--reps N] [--out file.csv]
//!
//! Motivation ("we cannot improve what we cannot measure"): the internal
//! bench measures the full pipeline per case, and `pattern_typecheck`
//! measures the checker in a hot loop — neither models a *session*: the
//! amortization horizon, shape-novelty distribution, human think-time
//! (which evicts hardware caches between queries), error mix, and DML
//! interleaving that decide what typechecking actually costs and saves
//! in consumption. This bench replays named session profiles end-to-end,
//! each in two configurations:
//!
//!   * `tc-on` — compile with the typechecker against the active schema;
//!     invalid queries are rejected, guaranteed-empty queries skip the
//!     runtime (the REPL's behavior).
//!   * `tc-off` — compile unchecked and always execute (the counterfactual).
//!
//! Profiles:
//!   * `repl_explorer` — interactive session: shape drift, typos/errors,
//!     a DML, one catastrophic-if-executed query; an LLC-sized eviction
//!     sweep before every query stands in for human think-time.
//!     (Limitation: the sweep cannot model frequency scaling or
//!     predictor decay of real seconds-long pauses.)
//!   * `sdk_app` — bindings/API usage: fixed reviewed templates executed
//!     repeatedly with different literals (errors don't occur here by
//!     construction; the question is the typechecker's TAX, not savings).
//!   * `one_shot_*` — single-query sessions; cost-to-first-verdict with
//!     the setup each path actually needs (eager vs lazy index warm).
//!   * `write_heavy` — DML→query cycles under a lazy runtime: after every
//!     mutation the runtime's TripleIndex is invalidated (~seconds to
//!     rebuild at SF0.1) while the typechecker's schema-side caches
//!     rebuild lazily in microseconds — the invalidation asymmetry.
//!
//! Fairness rules: fresh store + fresh caches per session (cold starts
//! are real); config order alternates AB/BA across repetitions so
//! machine drift cancels; setup time and RSS are first-class outputs
//! (the amortization/memory ledger), because per-query latency alone
//! hides what the runtime's fast paths cost up front.

use std::fs::File;
use std::hint::black_box;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};

use frogql::parser;
use frogql::runtime::dm::run_dm;
use frogql::runtime::engine::Runtime;
use frogql::store::lazy::LazyGraphStore;
use frogql::syntax::statement::Statement;

const DEFAULT_GDB: &str = "bench/data/ldbc-sf0.1.gdb";
const EXEC_LIMIT: usize = 20;
const DEFAULT_REPS: usize = 3;
/// LLC on the dev box is 12 MiB; a 32 MiB sweep evicts it deterministically.
const EVICT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq)]
enum Cfg {
    TcOn,
    TcOff,
}

impl Cfg {
    fn name(self) -> &'static str {
        match self {
            Cfg::TcOn => "tc-on",
            Cfg::TcOff => "tc-off",
        }
    }
}

enum Step {
    Query(String),
    Dml(String),
}

struct Scenario {
    name: &'static str,
    steps: Vec<Step>,
    /// Evict hardware caches before every query (human think-time).
    think: bool,
    /// Warm the TripleIndex at open (as the REPL/bindings do today).
    eager_warm: bool,
}

fn repl_explorer() -> Scenario {
    let q = |s: &str| Step::Query(s.to_string());
    Scenario {
        name: "repl_explorer",
        think: true,
        eager_warm: true,
        steps: vec![
            q("MATCH (p: Person) WHERE p.id = 933 RETURN p.firstName"),
            // Typo'd label: schema-empty. tc-on rejects; tc-off scans.
            q("MATCH (p: Persn) WHERE p.id = 933 RETURN p.firstName"),
            q("MATCH (p: Person) WHERE p.id = 933 RETURN p.firstName"),
            q("MATCH (p: Person)~[:knows]~(f: Person) WHERE p.id = 933 RETURN f.firstName"),
            // Typo'd edge label.
            q("MATCH (p: Person)~[:nows]~(f: Person) WHERE p.id = 933 RETURN f.firstName"),
            q("MATCH (p: Person)~[:knows]~(f: Person) WHERE p.id = 933 RETURN f.firstName"),
            // Unbound variable: invalid. tc-off silently returns 0 rows after work.
            q("MATCH (p: Person)~[:knows]~(f: Person) WHERE q.id = 933 RETURN f.firstName"),
            q("MATCH (p: Person)~[:knows]~(f: Person) WHERE p.id = 933 \
               RETURN f.firstName ORDER BY f.firstName LIMIT 10"),
            // The guardrail case: schema-empty only at the chain's leaf —
            // invisible to label-index pruning, catastrophic unchecked.
            q(
                "MATCH (a: Person)~[:knows]~(b: Person)~[:knows]~(c: Person)~[:knows]~(d: Wagumi) \
               RETURN a.id",
            ),
            q(
                "MATCH (p: Person)~[:knows]~(f: Person)~[:knows]~(g: Person) \
               WHERE p.id = 933 RETURN g.firstName LIMIT 10",
            ),
            Step::Dml("INSERT (:Person {id: 999999901, firstName: 'Bench'})".to_string()),
            // Post-DML: the runtime's TripleIndex was invalidated.
            q("MATCH (p: Person) WHERE p.id = 933 RETURN p.firstName"),
            q(
                "MATCH (m: Comment)-[:hasCreator]->(p: Person) WHERE p.id = 933 \
               RETURN m.id LIMIT 10",
            ),
            q("MATCH (p: Person)~[:knows]~(f: Person) OPTIONAL MATCH \
               (f)<-[:hasCreator]-(c: Comment) RETURN p.id LIMIT 10"),
            q(
                "MATCH (p: Person)~[:knows]~(f: Person) WHERE EXISTS { MATCH \
               (f)<-[:hasCreator]-(c: Comment) } RETURN p.id LIMIT 10",
            ),
            // Type clash: firstName is a string.
            q("MATCH (p: Person) WHERE p.firstName = 933 RETURN p.id"),
            q(
                "MATCH (m: Comment)-[:hasCreator]->(p: Person) WHERE p.id = 933 \
               RETURN m.id LIMIT 10",
            ),
            // Label-visible empty: the case the runtime rejects fast too.
            q("MATCH (x: Wagumi) RETURN x.id"),
            q("MATCH (p: Person)~[:knows]~(f: Person) WHERE p.id = 933 RETURN f.firstName"),
        ],
    }
}

fn sdk_app() -> Scenario {
    // Fixed, reviewed templates — errors cannot occur by construction.
    // The app formats literals into the string per request, exactly like
    // a REST handler would.
    let templates = [
        "MATCH (p: Person)~[:knows]~(f: Person) WHERE p.id = {ID} RETURN f.firstName",
        "MATCH (m: Comment)-[:hasCreator]->(p: Person) WHERE p.id = {ID} RETURN m.id LIMIT 10",
        "MATCH (p: Person) WHERE p.id = {ID} RETURN p.firstName",
    ];
    let ids = [933u64, 1129, 4194, 6597, 8333, 10995];
    let mut steps = Vec::new();
    for i in 0..100 {
        for t in &templates {
            let id = ids[i % ids.len()];
            steps.push(Step::Query(t.replace("{ID}", &id.to_string())));
        }
    }
    Scenario {
        name: "sdk_app",
        think: false,
        eager_warm: true,
        steps,
    }
}

fn one_shot(name: &'static str, query: &str, eager_warm: bool) -> Scenario {
    Scenario {
        name,
        think: false,
        eager_warm,
        steps: vec![Step::Query(query.to_string())],
    }
}

fn write_heavy() -> Scenario {
    // Lazy runtime: the index is built only if something executes. tc-on
    // rejects every query in the cycle, so it never builds the index at
    // all; tc-off rebuilds it after every DML just to discover 0 rows.
    let mut steps = Vec::new();
    for i in 0..5 {
        steps.push(Step::Dml(format!(
            "INSERT (:Person {{id: 99999991{i}, firstName: 'Bench'}})"
        )));
        steps.push(Step::Query(
            "MATCH (a: Person)~[:knows]~(b: Person)-[:noSuchEdge]->(c: Person) RETURN a.id"
                .to_string(),
        ));
    }
    // One valid query at the end so both configs pay one real execution.
    steps.push(Step::Query(
        "MATCH (p: Person) WHERE p.id = 933 RETURN p.firstName".to_string(),
    ));
    Scenario {
        name: "write_heavy",
        think: false,
        eager_warm: false,
        steps,
    }
}

fn rss_mb(sys: &mut System) -> f64 {
    let pid = Pid::from_u32(std::process::id());
    sys.refresh_process_specifics(pid, ProcessRefreshKind::new().with_memory());
    sys.process(pid)
        .map(|p| p.memory() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0)
}

fn evict(buf: &mut [u8]) {
    for i in (0..buf.len()).step_by(64) {
        buf[i] = buf[i].wrapping_add(1);
    }
    black_box(&buf[buf.len() / 2]);
}

struct SessionSummary {
    open_ms: f64,
    warm_ms: f64,
    compile_ns_total: u128,
    exec_ns_total: u128,
    session_ns: u128,
    rejected: usize,
    executed: usize,
    rss_after_mb: f64,
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    gdb: &Path,
    sc: &Scenario,
    cfg: Cfg,
    rep: usize,
    sys: &mut System,
    evict_buf: &mut [u8],
    csv: &mut File,
) -> SessionSummary {
    let t = Instant::now();
    let lazy = LazyGraphStore::open(gdb).unwrap_or_else(|e| panic!("open {gdb:?}: {e}"));
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;
    let schema = lazy.catalog().active_schema();
    let rt = Runtime::new(&lazy);
    let warm_ms = if sc.eager_warm {
        let t = Instant::now();
        rt.warm_triple_index();
        t.elapsed().as_secs_f64() * 1000.0
    } else {
        0.0
    };

    let mut sum = SessionSummary {
        open_ms,
        warm_ms,
        compile_ns_total: 0,
        exec_ns_total: 0,
        session_ns: 0,
        rejected: 0,
        executed: 0,
        rss_after_mb: 0.0,
    };

    let session_t = Instant::now();
    for (idx, step) in sc.steps.iter().enumerate() {
        match step {
            Step::Dml(stmt) => {
                let t = Instant::now();
                match parser::parse_statement(stmt) {
                    Ok(Statement::DataModification(dm)) => {
                        // DEFAULT graph type is active → no G2000 validation.
                        run_dm(&lazy, &dm, None).unwrap_or_else(|e| panic!("dml failed: {e}"));
                        rt.invalidate_caches();
                    }
                    other => panic!("expected DML statement, got {other:?}"),
                }
                let ns = t.elapsed().as_nanos();
                writeln!(
                    csv,
                    "{};{};{};{};dml;{ns};applied",
                    sc.name,
                    cfg.name(),
                    rep,
                    idx
                )
                .unwrap();
            }
            Step::Query(q) => {
                if sc.think {
                    evict(evict_buf);
                }
                let (compile_ns, exec_ns, outcome) = match cfg {
                    Cfg::TcOn => {
                        let t = Instant::now();
                        let compiled = frogql::compile_query_with_diagnostics_with(&schema, q);
                        let compile_ns = t.elapsed().as_nanos();
                        match compiled {
                            Err(_) => (compile_ns, 0, "rejected_invalid"),
                            Ok(cr) if cr.guaranteed_empty => (compile_ns, 0, "rejected_empty"),
                            Ok(cr) => {
                                let t = Instant::now();
                                black_box(rt.run_query(&cr.query, EXEC_LIMIT));
                                (compile_ns, t.elapsed().as_nanos(), "executed")
                            }
                        }
                    }
                    Cfg::TcOff => {
                        let t = Instant::now();
                        let compiled = frogql::compile_query_unchecked(q);
                        let compile_ns = t.elapsed().as_nanos();
                        match compiled {
                            Err(_) => (compile_ns, 0, "parse_error"),
                            Ok(query) => {
                                let t = Instant::now();
                                black_box(rt.run_query(&query, EXEC_LIMIT));
                                (compile_ns, t.elapsed().as_nanos(), "executed")
                            }
                        }
                    }
                };
                sum.compile_ns_total += compile_ns;
                sum.exec_ns_total += exec_ns;
                if outcome.starts_with("rejected") {
                    sum.rejected += 1;
                } else if outcome == "executed" {
                    sum.executed += 1;
                }
                writeln!(
                    csv,
                    "{};{};{};{};compile;{compile_ns};{outcome}",
                    sc.name,
                    cfg.name(),
                    rep,
                    idx
                )
                .unwrap();
                writeln!(
                    csv,
                    "{};{};{};{};exec;{exec_ns};{outcome}",
                    sc.name,
                    cfg.name(),
                    rep,
                    idx
                )
                .unwrap();
            }
        }
    }
    sum.session_ns = session_t.elapsed().as_nanos();
    sum.rss_after_mb = rss_mb(sys);
    sum
}

fn main() {
    let mut gdb: Option<String> = None;
    let mut only: Option<String> = None;
    let mut reps = DEFAULT_REPS;
    let mut out = "scenario_bench_results.csv".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--scenario" => only = args.next(),
            "--reps" => reps = args.next().and_then(|v| v.parse().ok()).unwrap_or(reps),
            "--out" => out = args.next().unwrap_or(out),
            other if gdb.is_none() => gdb = Some(other.to_string()),
            other => panic!("unexpected argument: {other}"),
        }
    }
    let gdb = gdb.unwrap_or_else(|| DEFAULT_GDB.to_string());
    let gdb = Path::new(&gdb);

    let scenarios: Vec<Scenario> = vec![
        repl_explorer(),
        sdk_app(),
        one_shot(
            "one_shot_valid_eager",
            "MATCH (p: Person)~[:knows]~(f: Person) WHERE p.id = 933 RETURN f.firstName",
            true,
        ),
        one_shot(
            "one_shot_reject_eager",
            "MATCH (a: Person)~[:knows]~(b: Person)-[:noSuchEdge]->(c: Person) RETURN a.id",
            true,
        ),
        one_shot(
            "one_shot_reject_lazy",
            "MATCH (a: Person)~[:knows]~(b: Person)-[:noSuchEdge]->(c: Person) RETURN a.id",
            false,
        ),
        write_heavy(),
    ];

    let mut sys = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::new().with_memory()),
    );
    let rss_baseline = rss_mb(&mut sys);
    let mut evict_buf = vec![0u8; EVICT_BYTES];
    let mut csv = File::create(&out).unwrap_or_else(|e| panic!("create {out}: {e}"));
    writeln!(csv, "scenario;config;rep;step;phase;ns;outcome").unwrap();

    eprintln!("scenario_bench: db={gdb:?} reps={reps} rss_baseline={rss_baseline:.1} MiB");
    eprintln!(
        "\n{:<24}{:<7}{:>4}{:>10}{:>10}{:>12}{:>12}{:>12}{:>6}{:>6}{:>10}",
        "scenario",
        "config",
        "rep",
        "open_ms",
        "warm_ms",
        "compile_ms",
        "exec_ms",
        "session_ms",
        "rej",
        "exec",
        "rss_MiB"
    );
    eprintln!("{}", "-".repeat(113));

    for sc in &scenarios {
        if let Some(f) = &only {
            if !sc.name.contains(f.as_str()) {
                continue;
            }
        }
        for rep in 0..reps {
            // AB/BA interleave so machine drift cancels across configs.
            let order = if rep % 2 == 0 {
                [Cfg::TcOn, Cfg::TcOff]
            } else {
                [Cfg::TcOff, Cfg::TcOn]
            };
            for cfg in order {
                let s = run_session(gdb, sc, cfg, rep, &mut sys, &mut evict_buf, &mut csv);
                eprintln!(
                    "{:<24}{:<7}{:>4}{:>10.1}{:>10.1}{:>12.3}{:>12.1}{:>12.1}{:>6}{:>6}{:>10.1}",
                    sc.name,
                    cfg.name(),
                    rep,
                    s.open_ms,
                    s.warm_ms,
                    s.compile_ns_total as f64 / 1e6,
                    s.exec_ns_total as f64 / 1e6,
                    s.session_ns as f64 / 1e6,
                    s.rejected,
                    s.executed,
                    s.rss_after_mb
                );
                writeln!(
                    csv,
                    "{};{};{};-1;session_total;{};open_ms={:.1} warm_ms={:.1} rss_mb={:.1}",
                    sc.name,
                    cfg.name(),
                    rep,
                    s.session_ns,
                    s.open_ms,
                    s.warm_ms,
                    s.rss_after_mb
                )
                .unwrap();
            }
        }
    }
    eprintln!("{}", "-".repeat(113));
    eprintln!("wrote {out}");
}
