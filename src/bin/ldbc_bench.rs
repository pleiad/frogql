//! LDBC SNB Interactive workload benchmark runner.
//!
//! Section 6 of the LDBC SNB v1 paper (arXiv:2001.02299) defines the
//! Interactive Complex (IC) reads. This binary runs the subset that
//! gqlite supports today, and reports both wall time and RSS per
//! backend (Memory / Lazy / Disk).
//!
//! ## Query catalog
//!
//! Thin `bench/ldbc-queries/ic<n>.toml` (title, spec URL, keys, `query`). Import
//! and bench flags: `bench/LDBC_BENCHMARK.md`.
//!
//! - **implemented**: `query` + `params_file`; `{col}` ↔ param header.
//! - **blocked**: `blocked_reason`, `required_features`, optional `query` /
//!   params (not executed). `--ic blocked` prints inventory.
//!
//! ## Usage
//!
//!     ldbc_bench <db.gdb> [--ic <n>|all|blocked]
//!                         [--backend memory|lazy|disk]
//!                         [--params-dir <dir>]
//!                         [--queries-dir <dir>]
//!                         [--iters N] [--warmup N]
//!                         [--csv-dir <dir>]      # required for --backend memory
//!
//! Row caps live in each `bench/ldbc-queries/ic<n>.toml`'s query (the
//! spec-faithful `LIMIT N`); there is no `--limit` flag to override.
//!
//! Three GraphAccess backends:
//!   - **memory** — `MemoryGraphStore` from `csv_loader::load_from_ldbc_csv_dir`.
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
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::store::disk::DiskGraphStore;
use gqlrust::store::lazy::LazyGraphStore;

// ----------------------------------------------------------- Result shape ---

/// One-letter type code per column position.
fn shape_of_value(v: &Value) -> &'static str {
    match v {
        Value::Null => "n",
        Value::Int(_) => "i",
        Value::Str(_) => "s",
        Value::Bool(_) => "b",
        Value::Float(_) => "f",
        Value::List(_) => "l",
        Value::Record(_) => "r",
        Value::Node(_) => "N",
        Value::Edge(_) => "E",
        Value::Path(_) => "P",
    }
}

/// Per-column type-set across all rows, joined by `/` within a column
/// and `,` between columns. Example: `i,s,s,i,n/s,i` means columns 0
/// 1, 2, 3, 5 carry exactly one type each (Int, Str, Str, Int, Int)
/// and column 4 carries both Null and Str across the row set.
///
/// This accommodates a sometimes-null column without making the shape
/// dependent on which N rows a system chose under LIMIT-without-ORDER-BY:
/// gqlite picking 20 rows that include null `c.content` and graphqlite
/// picking 20 rows that don't would still both expose Str at that
/// column; only one would expose Null. The per-column-set form lets
/// the comparison flag genuine type disagreement (e.g. column 1 being
/// Int on one system and Str on another) without false-positiving
/// nullability variation.
fn shape_of_result(result: &QueryResult) -> String {
    use std::collections::BTreeSet;
    let rows = match result {
        QueryResult::Projected(rs) => rs,
        QueryResult::Raw(_) => return "raw".to_string(),
    };
    if rows.is_empty() {
        return "empty".to_string();
    }
    let n_cols = rows[0].len();
    let mut cols: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); n_cols];
    for row in rows {
        for (i, v) in row.iter().take(n_cols).enumerate() {
            cols[i].insert(shape_of_value(v));
        }
    }
    cols.iter()
        .map(|set| set.iter().copied().collect::<Vec<_>>().join("/"))
        .collect::<Vec<_>>()
        .join(",")
}

// ----------------------------------------------------- Row-content hashing ---

// Canonical per-cell encoding for the cross-system row-equivalence
// oracle. Mirror of `canonicalize_cell` in
// `bench/cross-system/_lib/row_hash.py` — see that file's docstring
// for the format choice and rationale; the two implementations must
// stay byte-equivalent. Cells joined with "\x1f", rows with "\n",
// sha256 over the concatenation. Strings are rstripped to normalize
// loader-formatting drift between systems.
fn canonicalize_cell(v: &Value) -> String {
    match v {
        Value::Null => "\x00".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.trim_end().to_string(),
        other => format!("{other:?}"),
    }
}

