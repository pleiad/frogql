//! Run a chosen IC against GraphLite-AI/GraphLite for every substitution-param
//! row, emitting per-iter CSV in the cross-system schema.
//!
//! Output schema (matches src/bin/ldbc_bench.rs):
//!     query;backend;params;row;iter;result_count;elapsed_ns
//!
//! `backend` is fixed to `graphlite-iso-gql`. `params` is the raw pipe-joined
//! param row from the LDBC params file. `row` is the 0-based param-row index.
//!
//! GraphLite SDK 0.0.1 has no parameter-binding API, so the runner does
//! string substitution per param row before calling `session.query`. The
//! template uses double-curly placeholders (`{{name}}`); see ic<n>.gql for
//! the template.
//!
//! IC2 also requires union over the `:Comment | :Post` message labels —
//! GraphLite has no documented `:A | :B` syntax — so the runner issues
//! two queries per param row (one per label), concatenates the rows, and
//! treats the merged set as the IC's result for shape/timing.

use clap::Parser;
use graphlite_sdk::{GraphLite, QueryResult, Value};
use serde::Deserialize;
use std::fs;
use std::io::{BufWriter, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(about = "Run an IC against GraphLite, emitting cross-system CSV.")]
struct Args {
    /// Output CSV path.
    out_csv: PathBuf,
    /// LDBC IC number.
    #[arg(long, default_value_t = 2)]
    ic: u32,
    /// Measured iterations per param row.
    #[arg(long, default_value_t = 10)]
    iters: u32,
    /// Warmup iterations per param row (discarded).
    #[arg(long, default_value_t = 2)]
    warmup: u32,
}

#[derive(Deserialize)]
struct IcToml {
    status: String,
    params_file: String,
    return_columns: Option<Vec<String>>,
    expected_shape: Option<String>,
}

fn repo_root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    here.parent().unwrap().parent().unwrap().parent().unwrap().to_path_buf()
}

fn load_toml(path: &Path) -> Result<IcToml, Box<dyn std::error::Error>> {
    let raw = fs::read_to_string(path)?;
    Ok(toml::from_str(&raw)?)
}

/// Read an LDBC pipe-delimited params file.
/// Returns (header_columns, [data_rows]).
fn load_params(path: &Path) -> Result<(Vec<String>, Vec<Vec<String>>), Box<dyn std::error::Error>> {
    let raw = fs::read_to_string(path)?;
    let mut lines = raw.lines().filter(|l| !l.trim().is_empty());
    let header_line = lines.next().ok_or("empty params file")?;
    let header: Vec<String> = header_line.split('|').map(str::to_string).collect();
    let data: Vec<Vec<String>> = lines
        .map(|l| l.split('|').map(str::to_string).collect())
        .collect();
    Ok((header, data))
}

/// `dot.path` (in the toml) → `dot_path` (the AS-alias form we use in queries).
fn derive_columns(return_columns: &[String]) -> Vec<String> {
    return_columns.iter().map(|c| c.replace('.', "_")).collect()
}

fn shape_of_value(v: &Value) -> &'static str {
    // Map GraphLite's Value variants to the cross-system shape vocabulary
    // used in `expected_shape` (i=int, s=str, n=null, b=bool, f=float, l=list,
    // r=record). GraphLite stores all numbers as f64 (`Number(f64)`), so we
    // call a Number "i" when its fractional part is zero (the LDBC dataset's
    // ids and creationDates are conceptually ints, just transported as f64
    // by this engine) and "f" otherwise. Without this the bench would never
    // match the expected `i,s,s,i,n/s,i` shape for IC2 even on correct
    // results.
    match v {
        Value::Null => "n",
        Value::Boolean(_) => "b",
        Value::Number(n) if n.fract() == 0.0 => "i",
        Value::Number(_) => "f",
        Value::String(_) => "s",
        Value::Array(_) | Value::List(_) | Value::Vector(_) => "l",
        _ => "r",
    }
}

/// Per-column type-set across all rows; columns join with `,`, types within
/// a column join with `/`. Mirrors `shape_of_result` in src/bin/ldbc_bench.rs.
fn shape_of_rows(rows: &[Vec<Value>], n_columns: usize) -> String {
    if rows.is_empty() {
        return "empty".into();
    }
    let mut cols: Vec<std::collections::BTreeSet<&'static str>> =
        (0..n_columns).map(|_| std::collections::BTreeSet::new()).collect();
    for r in rows {
        for (i, v) in r.iter().enumerate().take(n_columns) {
            cols[i].insert(shape_of_value(v));
        }
    }
    cols.iter()
        .map(|s| s.iter().copied().collect::<Vec<_>>().join("/"))
        .collect::<Vec<_>>()
        .join(",")
}

