//! CLI argument handling for the `frogql` binary.
//!
//! The regression these pin: the REPL opens its database create-on-open
//! (sqlite3-style), so anything reaching the path position becomes a file.
//! `frogql --version` used to fall through to that dispatch and create a
//! database literally named `--version` in the working directory.

#![cfg(feature = "repl")]

use std::path::Path;
use std::process::Command;

fn frogql() -> Command {
    Command::new(env!("CARGO_BIN_EXE_frogql"))
}

/// A temp dir we can assert is still empty afterwards.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("frogql_cli_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .expect("read scratch dir")
        .next()
        .is_none()
}

#[test]
fn test_version_prints_and_exits_zero() {
    for flag in ["--version", "-V"] {
        let out = frogql().arg(flag).output().expect("run frogql");
        assert!(out.status.success(), "{flag} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            stdout.trim(),
            format!("frogql {}", env!("CARGO_PKG_VERSION")),
            "{flag} output"
        );
    }
}

#[test]
fn test_help_prints_usage_on_stdout_and_exits_zero() {
    for flag in ["--help", "-h"] {
        let out = frogql().arg(flag).output().expect("run frogql");
        assert!(out.status.success(), "{flag} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.starts_with("Usage:"), "{flag} stdout: {stdout}");
        assert!(stdout.contains("--no-typecheck"), "{flag} lists options");
    }
}

#[test]
fn test_no_arguments_prints_usage_on_stderr_and_exits_nonzero() {
    let out = frogql().output().expect("run frogql");
    assert!(!out.status.success(), "bare invocation should exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("Usage:"), "stderr: {stderr}");
}

#[test]
fn test_unknown_flag_is_rejected_not_treated_as_a_database_path() {
    let dir = scratch("unknown_flag");
    let out = frogql()
        .arg("--definitely-not-a-flag")
        .current_dir(&dir)
        .output()
        .expect("run frogql");

    assert!(!out.status.success(), "unknown flag should exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Unknown option: --definitely-not-a-flag"),
        "stderr: {stderr}"
    );
    assert!(
        is_empty(&dir),
        "an unknown flag must not create a database file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_version_creates_no_database_file() {
    let dir = scratch("version_no_file");
    let out = frogql()
        .arg("--version")
        .current_dir(&dir)
        .output()
        .expect("run frogql");

    assert!(out.status.success());
    assert!(
        is_empty(&dir),
        "--version must not create a database file (it once created one named `--version`)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