fn canonicalize_row(row: &[Value]) -> String {
    row.iter()
        .map(canonicalize_cell)
        .collect::<Vec<_>>()
        .join("\x1f")
}

/// `(canonical_blob, sha256_hex)` for the projected rows. Returns
/// `("", empty_hash)` for non-projected results so the oracle line
/// is always emitted (the runner has nothing useful to compare on
/// `Raw` results, but the SHAPE/HASH symmetry stays).
fn canonicalize_and_hash(rows: &[Vec<Value>]) -> (String, String) {
    use sha2::{Digest, Sha256};
    let blob = rows
        .iter()
        .map(|row| canonicalize_row(row))
        .collect::<Vec<_>>()
        .join("\n");
    let hash = format!("{:x}", Sha256::digest(blob.as_bytes()));
    (blob, hash)
}

/// Append a JSONL envelope for one params-row's iter-0 result to
/// `path`. One line per call. `rows_blob` is the canonical text
/// (a copy in case the user wants to spot-check); `hash` is the
/// hex sha256 of that text.
fn append_rows_jsonl(
    path: &Path,
    ic: u32,
    params: &str,
    row_idx: usize,
    count: usize,
    hash: &str,
    rows_blob: &str,
) {
    use serde_json::json;
    use std::fs::OpenOptions;
    use std::io::Write;
    let envelope = json!({
        "ic": ic,
        "params": params,
        "row": row_idx,
        "count": count,
        "hash": hash,
        // `rows` is the canonicalized text — same input the hash
        // is computed over. Splitting by '\n' restores the per-row
        // "\x1f"-separated cells. Diff this between systems on
        // hash mismatch.
        "rows": rows_blob,
    });
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", envelope);
    } else {
        eprintln!("  WARN failed to append rows-jsonl to {}", path.display());
    }
}

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
/// two `status` variants — `validate()` enforces the constraint.
///
/// `deny_unknown_fields` so a typo like `parms_file = "..."` produces
/// an immediate parse error naming the unknown key, instead of
/// silently leaving `params_file = None` and surfacing later as
/// "missing `params_file`".
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IcQuery {
    id: u32,
    name: String,
    status: Status,

    // implemented-only
    params_file: Option<String>,
    /// Names of param columns; documentation aid (the runtime reads
    /// the param file's own header). When `param_types` is set,
    /// `param_columns` length must match.
    param_columns: Option<Vec<String>>,
    /// Per-column type tag. Currently `"int"` (raw substitution) or
    /// `"string"` (substituted as `'<value>'`, with the value rejected
    /// if it contains `'` since gqlite's lexer has no escape syntax).
    /// Optional; defaults to all-`int` if absent. Length must match
    /// `param_columns` when both are set.
    param_types: Option<Vec<String>>,
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

impl IcQuery {
    /// Validate that fields match the declared `status`. Called at
    /// load time so a malformed TOML fails before any backend is
    /// opened or any other IC runs.
    fn validate(&self, source: &Path) -> Result<(), String> {
        match self.status {
            Status::Implemented => {
                if self.params_file.is_none() {
                    return Err(format!(
                        "{}: status='implemented' but missing `params_file`",
                        source.display()
                    ));
                }
                if self.query.is_none() {
                    return Err(format!(
                        "{}: status='implemented' but missing `query`",
                        source.display()
                    ));
                }
                if let (Some(cols), Some(types)) = (&self.param_columns, &self.param_types) {
                    if cols.len() != types.len() {
                        return Err(format!(
                            "{}: param_columns has {} entries but param_types has {}",
                            source.display(),
                            cols.len(),
                            types.len()
                        ));
                    }
                }
                if let Some(types) = &self.param_types {
                    for t in types {
                        if t != "int" && t != "string" {
                            return Err(format!(
                                "{}: param_types entry {t:?} is not 'int' or 'string'",
                                source.display()
                            ));
                        }
                    }
                }
            }
            Status::Blocked => {
                if self.blocked_reason.is_none() {
                    return Err(format!(
                        "{}: status='blocked' but missing `blocked_reason`",
                        source.display()
                    ));
                }
            }
        }
        Ok(())
    }
}

fn load_queries(dir: &Path) -> Vec<IcQuery> {
    let mut out: Vec<(IcQuery, PathBuf)> = Vec::new();
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
        if let Err(e) = q.validate(&p) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        out.push((q, p));
    }
    out.sort_by_key(|(q, _)| q.id);

    // Detect duplicate IDs after sort. Two TOMLs with `id = 2` would
    // otherwise silently load both with `find()` returning only the
    // first — a footgun if someone copies a TOML to bootstrap a new IC
    // and forgets to renumber.
    for w in out.windows(2) {
        if w[0].0.id == w[1].0.id {
            eprintln!(
                "duplicate IC id {} in {} and {}",
                w[0].0.id,
                w[0].1.display(),
                w[1].1.display()
            );
            std::process::exit(1);
        }
    }

    out.into_iter().map(|(q, _)| q).collect()
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
    let header_line = match lines.next() {
        Some(l) => l,
        None => {
            eprintln!("params file {} is empty (no header line)", path.display());
            std::process::exit(1);
        }
    };
    let header: Vec<String> = header_line.split('|').map(str::to_string).collect();
    let rows: Vec<Vec<String>> = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('|').map(str::to_string).collect())
        .collect();
    (header, rows)
}

