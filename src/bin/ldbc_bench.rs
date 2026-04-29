//! LDBC SNB Interactive workload benchmark runner.
//!
//! Section 6 of the LDBC SNB v1 paper (arXiv:2001.02299) defines the
//! Interactive Complex (IC) reads. This binary runs the subset that
//! gqlite supports today, and reports both wall time and RSS per
//! backend (Memory / Lazy / Disk).
//!
//! ## Query catalog
//!
//! Each IC is described by a `bench/ldbc-queries/ic<n>.toml` file:
//!
//! - `status = "implemented"` queries carry a `query` template and a
//!   `params_file` reference; the runner substitutes `{paramName}`
//!   placeholders against the LDBC `substitution_parameters-sf0.1/
//!   interactive_<n>_param.txt` file.
//! - `status = "blocked"` queries carry a `blocked_reason` and
//!   `required_features` listing the gqlite gaps. The runner skips
//!   them by default and prints the reason in `--show-blocked` mode.
//!
//! ## Usage
//!
//!     ldbc_bench <db.gdb> [--ic <n>|all|blocked]
//!                         [--backend memory|lazy|disk]
//!                         [--params-dir <dir>]
//!                         [--queries-dir <dir>]
//!                         [--iters N] [--limit N]
//!                         [--csv-dir <dir>]      # required for --backend memory
//!
//! Three GraphAccess backends:
//!   - **memory** — `Graph` from `csv_loader::load_from_ldbc_csv_dir`.
//!     Needs `--csv-dir`; the .gdb path is ignored.
//!   - **lazy** — `LazyGraphStore::open(.gdb)`. Default.
//!   - **disk** — `DiskGraphStore::open(.gdb)`.
//!
//! Each run reports peak RSS via `sysinfo`. See `bench/LDBC_BENCHMARK.md`.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};

use gqlrust::compile_query_unchecked;
use gqlrust::model::csv_loader;
use gqlrust::model::graph_access::GraphAccess;
use gqlrust::runtime::engine::Runtime;
use gqlrust::store::disk::DiskGraphStore;
use gqlrust::store::lazy::LazyGraphStore;

// ---------------------------------------------------------------- Backend ---

#[derive(Clone, Copy, Debug)]
enum Backend {
    Memory,
    Lazy,
    Disk,
}

impl Backend {
    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "memory" | "mem" => Some(Backend::Memory),
            "lazy" => Some(Backend::Lazy),
            "disk" => Some(Backend::Disk),
            _ => None,
        }
    }
    fn label(&self) -> &'static str {
        match self {
            Backend::Memory => "memory",
            Backend::Lazy => "lazy",
            Backend::Disk => "disk",
        }
    }
}

// ----------------------------------------------------------- IcQuery TOML ---

/// Per-IC TOML file shape. Optional fields apply only to one of the
/// two `status` variants (see field comments).
#[derive(Debug, Deserialize)]
struct IcQuery {
    id: u32,
    name: String,
    status: Status,

    // implemented-only
    params_file: Option<String>,
    #[allow(dead_code)] // documentation aid; runtime uses header line of params file
    param_columns: Option<Vec<String>>,
    query: Option<String>,
    #[allow(dead_code)]
    return_columns: Option<Vec<String>>,
    #[serde(default)]
    #[allow(dead_code)]
    divergences: HashMap<String, String>,

    // blocked-only
    blocked_reason: Option<String>,
    required_features: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Status {
    Implemented,
    Blocked,
}

fn load_queries(dir: &Path) -> Vec<IcQuery> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir).unwrap_or_else(|e| {
        eprintln!("failed to read queries dir {}: {e}", dir.display());
        std::process::exit(1);
    });
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let raw = fs::read_to_string(&p).unwrap_or_else(|e| {
            eprintln!("failed to read {}: {e}", p.display());
            std::process::exit(1);
        });
        let q: IcQuery = toml::from_str(&raw).unwrap_or_else(|e| {
            eprintln!("failed to parse {}: {e}", p.display());
            std::process::exit(1);
        });
        out.push(q);
    }
    out.sort_by_key(|q| q.id);
    out
}