fn verify_shape(actual: &str, expected: &str) -> Option<String> {
    let a: Vec<std::collections::BTreeSet<&str>> = actual
        .split(',')
        .map(|c| c.split('/').collect())
        .collect();
    let e: Vec<std::collections::BTreeSet<&str>> = expected
        .split(',')
        .map(|c| c.split('/').collect())
        .collect();
    if a.len() != e.len() {
        return Some(format!("column count: actual={}, expected={}", a.len(), e.len()));
    }
    for (i, (ac, ec)) in a.iter().zip(e.iter()).enumerate() {
        if !ac.is_subset(ec) {
            let extras: Vec<&str> = ac.difference(ec).copied().collect();
            return Some(format!(
                "col {i}: actual {ac:?} not ⊆ expected {ec:?} (extra: {extras:?})"
            ));
        }
    }
    None
}

/// Pull the rows out of a `QueryResult` as positional `Vec<Value>` keyed by
/// the alias names we asked for in the RETURN. The SDK 0.0.1 surface gives us
/// `Row` accessors; we keep the public types here narrow to simplify the rest
/// of the runner.
fn extract_rows(result: &QueryResult, columns: &[String]) -> Vec<Vec<Value>> {
    let mut out = Vec::with_capacity(result.rows.len());
    for row in &result.rows {
        let vals: Vec<Value> = columns
            .iter()
            .map(|c| row.get_value(c).cloned().unwrap_or(Value::Null))
            .collect();
        out.push(vals);
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let repo = repo_root();
    let ic = args.ic;
    let toml_path = repo.join(format!("bench/ldbc-queries/ic{ic}.toml"));
    let template_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("ic{ic}.gql"));
    let db_path = repo.join(format!("bench/data/cross-system/graphlite/ic{ic}.db"));

    if !toml_path.is_file() {
        eprintln!("  toml missing: {}", toml_path.display());
        std::process::exit(1);
    }
    if !template_path.is_file() {
        eprintln!(
            "  query template missing: {}\n  \
             (write it as a translation of {})",
            template_path.display(),
            toml_path.display()
        );
        std::process::exit(1);
    }
    let ic_toml = load_toml(&toml_path)?;
    if ic_toml.status != "implemented" {
        eprintln!(
            "  ic{ic}.toml status is {:?}, not 'implemented'. Skipping.",
            ic_toml.status
        );
        return Ok(());
    }
    let params_file = repo
        .join("bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1")
        .join(&ic_toml.params_file);
    if !params_file.is_file() {
        eprintln!("  params file missing: {}", params_file.display());
        eprintln!("  run ./target/release/bench_setup from the repo root first.");
        std::process::exit(1);
    }
    if !db_path.exists() {
        eprintln!("  graphlite db missing: {}", db_path.display());
        eprintln!("  run `cargo run --release --bin graphlite-setup --` from this dir first.");
        std::process::exit(1);
    }

    let columns = derive_columns(ic_toml.return_columns.as_deref().unwrap_or(&[]));
    let template = fs::read_to_string(&template_path)?;
    // Strip leading `--` comment lines so they don't reach the engine.
    let template: String = template
        .lines()
        .skip_while(|l| l.starts_with("--") || l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    let (header, params_rows) = load_params(&params_file)?;
    eprintln!(
        "  graphlite ic{ic}: {} param rows × {} iters (+ {} warmup)",
        params_rows.len(),
        args.iters,
        args.warmup
    );

    let db = GraphLite::open(&db_path)?;
    let session = db.session("bench")?;
    session.execute("USE SCHEMA ldbc")?;
    session.execute("USE GRAPH sf01")?;

    let backend_label = "graphlite-iso-gql";
    let query_label = format!("IC{ic}");

    let out = fs::File::create(&args.out_csv)?;
    let mut out = BufWriter::new(out);
    writeln!(out, "query;backend;params;row;iter;result_count;elapsed_ns")?;

    // IC2's union-of-message-labels expressed at the runner level: run the
    // template once per label, concatenate rows. For other ICs without this
    // pattern, the labels list will be `["Person"]` or similar (TODO when
    // such an IC lands; for now hard-code IC2's set).
    let message_labels: Vec<&str> = match ic {
        2 => vec!["Comment", "Post"],
        _ => vec![],
    };

    let mut error_rows = 0usize;
    for (row_idx, raw_row) in params_rows.iter().enumerate() {
        let joined = raw_row.join("|");

        // warmup. A warmup failure is a strong signal that this param row
        // will fail for every measured iter too — record one sentinel and
        // skip the measured iters rather than spam identical failures.
        let mut warmup_failed: Option<String> = None;
        for _ in 0..args.warmup {
            if let Err(why) = run_one(
                &session, &template, &header, raw_row, &message_labels, &columns,
            ) {
                warmup_failed = Some(why);
                break;
            }
        }
        if let Some(why) = warmup_failed {
            eprintln!("  row {row_idx}: WARMUP FAIL — {why}; emitting sentinel and skipping measured iters");
            // Emit ONE sentinel row per failing param row. result_count = -1
            // marks "errored" for compare_results.py.
            writeln!(
                out,
                "{};{};{};{};{};{};{}",
                query_label, backend_label, joined, row_idx, 0, -1, 0
            )?;
            error_rows += 1;
            continue;
        }

        let mut iter0_rows: Option<Vec<Vec<Value>>> = None;
        let mut last_elapsed_ns: u128 = 0;
        let mut iter_errored = false;
        for n in 0..args.iters {
            let t = Instant::now();
            let outcome = run_one(
                &session, &template, &header, raw_row, &message_labels, &columns,
            );
            let elapsed_ns = t.elapsed().as_nanos();
            last_elapsed_ns = elapsed_ns;
            match outcome {
                Ok(rows) => {
                    writeln!(
                        out,
                        "{};{};{};{};{};{};{}",
                        query_label, backend_label, joined, row_idx, n, rows.len(), elapsed_ns
                    )?;
                    if n == 0 {
                        iter0_rows = Some(rows);
                    }
                }
                Err(why) => {
                    eprintln!("  row {row_idx} iter {n}: ERROR — {why}");
                    writeln!(
                        out,
                        "{};{};{};{};{};{};{}",
                        query_label, backend_label, joined, row_idx, n, -1, elapsed_ns
                    )?;
                    iter_errored = true;
                }
            }
        }
        if iter_errored {
            error_rows += 1;
        }

        match iter0_rows {
            Some(rows) => {
                let actual_shape = shape_of_rows(&rows, columns.len());
                let actual_count = rows.len();
                let status = match &ic_toml.expected_shape {
                    None => "no-expected".to_string(),
                    Some(exp) => match verify_shape(&actual_shape, exp) {
                        None => "ok".to_string(),
                        Some(why) => format!("fail reason=\"{why}\""),
                    },
                };
                eprintln!(
                    "  SHAPE row={row_idx} count={actual_count} shape={actual_shape} status={status}"
                );
                eprintln!(
                    "  row {row_idx}: rc={actual_count} last_iter_ms={:.2}",
                    last_elapsed_ns as f64 / 1e6
                );
            }
            None => {
                eprintln!("  row {row_idx}: no successful iters");
            }
        }
    }

    eprintln!(
        "  done -> {} ({} of {} param rows had at least one error)",
        args.out_csv.display(),
        error_rows,
        params_rows.len()
    );
    Ok(())
}

/// Outcome of running the template once for a single param row.
///
/// `Ok(rows)` carries the union of returned rows across the per-label sub-queries.
/// `Err(reason)` is set if ANY sub-query failed (returned `Err` or panicked) —
/// we treat the whole param-row run as failed because the IC2 result is the
/// union of Comment + Post, and a partial union would silently undercount.
/// The runner emits a sentinel CSV row (result_count = -1) for failures so
/// compare_results.py can surface them.
fn run_one(
    session: &graphlite_sdk::Session,
    template: &str,
    header: &[String],
    raw_row: &[String],
    message_labels: &[&str],
    columns: &[String],
) -> Result<Vec<Vec<Value>>, String> {
    let mut all_rows = Vec::new();
    let labels_iter: Vec<&str> = if message_labels.is_empty() {
        vec![""] // single pass with no message-label substitution
    } else {
        message_labels.to_vec()
    };
    for label in labels_iter {
        let mut q = template.to_string();
        for (col, val) in header.iter().zip(raw_row.iter()) {
            q = q.replace(&format!("{{{{{col}}}}}"), val);
        }
        if !label.is_empty() {
            q = q.replace("{{messageLabel}}", label);
        }
        // Catch both `Err(_)` and panics. GraphLite 0.0.1's lexer panics on
        // some inputs (UTF-8 boundary slicing, see DIVERGENCES.md); we want
        // to record the failure rather than abort the whole bench.
        let result: Result<Result<QueryResult, String>, _> = catch_unwind(AssertUnwindSafe(|| {
            session.query(&q).map_err(|e| format!("{e}"))
        }));
        match result {
            Ok(Ok(qr)) => all_rows.extend(extract_rows(&qr, columns)),
            Ok(Err(e)) => return Err(format!("error: {e}")),
            Err(p) => {
                let msg = if let Some(s) = p.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = p.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "<unknown panic>".to_string()
                };
                return Err(format!("panic: {msg}"));
            }
        }
    }
    Ok(all_rows)
}