/// Replace each `{colName}` in `template` with the corresponding
/// value from `row`. `param_types` (default all-`int`) controls
/// formatting per column:
///   - `"int"` — raw substitution (`933` becomes literal `933`)
///   - `"string"` — wrapped in single quotes (`Belize` becomes
///     `'Belize'`). Rejects values containing `'` since gqlite's
///     lexer has no escape syntax for embedded quotes.
///
/// Errors if any `{...}` placeholder remains unsubstituted after the
/// pass — that means the query template references a column not in
/// the param file's header (typo or mismatch between TOML and the
/// LDBC param file). Catching this here gives a clearer error than
/// letting the gqlite parser fail on a stray brace.
fn substitute(
    template: &str,
    header: &[String],
    row: &[String],
    param_types: &[&str],
) -> Result<String, String> {
    let mut out = template.to_string();
    for (i, (h, v)) in header.iter().zip(row.iter()).enumerate() {
        let ty = param_types.get(i).copied().unwrap_or("int");
        let formatted = match ty {
            "int" => v.clone(),
            "string" => {
                if v.contains('\'') {
                    return Err(format!(
                        "param value for column {h:?} contains a single quote ({v:?}); \
                         gqlite's lexer has no escape syntax for embedded quotes"
                    ));
                }
                format!("'{v}'")
            }
            other => return Err(format!("unknown param type {other:?} for column {h:?}")),
        };
        out = out.replace(&format!("{{{h}}}"), &formatted);
    }
    // Detect unsubstituted `{name}` placeholders. Looks for `{` …
    // `}` pairs where the contents are a valid identifier (avoids
    // false positives on the `(p: Person {id: ...})` descriptor
    // shorthand, which has no leftover braces post-substitute since
    // the inner placeholder *was* replaced).
    if let Some(stray) = find_unsubstituted_placeholder(&out) {
        return Err(format!(
            "unsubstituted placeholder {{{stray}}} remains after substitution; \
             check that the query template's column names match the param file's header \
             ({})",
            header.join(", ")
        ));
    }
    Ok(out)
}

