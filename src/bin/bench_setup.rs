//! `bench_setup` — fetch an LDBC SNB dataset + substitution parameters
//! and build the frogql `.gdb`. Idempotent: each step checks for its
//! output and skips if present. Pass `--rebuild` to force the .gdb to
//! rebuild even if it exists.
//!
//! Usage:
//!     bench_setup [--data-dir bench/data] [--sf 0.1] [--rebuild] [--skip-download]
//!
//! Defaults:
//!   --data-dir   bench/data
//!   --sf         0.1
//!   --skip-download   off (set to use already-present archives only)
//!
//! Pass `--sf 0.3` (or any other LDBC scale factor — `1`, `3`, `10`,
//! ...) to fetch a larger dataset. The output paths and URLs are
//! parametrized on the SF string so different scales coexist under
//! the same `--data-dir`. The companion bench (`ldbc_bench`,
//! `internal_bench`) reads the same `--sf` flag and points at the
//! per-SF artifacts.
//!
//! Layout produced (with `<SF>` replaced by the `--sf` value):
//!   <data-dir>/ldbc-sf<SF>.gdb
//!   <data-dir>/ldbc-sf<SF>/social_network-sf<SF>-CsvBasic-LongDateFormatter/
//!   <data-dir>/substitution_parameters-sf<SF>/substitution_parameters-sf<SF>/

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const DATASET_URL_TEMPLATE: &str =
    "https://datasets.ldbcouncil.org/snb-interactive-v1/social_network-sf{SF}-CsvBasic-LongDateFormatter.tar.zst";
const PARAMS_URL_TEMPLATE: &str =
    "https://datasets.ldbcouncil.org/snb-interactive-v1/substitution_parameters-sf{SF}.tar.zst";

/// Scale factors LDBC publishes substitution params for at the canonical
/// URL (probed 2026-05-06 against `datasets.ldbcouncil.org/snb-interactive-v1/`).
/// SF0.3 and SF1 have datasets but no params; for those, bench_setup
/// fetches the dataset and skips params with a warning. internal_bench
/// works without params; ldbc_bench would fail later when it tries to
/// load the params file (its own concern).
const SFS_WITH_PARAMS: &[&str] = &["0.1", "3", "10", "30", "100", "300", "1000"];

