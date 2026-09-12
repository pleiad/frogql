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

# What to run while iterating: the in-crate unit tests plus whichever
# integration targets you touched. Seconds, not minutes.
#   just t                          # lib only
#   just t runtime_test parser_test # lib + those targets
t *targets:
    cargo test --lib
    @if [ -n "{{targets}}" ]; then cargo test $(for t in {{targets}}; do printf -- "--test $t "; done); fi

# Full sweep — the pre-commit gate, not a per-edit one.
#
# Its wall clock is not the tests: they total ~5 s. It is a first-execution
# cost macOS charges per freshly linked binary — ~5-8 s each, at 0% CPU,
# once per file — and `cargo test` runs targets one at a time, so 108 of
# them serialise into minutes. Touching store/, pager/ or model/ relinks
# everything and you pay it in full.
#
# So: link everything first, then run every binary once in parallel to
# absorb that cost 16-wide, then let cargo do the actual testing against
# already-warm files. ~30 s of warming replaces ~30 min of serial waiting.
test:
    cargo test --no-run
    @find target/debug/deps -type f -perm +111 ! -name '*.d' \
        | xargs -P 16 -I{} sh -c '{} --list >/dev/null 2>&1' || true
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