/// Look for a `{IDENT}` token in `s` — used to spot unsubstituted
/// placeholders. Returns the inner identifier, or None if the only
/// braces are part of legitimate gqlite syntax (descriptor shorthand
/// `{k: v}`, repetitions `{n,m}`).
fn find_unsubstituted_placeholder(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            // Find matching `}` on the same span; only consider the
            // contents an "unsubstituted placeholder" if it's a bare
            // identifier (letters / digits / underscore, starting with
            // a letter or underscore). Things like `{id: 933}` or
            // `{1,3}` won't match.
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'}' {
                end += 1;
            }
            if end < bytes.len() {
                let inner = &s[start..end];
                if !inner.is_empty()
                    && inner
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    && !inner.bytes().next().unwrap().is_ascii_digit()
                {
                    return Some(inner);
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    None
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
         \t[--iters N] [--warmup N] [--csv-dir <dir>]\n\
         \n\
         Defaults: --ic 2  --backend lazy  --iters 3  --warmup 0\n\
         Row caps are spec-faithful and live in each ic<n>.toml's query\n\
         (e.g. IC2 has `LIMIT 20`); no CLI override.\n\
         --warmup N runs N extra iters per row before the timed ones; their\n\
         measurements are discarded. Whether this helps depends on OS / RAM /\n\
         storage: on a warm machine with free RAM >= dataset size it usually\n\
         doesn't (OS page cache is cross-row, populated by row#0). On cold or\n\
         cache-constrained machines it can absorb a cold-iter spike. Default 0;\n\
         raise it if the per-row stderr summary shows iter 0 consistently\n\
         dominating min/median.\n\
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
    // Bare `--help` / `-h` as the first arg (where the positional
    // .gdb is otherwise expected) prints usage and exits without
    // trying to open the file.
    if args[1] == "-h" || args[1] == "--help" {
        print_usage(&args[0]);
        return;
    }
    let db_path = args[1].clone();

    let mut iters: usize = 3;
    let mut warmup: usize = 0;
    let mut backend = Backend::Lazy;
    let mut csv_dir: Option<String> = None;
    let mut params_dir: Option<String> = None;
    let mut queries_dir = PathBuf::from("bench/ldbc-queries");
    let mut ic_arg: String = "2".to_string();
    let mut i = 2;
    // Helper to read the next arg as the value for the current flag.
    // Avoids panicking with index-out-of-bounds when a flag is the
    // last token (e.g. `ldbc_bench db.gdb --iters`).
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
                    eprintln!("invalid --iters value: {e}");
                    std::process::exit(1);
                });
                i += 2;
            }
            "--warmup" => {
                warmup = need_value(i, "--warmup").parse().unwrap_or_else(|e| {
                    eprintln!("invalid --warmup value: {e}");
                    std::process::exit(1);
                });
                i += 2;
            }
            "--backend" => {
                let v = need_value(i, "--backend");
                backend = Backend::parse(v).unwrap_or_else(|| {
                    eprintln!("--backend must be memory|lazy|disk, got {v:?}");
                    std::process::exit(1);
                });
                i += 2;
            }
            "--csv-dir" => {
                csv_dir = Some(need_value(i, "--csv-dir").to_string());
                i += 2;
            }
            "--params-dir" => {
                params_dir = Some(need_value(i, "--params-dir").to_string());
                i += 2;
            }
            "--queries-dir" => {
                queries_dir = PathBuf::from(need_value(i, "--queries-dir"));
                i += 2;
            }
            "--ic" => {
                ic_arg = need_value(i, "--ic").to_string();
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
            if let Some(query) = &q.query {
                eprintln!("        query:");
                for line in query.trim_end().lines() {
                    eprintln!("        | {line}");
                }
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
        match ic_arg
            .split(',')
            .map(|s| s.trim().parse::<u32>())
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(ids) => ids,
            Err(e) => {
                eprintln!(
                    "invalid --ic value {ic_arg:?}: {e}\n\
                     expected: N, N,M,K, all, or blocked"
                );
                std::process::exit(1);
            }
        }
    };

    if target_ids.is_empty() {
        eprintln!("no implemented ICs to run");
        return;
    }

    // Reject --iters 0 explicitly: the bench would run warmup iters
    // (if any) but record zero measured iters, producing no CSV rows
    // and no per-IC summary line. Misleading — users would see a
    // "✓ done" with nothing to show. Force the user to pick at
    // least one measured iter.
    if iters == 0 {
        eprintln!("--iters must be >= 1 (got 0; nothing would be measured)");
        std::process::exit(1);
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
            let dir = csv_dir.unwrap_or_else(|| {
                eprintln!("--csv-dir is required when --backend memory is used");
                std::process::exit(1);
            });
            eprintln!("Loading LDBC CSV from {dir} into MemoryGraphStore (in-memory)...");
            let t0 = Instant::now();
            let g = csv_loader::load_from_ldbc_csv_dir(Path::new(&dir)).unwrap_or_else(|e| {
                eprintln!("failed to load LDBC CSV from {dir}: {e}");
                std::process::exit(1);
            });
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
                warmup,
                &mut sys,
                rss_baseline,
            );
        }
        Backend::Lazy => {
            eprintln!("Loading {db_path} (LazyGraphStore)...");
            let t0 = Instant::now();
            let store = LazyGraphStore::open(Path::new(&db_path)).unwrap_or_else(|e| {
                eprintln!("failed to open .gdb {db_path}: {e}");
                std::process::exit(1);
            });
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
                warmup,
                &mut sys,
                rss_baseline,
            );
        }
        Backend::Disk => {
            eprintln!("Loading {db_path} (DiskGraphStore)...");
            let t0 = Instant::now();
            let store = DiskGraphStore::open(Path::new(&db_path)).unwrap_or_else(|e| {
                eprintln!("failed to open .gdb {db_path}: {e}");
                std::process::exit(1);
            });
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
                warmup,
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
    warmup: usize,
    sys: &mut System,
    rss_baseline: f64,
) {
    // CSV header for stdout — same shape regardless of which IC.
    // `params` carries the substitution-param values for this row
    // joined by `|`; `row` is the 0-indexed line of the LDBC param
    // file (matches the stderr summary's `row#N` label). Column
    // meanings documented in bench/LDBC_BENCHMARK.md.
    println!("query;backend;params;row;iter;result_count;elapsed_ns");

    let mut peak_rss = rss_baseline;
    let mut summaries: Vec<IcSummary> = Vec::new();

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
        summaries.push(run_one_ic(
            graph,
            backend,
            q,
            params_dir,
            iters,
            warmup,
            sys,
            &mut peak_rss,
        ));
    }

    let any_ic_had_no_rows = summaries
        .iter()
        .any(|s| s.row_medians_ns.is_empty() && s.failed_rows > 0);
    if summaries.is_empty() {
        // None of the requested ICs were available to run (either
        // not in the catalog, or all blocked). The bench produced no
        // measurements and "✓ done" would falsely imply success.
        eprintln!(
            "\n⚠ done — 0 IC(s) ran (none of the requested ICs were implemented \
             in the catalog). Use --ic blocked to see the inventory."
        );
    } else if any_ic_had_no_rows {
        eprintln!(
            "\n⚠ done with errors — {} IC(s) processed, but at least one had every \
             row fail at substitute or compile (no measurements taken).",
            summaries.len()
        );
    } else {
        eprintln!("\n✓ done — {} IC(s) ran to completion", summaries.len());
    }
    for s in &summaries {
        if s.row_medians_ns.is_empty() {
            // Either no rows in the params file, or every row failed.
            // Surface the failure count so it's not invisible.
            if s.failed_rows > 0 {
                eprintln!(
                    "  IC{}: 0 of {} rows produced measurements (all failed at \
                     substitute or compile); see SUBSTITUTE/PARSE ERROR lines above.",
                    s.ic_id, s.failed_rows,
                );
            }
            continue;
        }
        let mut sorted = s.row_medians_ns.clone();
        sorted.sort_unstable();
        let n = sorted.len();
        let median = median_of(&sorted);
        let min = sorted[0];
        let max = sorted[n - 1];
        let suffix = if s.failed_rows > 0 {
            format!("  ({} additional row(s) failed)", s.failed_rows)
        } else {
            String::new()
        };
        eprintln!(
            "  IC{}: {} rows × {} iter(s) = {} runs; \
             across-row median {:.2}ms (range {:.2}-{:.2}ms){}",
            s.ic_id,
            n,
            s.iters,
            n * s.iters,
            median as f64 / 1e6,
            min as f64 / 1e6,
            max as f64 / 1e6,
            suffix,
        );
    }
    eprintln!(
        "Peak RSS during query loop: {peak_rss:.1} MiB (+{:.1} MiB over baseline)",
        peak_rss - rss_baseline
    );
}