// ------------------------------------------------------------- Param file ---

/// Parse an LDBC substitution param file: `|`-separated, first line
/// is the header (column names), subsequent lines are param-value
/// rows. Returns `(header, rows)`.
fn load_params(path: &Path) -> (Vec<String>, Vec<Vec<String>>) {
    let text = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("failed to read params file {}: {e}", path.display());
        std::process::exit(1);
    });
    let mut lines = text.lines();
    let header: Vec<String> = lines
        .next()
        .expect("empty params file")
        .split('|')
        .map(str::to_string)
        .collect();
    let rows: Vec<Vec<String>> = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('|').map(str::to_string).collect())
        .collect();
    (header, rows)
}

/// Replace each `{colName}` in `template` with the corresponding
/// value from `row`, matched by position to `header`.
fn substitute(template: &str, header: &[String], row: &[String]) -> String {
    let mut out = template.to_string();
    for (h, v) in header.iter().zip(row.iter()) {
        out = out.replace(&format!("{{{h}}}"), v);
    }
    out
}

// ----------------------------------------------------------- RSS reading ---

fn rss_mb(sys: &mut System) -> f64 {
    let pid = Pid::from_u32(std::process::id());
    sys.refresh_process_specifics(pid, ProcessRefreshKind::new().with_memory());
    sys.process(pid)
        .map(|p| p.memory() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0)
}

// ----------------------------------------------------------------- Main ---

