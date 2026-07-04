# froGQL local-dev recipes. Mirrors CI (.github/workflows/ci.yml) so the
# commands you type locally match what gates a PR — but CI stays the source
# of truth for what actually runs (four parallel jobs); this file is for
# ergonomics, not for driving CI.
#
# Recipes use bash pipelines (ls | sed | awk), so force bash even on Windows.
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

# Lib unit tests + every integration test under tests/, auto-discovered and
# excluding bench_test (pre-existing failures). Same shim CI's test job uses,
# so new tests/*.rs files are picked up the moment they land.
test:
    cargo test --workspace --lib
    cargo test $(ls tests/*.rs | xargs -n1 basename | sed 's/\.rs$//' \
        | grep -v '^bench_test$' | awk '{print "--test " $0}' | tr '\n' ' ')

# --- REPL --------------------------------------------------------------------

# Open the frogql REPL on a database (rebuilds if stale via `cargo run`).
# Extra args pass straight through, so this one recipe covers every variant:
#   just repl movies.gdb                          # open existing
#   just repl movies.gdb --import-csv path/to/dir # create + import, then open
#   just repl movies.gdb --no-typecheck           # skip typecheck this session
repl database *args:
    cargo run --release --bin frogql -- {{database}} {{args}}