fn print_usage(prog: &str) {
    eprintln!(
        "Usage: {prog} [--data-dir <dir>] [--sf <factor>] [--rebuild] [--skip-download]\n\
         \n\
         Downloads an LDBC SNB dataset + substitution params at the\n\
         given scale factor, then builds <data-dir>/ldbc-sf<SF>.gdb\n\
         via frogql --import-ldbc-csv. Idempotent — re-running with\n\
         the artifacts present is a no-op.\n\
         \n\
         Defaults:\n\
         \t--data-dir bench/data\n\
         \t--sf 0.1 (any LDBC SF; params auto-downloaded for\n\
         \t           0.1, 3, 10, 30, 100, 300, 1000 — others get dataset only)\n\
         \t--skip-download is off (set to use only already-present archives)\n\
         \t--rebuild is off (force-rebuild the .gdb even if present)"
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut data_dir = PathBuf::from("bench/data");
    let mut sf: String = "0.1".to_string();
    let mut rebuild = false;
    let mut skip_download = false;
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
            "--data-dir" => {
                data_dir = PathBuf::from(need_value(i, "--data-dir"));
                i += 2;
            }
            "--sf" => {
                sf = need_value(i, "--sf").to_string();
                i += 2;
            }
            "--rebuild" => {
                rebuild = true;
                i += 1;
            }
            "--skip-download" => {
                skip_download = true;
                i += 1;
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

    fs::create_dir_all(&data_dir)
        .unwrap_or_else(|e| fail(&format!("create data-dir {}: {e}", data_dir.display())));
    eprintln!("data-dir: {}", data_dir.display());
    eprintln!("scale:    sf{sf}");
    let params_available = SFS_WITH_PARAMS.contains(&sf.as_str());
    if !params_available {
        eprintln!(
            "note: LDBC has no substitution params for sf{sf} at the canonical URL.\n\
             Will fetch the dataset only — internal_bench works without\n\
             params, ldbc_bench will need user-supplied params (provide\n\
             via --params-dir)."
        );
    }

    let dataset_url = DATASET_URL_TEMPLATE.replace("{SF}", &sf);
    let params_url = PARAMS_URL_TEMPLATE.replace("{SF}", &sf);

    // Step 1: dataset.
    let dataset_archive = data_dir.join(format!("ldbc-sf{sf}.tar.zst"));
    let dataset_dir = data_dir.join(format!("ldbc-sf{sf}"));
    let csv_dir = dataset_dir.join(format!("social_network-sf{sf}-CsvBasic-LongDateFormatter"));
    // Idempotency guard: a partially-extracted dir can exist from an
    // interrupted run. Verify a key sub-path that only appears post-
    // extraction (`dynamic/`).
    let dataset_extracted = csv_dir.is_dir() && csv_dir.join("dynamic").is_dir();
    if dataset_extracted {
        eprintln!("[skip] dataset already extracted at {}", csv_dir.display());
    } else {
        if !dataset_archive.exists() {
            if skip_download {
                fail(&format!(
                    "missing {} and --skip-download set",
                    dataset_archive.display()
                ));
            }
            download(&dataset_url, &dataset_archive);
        } else {
            eprintln!(
                "[skip] dataset archive already at {}",
                dataset_archive.display()
            );
        }
        extract_tar_zst(&dataset_archive, &dataset_dir);
    }

    // Step 2: substitution params (skip when LDBC publishes none for this SF).
    let params_archive = data_dir.join(format!("substitution_parameters-sf{sf}.tar.zst"));
    let params_dir = data_dir.join(format!("substitution_parameters-sf{sf}"));
    let params_inner = params_dir.join(format!("substitution_parameters-sf{sf}"));
    let params_extracted =
        params_inner.is_dir() && params_inner.join("interactive_2_param.txt").is_file();
    if !params_available {
        eprintln!("[skip] no params published for sf{sf}; user must supply via --params-dir");
    } else if params_extracted {
        eprintln!(
            "[skip] params already extracted at {}",
            params_inner.display()
        );
    } else {
        if !params_archive.exists() {
            if skip_download {
                fail(&format!(
                    "missing {} and --skip-download set",
                    params_archive.display()
                ));
            }
            download(&params_url, &params_archive);
        } else {
            eprintln!(
                "[skip] params archive already at {}",
                params_archive.display()
            );
        }
        extract_tar_zst(&params_archive, &params_dir);
    }

    // Step 3: .gdb. Shells out to the frogql binary's --import-ldbc-csv
    // path so the .gdb is built by exactly the same code the REPL uses.
    let gdb_path = data_dir.join(format!("ldbc-sf{sf}.gdb"));
    if gdb_path.exists() && !rebuild {
        eprintln!(
            "[skip] .gdb already at {} (pass --rebuild to force)",
            gdb_path.display()
        );
    } else {
        if rebuild && gdb_path.exists() {
            fs::remove_file(&gdb_path)
                .unwrap_or_else(|e| fail(&format!("remove old .gdb {}: {e}", gdb_path.display())));
        }
        build_gdb(&gdb_path, &csv_dir);
    }

    eprintln!("\n✓ setup complete");
    let default_data_dir = data_dir == Path::new("bench/data");
    if default_data_dir {
        eprintln!(
            "  run the bench with:\n\
             \t./target/release/ldbc_bench {} --ic 2",
            gdb_path.display()
        );
    } else {
        eprintln!(
            "  run the bench with:\n\
             \t./target/release/ldbc_bench {} \\\n\
             \t  --params-dir {} \\\n\
             \t  --ic 2",
            gdb_path.display(),
            params_inner.display()
        );
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

/// Download `url` to `dest`. Streams the response so the whole
/// archive doesn't need to fit in RAM.
fn download(url: &str, dest: &Path) {
    eprintln!("downloading {url}");
    let t0 = Instant::now();
    let resp = ureq::get(url)
        .call()
        .unwrap_or_else(|e| fail(&format!("download failed: {e}")));
    let total: Option<u64> = resp.header("Content-Length").and_then(|s| s.parse().ok());
    let mut reader = resp.into_reader();
    let mut file =
        fs::File::create(dest).unwrap_or_else(|e| fail(&format!("create {}: {e}", dest.display())));

    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    let mut total_read: u64 = 0;
    let mut next_log: u64 = 10 << 20;
    loop {
        let n = reader
            .read(&mut buf)
            .unwrap_or_else(|e| fail(&format!("read: {e}")));
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .unwrap_or_else(|e| fail(&format!("write {}: {e}", dest.display())));
        total_read += n as u64;
        if total_read >= next_log {
            match total {
                Some(t) => eprintln!("  {} MiB / {} MiB", total_read >> 20, t >> 20),
                None => eprintln!("  {} MiB", total_read >> 20),
            }
            next_log += 10 << 20;
        }
    }
    eprintln!(
        "  done: {} in {:.1}s",
        format_bytes(total_read),
        t0.elapsed().as_secs_f64()
    );
}

fn format_bytes(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1} KiB", n as f64 / (1u64 << 10) as f64)
    } else {
        format!("{n} B")
    }
}

/// Decompress `archive` (a `.tar.zst` file) into `dest`. Streams; no
/// intermediate uncompressed tar on disk.
fn extract_tar_zst(archive: &Path, dest: &Path) {
    eprintln!("extracting {} -> {}", archive.display(), dest.display());
    let t0 = Instant::now();
    fs::create_dir_all(dest).unwrap_or_else(|e| fail(&format!("create dest: {e}")));
    let f = fs::File::open(archive).unwrap_or_else(|e| fail(&format!("open archive: {e}")));
    let dec = zstd::Decoder::new(f).unwrap_or_else(|e| fail(&format!("zstd decode: {e}")));
    let mut tar = tar::Archive::new(dec);
    tar.unpack(dest)
        .unwrap_or_else(|e| fail(&format!("tar extract: {e}")));
    eprintln!("  done in {:.1}s", t0.elapsed().as_secs_f64());
}

/// Build the .gdb by shelling out to the frogql binary, which we
/// expect alongside this one in `target/release/`. Using the binary
/// rather than calling the loader API directly means the .gdb is
/// constructed by exactly the same code path users run from the REPL.
fn build_gdb(gdb_path: &Path, csv_dir: &Path) {
    eprintln!("building {} from {}", gdb_path.display(), csv_dir.display());
    let t0 = Instant::now();

    let exe_dir = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| fail("could not locate own executable to find sibling frogql binary"));
    let frogql_bin = exe_dir.join(if cfg!(windows) {
        "frogql.exe"
    } else {
        "frogql"
    });
    if !frogql_bin.exists() {
        fail(&format!(
            "frogql binary not found at {}.\n\
             Build it first:\n\t\
             cargo build --release --bin frogql",
            frogql_bin.display()
        ));
    }

    let status = Command::new(&frogql_bin)
        .arg(gdb_path)
        .arg("--import-ldbc-csv")
        .arg(csv_dir)
        .arg("--no-typecheck")
        .stdin(std::process::Stdio::null())
        .status()
        .unwrap_or_else(|e| fail(&format!("spawn frogql: {e}")));
    if !status.success() {
        fail(&format!("frogql import exited with status {status}"));
    }
    eprintln!(
        "  done: {} in {:.1}s",
        gdb_path.display(),
        t0.elapsed().as_secs_f64()
    );
}