fn print_usage(prog: &str) {
    eprintln!(
        "Usage: {prog} <db.gdb> [--ic <n>|all|blocked]\n\
         \t[--backend memory|lazy|disk] [--params-dir <dir>] [--queries-dir <dir>]\n\
         \t[--iters N] [--limit N] [--csv-dir <dir>]\n\
         \n\
         Defaults: --ic 2  --backend lazy  --queries-dir bench/ldbc-queries\n\
         --csv-dir is required when --backend memory is used (the .gdb path is ignored).\n\
         --ic blocked prints the blocked-IC inventory (no bench run)."
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage(&args[0]);
        std::process::exit(1);
    }
    let db_path = args[1].clone();

    let mut iters: usize = 3;
    let mut limit: usize = 20;
    let mut backend = Backend::Lazy;
    let mut csv_dir: Option<String> = None;
    let mut params_dir: Option<String> = None;
    let mut queries_dir = PathBuf::from("bench/ldbc-queries");
    let mut ic_arg: String = "2".to_string();
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--iters" => {
                iters = args[i + 1].parse().expect("invalid iters");
                i += 2;
            }
            "--limit" => {
                limit = args[i + 1].parse().expect("invalid limit");
                i += 2;
            }
            "--backend" => {
                backend = Backend::parse(&args[i + 1]).expect("backend must be memory|lazy|disk");
                i += 2;
            }
            "--csv-dir" => {
                csv_dir = Some(args[i + 1].clone());
                i += 2;
            }
            "--params-dir" => {
                params_dir = Some(args[i + 1].clone());
                i += 2;
            }
            "--queries-dir" => {
                queries_dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--ic" => {
                ic_arg = args[i + 1].clone();
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

    let queries = load_queries(&queries_dir);
    eprintln!(
        "Loaded {} IC definitions from {} ({} implemented, {} blocked)",
        queries.len(),
        queries_dir.display(),
        queries
            .iter()
            .filter(|q| q.status == Status::Implemented)
            .count(),
        queries
            .iter()
            .filter(|q| q.status == Status::Blocked)
            .count(),
    );

    // --ic blocked: print inventory and exit.
    if ic_arg == "blocked" {
        eprintln!("\n=== Blocked ICs ===");
        for q in queries.iter().filter(|q| q.status == Status::Blocked) {
            eprintln!("  IC{:<2} {}", q.id, q.name);
            if let Some(reason) = &q.blocked_reason {
                for line in reason.lines().filter(|l| !l.trim().is_empty()) {
                    eprintln!("        {}", line.trim());
                }
            }
            if let Some(feats) = &q.required_features {
                eprintln!("        required: {}", feats.join(", "));
            }
        }
        return;
    }

    // Resolve which ICs to run.
    let target_ids: Vec<u32> = if ic_arg == "all" {
        queries
            .iter()
            .filter(|q| q.status == Status::Implemented)
            .map(|q| q.id)
            .collect()
    } else {
        ic_arg
            .split(',')
            .map(|s| {
                s.trim()
                    .parse::<u32>()
                    .expect("--ic expects N or N,M,K or all|blocked")
            })
            .collect()
    };

    if target_ids.is_empty() {
        eprintln!("no implemented ICs to run");
        return;
    }

    // Resolve params dir (default: alongside the .gdb dataset).
    let params_dir = params_dir.map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from("bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1")
    });

    // Set up RSS tracking.
    let mut sys = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::new().with_memory()),
    );
    let rss_baseline = rss_mb(&mut sys);
    eprintln!("RSS baseline: {rss_baseline:.1} MiB");

    // Open the chosen backend once and run all selected ICs against it.
    match backend {
        Backend::Memory => {
            let dir = csv_dir.expect("--csv-dir required for memory backend");
            eprintln!("Loading LDBC CSV from {dir} into Graph (in-memory)...");
            let t0 = Instant::now();
            let g = csv_loader::load_from_ldbc_csv_dir(Path::new(&dir))
                .expect("load_from_ldbc_csv_dir failed");
            eprintln!(
                "  loaded {} nodes / {} edges in {:.2}s",
                g.node_count(),
                g.edge_count(),
                t0.elapsed().as_secs_f64()
            );
            log_rss_after_load(&mut sys, rss_baseline);
            run_targets(
                &g,
                backend,
                &queries,
                &target_ids,
                &params_dir,
                iters,
                limit,
                &mut sys,
                rss_baseline,
            );
        }
        Backend::Lazy => {
            eprintln!("Loading {db_path} (LazyGraphStore)...");
            let t0 = Instant::now();
            let store = LazyGraphStore::open(Path::new(&db_path)).expect("open .gdb");
            eprintln!(
                "  loaded {} nodes / {} edges in {:.2}s",
                store.node_count(),
                store.edge_count(),
                t0.elapsed().as_secs_f64()
            );
            log_rss_after_load(&mut sys, rss_baseline);
            run_targets(
                &store,
                backend,
                &queries,
                &target_ids,
                &params_dir,
                iters,
                limit,
                &mut sys,
                rss_baseline,
            );
        }
        Backend::Disk => {
            eprintln!("Loading {db_path} (DiskGraphStore)...");
            let t0 = Instant::now();
            let store = DiskGraphStore::open(Path::new(&db_path)).expect("open .gdb");
            let n_nodes = store.nodes().len();
            let n_edges = store.edges_directed().len() + store.edges_undirected().len();
            eprintln!(
                "  loaded {n_nodes} nodes / {n_edges} edges in {:.2}s",
                t0.elapsed().as_secs_f64()
            );
            log_rss_after_load(&mut sys, rss_baseline);
            run_targets(
                &store,
                backend,
                &queries,
                &target_ids,
                &params_dir,
                iters,
                limit,
                &mut sys,
                rss_baseline,
            );
        }
    }
}

fn log_rss_after_load(sys: &mut System, rss_baseline: f64) {
    let cur = rss_mb(sys);
    eprintln!(
        "  RSS after open: {cur:.1} MiB (+{:.1} MiB)",
        cur - rss_baseline
    );
}

