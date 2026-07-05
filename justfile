# froGQL local-dev recipes. Mirrors CI (.github/workflows/ci.yml) so the
# commands you type locally match what gates a PR — but CI stays the source
# of truth for what actually runs (four parallel jobs); this file is for
# ergonomics, not for driving CI.
#
# Install the `just` runner:  cargo install just
#   (Windows, skip the from-source build:  winget install Casey.Just)
#
# Pin bash for deterministic behaviour on Windows (Git Bash) rather than
# relying on just's default shell resolution.
set shell := ["bash", "-cu"]

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

# Auto-fix: rewrite formatting + apply machine-applicable clippy fixes.
# --allow-dirty/--allow-staged so it runs over in-progress work; the cost is
# its edits intermix with yours in `git diff`. Anything not machine-fixable
# still surfaces as an error via `-D clippy::all`.
lint-fix:
    cargo fmt --all
    cargo clippy --fix --workspace --all-targets --allow-dirty --allow-staged -- -D clippy::all

# --- Tests -------------------------------------------------------------------

# Full sweep: lib unit tests + every integration target. Unqualified so the
# set never drifts as tests are added. Includes the slow bench_test (~1 min);
# during tight iteration name the targets you touched instead, or `just fmt`.
test:
    cargo test

# --- REPL --------------------------------------------------------------------

# Open the frogql REPL on a database (rebuilds if stale via `cargo run`).
# Extra args pass straight through, so this one recipe covers every variant:
#   just repl movies.gdb                          # open existing
#   just repl movies.gdb --import-csv path/to/dir # create + import, then open
#   just repl movies.gdb --no-typecheck           # skip typecheck this session
repl database *args:
    cargo run --release --bin frogql -- {{database}} {{args}}
