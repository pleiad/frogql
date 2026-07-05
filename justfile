# froGQL local-dev recipes. Mirrors CI (.github/workflows/ci.yml) so the
# commands you type locally match what gates a PR — but CI stays the source
# of truth for what actually runs (four parallel jobs); this file is for
# ergonomics, not for driving CI.
#
# Install the `just` runner:  cargo install just
#   (Windows, skip the from-source build:  winget install Casey.Just)
#
# Recipes are plain `cargo` commands (no shell-specific syntax), so run each
# platform under its native shell. On Windows use PowerShell: just's default
# (sh / Git Bash) is a non-login shell that often lacks ~/.cargo/bin on PATH,
# which surfaces as `cargo: command not found`.
set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoProfile", "-NoLogo", "-Command"]

# Default: list recipes.
default:
    @just --list

# --- Static checks -----------------------------------------------------------

# Format only — instant, no compile. Use when you just want to reformat.
fmt:
    cargo fmt --all

# All static gates, no mutation: the exact set CI's fmt/check/clippy jobs run.
lint:
    cargo fmt --all -- --check
    cargo check --workspace --all-targets
    cargo clippy --workspace --all-targets -- -D clippy::all

# Runs over in-progress work (--allow-dirty/--allow-staged), so its edits
# intermix with yours in `git diff`; anything not machine-fixable still errors.
# Auto-fix: reformat + apply machine-applicable clippy fixes.
lint-fix:
    cargo fmt --all
    cargo clippy --fix --workspace --all-targets --allow-dirty --allow-staged -- -D clippy::all

# --- Tests -------------------------------------------------------------------

# Unqualified so the set never drifts as tests are added. Includes the slow
# bench_test (~1 min); during tight iteration name the targets you touched.
# Full sweep: cargo test (lib unit tests + every integration target).
test:
    cargo test

# --- REPL --------------------------------------------------------------------

# Rebuilds if stale via `cargo run`. Extra args pass straight through, so one
# recipe covers every variant:
#   just repl movies.gdb                          # open existing
#   just repl movies.gdb --import-csv path/to/dir # create + import, then open
#   just repl movies.gdb --no-typecheck           # skip typecheck this session
# Open the frogql REPL on a database.
repl database *args:
    cargo run --release --bin frogql -- {{database}} {{args}}