/// Per-IC stats returned from `run_one_ic` so `run_targets` can
/// build a final cross-IC summary.
struct IcSummary {
    ic_id: u32,
    iters: usize,
    /// One median (in nanoseconds) per param row that ran cleanly.
    row_medians_ns: Vec<u128>,
    /// Rows that failed at substitute or compile time. Counted so
    /// the final "done" summary can call out "ran 0 of N rows" vs
    /// silently showing nothing.
    failed_rows: usize,
}

#[allow(clippy::too_many_arguments)]
fn run_one_ic<G: GraphAccess>(
    graph: &G,
    backend: Backend,
    q: &IcQuery,
    params_dir: &Path,
    iters: usize,
    warmup: usize,
    sys: &mut System,
    peak_rss: &mut f64,
) -> IcSummary {
    // `validate()` at load time guaranteed these are present.
    let params_file_name = q.params_file.as_ref().unwrap();
    let params_file = params_dir.join(params_file_name);
    let template = q.query.as_ref().unwrap();
    let (header, rows) = load_params(&params_file);

    // Per-column types (default all-int when absent or shorter than
    // the param file's header — substitute() falls back to "int").
    let param_types: Vec<&str> = q
        .param_types
        .as_ref()
        .map(|ts| ts.iter().map(String::as_str).collect())
        .unwrap_or_default();

    eprintln!(
        "\n=== IC{}: {} (backend={}) ===",
        q.id,
        q.name,
        backend.label()
    );
    eprintln!(
        "Params: {} ({} rows, columns: {});  {warmup}+{iters} iters/param (warmup+measured)",
        params_file_name,
        rows.len(),
        header.join(", "),
    );

    let rt = Runtime::new(graph);
    // Pre-build the LTJ index so it doesn't poison the first row's iter#0
    // measurement. Cost is reported separately and excluded from the per-
    // row stats.
    let t_idx = std::time::Instant::now();
    rt.warm_triple_index();
    eprintln!(
        "  LTJ TripleIndex built in {:.2}s (excluded from per-row timings)",
        t_idx.elapsed().as_secs_f64()
    );
    let mut row_medians_ns: Vec<u128> = Vec::with_capacity(rows.len());
    let mut failed_rows = 0usize;

    for (row_idx, row) in rows.iter().enumerate() {
        let query_text = match substitute(template, &header, row, &param_types) {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "  SUBSTITUTE ERROR on row {row_idx} (params: {}): {e}",
                    row.join("|")
                );
                failed_rows += 1;
                continue;
            }
        };
        let parsed = match compile_query_unchecked(&query_text) {
            Ok(parsed) => parsed,
            Err(e) => {
                eprintln!(
                    "  PARSE ERROR on row {row_idx} (params: {}): {e}",
                    row.join("|")
                );
                failed_rows += 1;
                continue;
            }
        };

        let mut samples: Vec<Duration> = Vec::with_capacity(iters);
        let mut last_count = 0usize;
        // Warmup iters: run silently, no CSV emit, no measurement.
        // Whether this helps depends on the run environment: on a
        // warm machine where the OS page cache holds the .gdb across
        // rows, iter 0 of a row isn't paying any special cold cost
        // and warmup is wasted. On cold/cache-constrained machines
        // (or huge SFs where the cache can't hold the working set),
        // it can absorb a real first-iter penalty. Default 0; raise
        // via --warmup if the per-row summary shows iter 0 dominant.
        for _ in 0..warmup {
            let _ = rt.run_query(&parsed, 0);
        }
        // Shape work runs after the iter loop, never between timed iters.
        let mut iter0_result: Option<QueryResult> = None;
        for n in 0..iters {
            let start = Instant::now();
            let result = rt.run_query(&parsed, 0);
            let elapsed = start.elapsed();
            samples.push(elapsed);
            last_count = result.row_count();
            println!(
                "IC{};{};{};{row_idx};{n};{};{}",
                q.id,
                backend.label(),
                row.join("|"),
                last_count,
                elapsed.as_nanos()
            );
            if n == 0 {
                iter0_result = Some(result);
            }
        }
        // Pin both shape and count to iter 0; if a non-determinism bug
        // ever appears, both fields move together rather than just one.
        let (actual_shape, actual_count) = match &iter0_result {
            Some(r) => (shape_of_result(r), r.row_count()),
            None => (String::new(), 0),
        };
        // Row-content hash for the cross-system row-equivalence oracle.
        // Canonicalize iter-0 rows, sha256, log to stderr; if the
        // GQLITE_BENCH_ROWS_JSONL env var points at a path, also dump
        // the per-row envelope there so compare_results.py can diff
        // mismatches. Mirror the Python runners' logic in
        // `graphqlite/run.py` and `kuzu/run.py` so the three runners
        // produce byte-identical canonicalized blobs.
        //
        // The single per-row stderr line below carries everything a
        // human or compare_results.py wants: count, shape (informational
        // — quick visual smell-check), and hash (the strong cross-system
        // oracle). We dropped the separate SHAPE/HASH lines + the
        // `expected_shape` toml contract because the hash subsumes
        // the shape check — any column-count or per-cell-type drift
        // changes the hash. The shape printout stays as human-readable
        // context; nothing parses it as a contract anymore.
        let projected_rows: &[Vec<Value>] = match &iter0_result {
            Some(QueryResult::Projected(rs)) => rs.as_slice(),
            _ => &[],
        };
        let (rows_blob, row_hash) = canonicalize_and_hash(projected_rows);
        eprintln!("  ROW row={row_idx} count={actual_count} shape={actual_shape} hash={row_hash}");
        if let Ok(jsonl_path) = env::var("GQLITE_BENCH_ROWS_JSONL") {
            append_rows_jsonl(
                Path::new(&jsonl_path),
                q.id,
                &row.join("|"),
                row_idx,
                actual_count,
                &row_hash,
                &rows_blob,
            );
        }
        let cur_rss = rss_mb(sys);
        if cur_rss > *peak_rss {
            *peak_rss = cur_rss;
        }
        if let Some(med) = report(q.id, row_idx, row, &samples, last_count) {
            row_medians_ns.push(med);
        }
    }

    IcSummary {
        ic_id: q.id,
        iters,
        row_medians_ns,
        failed_rows,
    }
}