#[allow(clippy::too_many_arguments)]
fn run_targets<G: GraphAccess>(
    graph: &G,
    backend: Backend,
    queries: &[IcQuery],
    target_ids: &[u32],
    params_dir: &Path,
    iters: usize,
    limit: usize,
    sys: &mut System,
    rss_baseline: f64,
) {
    // CSV header for stdout — same shape regardless of which IC.
    println!("query;backend;ic;param_idx;iter;result_count;elapsed_ns");

    let mut peak_rss = rss_baseline;

    for &id in target_ids {
        let q = match queries.iter().find(|q| q.id == id) {
            Some(q) => q,
            None => {
                eprintln!("\n  IC{id}: no definition found in queries dir; skipping");
                continue;
            }
        };
        if q.status != Status::Implemented {
            eprintln!("\n  IC{id} ({}): blocked, skipping", q.name);
            if let Some(reason) = &q.blocked_reason {
                eprintln!("    reason: {}", reason.trim());
            }
            continue;
        }
        run_one_ic(
            graph,
            backend,
            q,
            params_dir,
            iters,
            limit,
            sys,
            &mut peak_rss,
        );
    }

    eprintln!(
        "\nPeak RSS during query loop: {peak_rss:.1} MiB (+{:.1} MiB over baseline)",
        peak_rss - rss_baseline
    );
}

#[allow(clippy::too_many_arguments)]
fn run_one_ic<G: GraphAccess>(
    graph: &G,
    backend: Backend,
    q: &IcQuery,
    params_dir: &Path,
    iters: usize,
    limit: usize,
    sys: &mut System,
    peak_rss: &mut f64,
) {
    let params_file_name = q
        .params_file
        .as_ref()
        .expect("implemented IC must have params_file");
    let params_file = params_dir.join(params_file_name);
    let template = q.query.as_ref().expect("implemented IC must have query");
    let (header, rows) = load_params(&params_file);

    eprintln!(
        "\n=== IC{}: {} (backend={}) ===",
        q.id,
        q.name,
        backend.label()
    );
    eprintln!(
        "Params: {} ({} rows, columns: {});  {iters} iters/param;  limit={limit}",
        params_file_name,
        rows.len(),
        header.join(", "),
    );

    let rt = Runtime::new(graph);

    for (param_idx, row) in rows.iter().enumerate() {
        let query_text = substitute(template, &header, row);
        let parsed = match compile_query_unchecked(&query_text) {
            Ok(parsed) => parsed,
            Err(e) => {
                eprintln!(
                    "  PARSE ERROR on row {param_idx} (params: {}): {e}",
                    row.join("|")
                );
                continue;
            }
        };

        let mut samples: Vec<Duration> = Vec::with_capacity(iters);
        let mut last_count = 0usize;
        for n in 0..iters {
            let start = Instant::now();
            let result = rt.run_query(&parsed, limit);
            let elapsed = start.elapsed();
            samples.push(elapsed);
            last_count = result.row_count();
            println!(
                "IC{};{};{};{param_idx};{n};{};{}",
                q.id,
                backend.label(),
                row.join("|"),
                last_count,
                elapsed.as_nanos()
            );
        }
        let cur_rss = rss_mb(sys);
        if cur_rss > *peak_rss {
            *peak_rss = cur_rss;
        }
        report(q.id, param_idx, row, &samples, last_count);
    }
}

fn report(ic: u32, param_idx: usize, row: &[String], samples: &[Duration], count: usize) {
    let mut sorted: Vec<u128> = samples.iter().map(|d| d.as_nanos()).collect();
    sorted.sort_unstable();
    let n = sorted.len();
    if n == 0 {
        return;
    }
    let row_summary = row.join("|");
    if n == 1 {
        eprintln!(
            "  IC{ic} row#{param_idx:<3} ({row_summary}) count={count:<3} \
             wall={:>8.2}ms  (n=1, --iters >=3 recommended for stable median)",
            sorted[0] as f64 / 1e6,
        );
        return;
    }
    let min = sorted[0];
    let max = sorted[n - 1];
    let median = sorted[n / 2];
    let mean = sorted.iter().sum::<u128>() / n as u128;
    eprintln!(
        "  IC{ic} row#{param_idx:<3} ({row_summary}) count={count:<3} \
         min={:>8.2}ms  med={:>8.2}ms  mean={:>8.2}ms  max={:>8.2}ms  (n={n})",
        min as f64 / 1e6,
        median as f64 / 1e6,
        mean as f64 / 1e6,
        max as f64 / 1e6,
    );
}