/// Median of a sorted slice. For odd N, the middle element. For
/// even N, the integer average of the two middle elements (matches
/// the standard statistical convention; previous version returned
/// the upper of the two midpoints, which is neither median nor a
/// labeled percentile).
fn median_of(sorted: &[u128]) -> u128 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2
    }
}

/// Print a per-row stderr summary line and return the median sample
/// in nanoseconds (so `run_one_ic` can build a cross-row aggregate).
fn report(
    ic: u32,
    row_idx: usize,
    row: &[String],
    samples: &[Duration],
    count: usize,
) -> Option<u128> {
    let mut sorted: Vec<u128> = samples.iter().map(|d| d.as_nanos()).collect();
    sorted.sort_unstable();
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    let row_summary = row.join("|");
    if n == 1 {
        eprintln!(
            "  IC{ic} row#{row_idx:<3} ({row_summary}) count={count:<3} \
             wall={:>8.2}ms  (n=1, --iters >=3 recommended for stable median)",
            sorted[0] as f64 / 1e6,
        );
        return Some(sorted[0]);
    }
    let min = sorted[0];
    let max = sorted[n - 1];
    let median = median_of(&sorted);
    let mean = sorted.iter().sum::<u128>() / n as u128;
    eprintln!(
        "  IC{ic} row#{row_idx:<3} ({row_summary}) count={count:<3} \
         min={:>8.2}ms  med={:>8.2}ms  mean={:>8.2}ms  max={:>8.2}ms  (n={n})",
        min as f64 / 1e6,
        median as f64 / 1e6,
        mean as f64 / 1e6,
        max as f64 / 1e6,
    );
    Some(median)
}

// ----------------------------------------------------------------- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> String {
        v.to_string()
    }

    // --- median_of ---

    #[test]
    fn median_odd_n() {
        assert_eq!(median_of(&[1, 2, 3]), 2);
        assert_eq!(median_of(&[1, 5, 9, 11, 17]), 9);
    }

    #[test]
    fn median_even_n_averages_midpoints() {
        // Was the bug A3 caught: previously returned sorted[n/2] = 4
        // (the upper midpoint) for [1,2,3,4]. Now returns
        // (sorted[n/2-1] + sorted[n/2]) / 2 = (2+3)/2 = 2 (integer
        // division — the true median is 2.5; for u128 nanosecond
        // timings the 1 ns rounding is negligible).
        assert_eq!(median_of(&[1, 2, 3, 4]), 2);
        assert_eq!(median_of(&[10, 20, 30, 40, 50, 60]), 35);
    }

    #[test]
    fn median_single_value() {
        assert_eq!(median_of(&[42]), 42);
    }

    // --- substitute ---

    #[test]
    fn substitute_int_default() {
        let header = vec![s("personId"), s("maxDate")];
        let row = vec![s("933"), s("1354060800000")];
        let out = substitute(
            "WHERE p.id = {personId} AND c.creationDate <= {maxDate}",
            &header,
            &row,
            &[],
        )
        .unwrap();
        assert_eq!(out, "WHERE p.id = 933 AND c.creationDate <= 1354060800000");
    }

    #[test]
    fn substitute_string_wraps_quotes() {
        let header = vec![s("firstName")];
        let row = vec![s("Mahinda")];
        let out = substitute(
            "WHERE p.firstName = {firstName}",
            &header,
            &row,
            &["string"],
        )
        .unwrap();
        assert_eq!(out, "WHERE p.firstName = 'Mahinda'");
    }

    #[test]
    fn substitute_string_rejects_embedded_quote() {
        let header = vec![s("name")];
        let row = vec![s("d'Ivoire")];
        let err = substitute("WHERE x.name = {name}", &header, &row, &["string"]).unwrap_err();
        assert!(err.contains("single quote"), "error was: {err}");
    }

    #[test]
    fn substitute_unknown_type_errors() {
        let header = vec![s("x")];
        let row = vec![s("v")];
        let err = substitute("{x}", &header, &row, &["weird"]).unwrap_err();
        assert!(err.contains("unknown param type"), "error was: {err}");
    }

    #[test]
    fn substitute_unmatched_placeholder_errors() {
        // Template references {countryName} but param file only has personId.
        // Was bug C3: previously returned the template with `{countryName}`
        // intact, then the gqlite parser failed with a confusing error.
        let header = vec![s("personId")];
        let row = vec![s("933")];
        let err = substitute(
            "MATCH (p: Person {{id: {personId}}}) WHERE c.country = {countryName}",
            &header,
            &row,
            &[],
        )
        .unwrap_err();
        assert!(
            err.contains("unsubstituted placeholder"),
            "error was: {err}"
        );
        assert!(err.contains("countryName"), "error was: {err}");
    }

    #[test]
    fn substitute_doesnt_false_alarm_on_descriptor_shorthand() {
        // The descriptor shorthand `{id: 933}` (with a colon and a
        // value, post-substitute) must NOT be flagged as an
        // unsubstituted placeholder. The placeholder check should only
        // catch bare `{identifier}` tokens. (Rust string literals
        // don't need `{{` escaping — what's written here is what's
        // in the template, single-brace-on-each-side.)
        let header = vec![s("personId")];
        let row = vec![s("933")];
        let out = substitute(
            "MATCH (p: Person {id: {personId}}) RETURN p.firstName",
            &header,
            &row,
            &[],
        )
        .unwrap();
        assert_eq!(out, "MATCH (p: Person {id: 933}) RETURN p.firstName");
    }

    #[test]
    fn substitute_doesnt_false_alarm_on_repetition_quantifier() {
        // `{1,3}` in -[:knows*1..3]- shouldn't trip the
        // unsubstituted-placeholder check. (No params, no template
        // placeholders — just verifying the post-pass scan is clean.)
        let header: Vec<String> = vec![];
        let row: Vec<String> = vec![];
        let out = substitute(
            "MATCH (a)-[:knows*1..3]-(b) RETURN b.name",
            &header,
            &row,
            &[],
        )
        .unwrap();
        assert_eq!(out, "MATCH (a)-[:knows*1..3]-(b) RETURN b.name");
    }

    // --- find_unsubstituted_placeholder ---

    #[test]
    fn placeholder_finder_catches_bare_identifier() {
        assert_eq!(
            find_unsubstituted_placeholder("{personId}"),
            Some("personId")
        );
        assert_eq!(
            find_unsubstituted_placeholder("WHERE x = {foo} AND y = 1"),
            Some("foo")
        );
    }

    #[test]
    fn placeholder_finder_skips_descriptor_shorthand() {
        assert_eq!(find_unsubstituted_placeholder("{id: 933}"), None);
        assert_eq!(find_unsubstituted_placeholder("{name: 'Alice'}"), None);
    }

    #[test]
    fn placeholder_finder_skips_repetition() {
        assert_eq!(find_unsubstituted_placeholder("{1,3}"), None);
        assert_eq!(find_unsubstituted_placeholder("{0,}"), None);
    }
}
