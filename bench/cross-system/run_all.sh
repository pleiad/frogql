#!/usr/bin/env bash
# Cross-system bench — orchestrator (system-outer-loop redesign).
#
# For each system, sequentially:
#   1. Set up ONCE — load full LDBC SF0.1 into the system's native
#      format. Captured to setup_times.txt.
#   2. For each requested IC, run the per-iter bench. Each runner is
#      a separate process invocation; per-system memory is reclaimed
#      automatically at process exit.
#
# This avoids fragmented "per-IC subset" loaders. All systems load
# the same dataset, all queries run against the same DB across ICs.
#
# Usage:
#   bench/cross-system/run_all.sh \
#       [--ics 2,3,11]                # default: 2
#       [--iters N]                   # default: 10 measured iters per param row
#       [--warmup N]                  # default: 2
#       [--only gqlite,kuzu]          # subset of systems
#       [--rebuild-setup]             # force per-system setup re-run
#                                     # (default: skip setup if DB exists)
#       [--ablate]                    # run gqlite in 2 modes: baseline
#                                     # (all opts on) and
#                                     # GQLITE_DISABLE_INDEX_FOLD=1.
#                                     # The fold pass turns
#                                     # `MATCH (n {id:X})` into a single
#                                     # NodeId pre-bind via the secondary
#                                     # index. With it disabled, the
#                                     # variable enters the LTJ join
#                                     # normally and gets scanned. This
#                                     # is the surgical "did the LTJ
#                                     # optimization buy anything"
#                                     # comparison. Other gqlite
#                                     # ablations (lazy-vs-disk
#                                     # backend, etc.) belong in the
#                                     # internal bench, not here.
#
# Output: bench/cross-system/results/<timestamp>/
#   setup_times.txt         metadata: per-system setup wall time
#   <system>.<icN>.csv      per (system, IC): per-iter latency rows
#   cross_system.csv        all per-iter rows concatenated
#   comparison.txt          compare_results.py output (per-IC tables)
#   <system>.<icN>.stderr.log  per (system, IC) runner stderr
#   skipped.log             any system+IC pair that couldn't run
#   run_info.txt            timestamp, host, gqlite commit, etc.
#
# Prereq: ./target/release/bench_setup has been run so the LDBC SF0.1
# CSVs are present under bench/data/ldbc-sf0.1/, and gqlite's own
# .gdb file is built. Each external system additionally needs its
# Python deps installed — see the per-system subdir's README.md or
# run `bench/cross-system/install_python_deps.sh`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RESULTS_ROOT="$SCRIPT_DIR/results"

ICS_ARG="2,5,6,8,9,11"   # default: every IC currently flipped to status="implemented" in bench/ldbc-queries/. Keep this in sync as ICs come online.
ITERS=10
WARMUP=2
ONLY=""
REBUILD=0
ABLATE=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --ic|--ics)         ICS_ARG="$2"; shift 2 ;;
        --iters)            ITERS="$2"; shift 2 ;;
        --warmup)           WARMUP="$2"; shift 2 ;;
        --only)             ONLY="$2"; shift 2 ;;
        --rebuild-setup)    REBUILD=1; shift ;;
        --ablate)           ABLATE=1; shift ;;
        -h|--help)
            awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
            exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

IFS=',' read -ra IC_LIST <<<"$ICS_ARG"

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUT_DIR="$RESULTS_ROOT/$TIMESTAMP"
mkdir -p "$OUT_DIR"
START_EPOCH=$(date +%s)

# Run-environment metadata. Captured up-front so any future re-analysis
# of the timestamped results dir knows which gqlite revision and host
# produced these numbers.
{
    echo "timestamp:        $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "ics:              ${IC_LIST[*]}"
    echo "iters:            $ITERS"
    echo "warmup:           $WARMUP"
    echo "rebuild_setup:    $REBUILD"
    echo "ablate:           $ABLATE"
    echo "host:             $(hostname 2>/dev/null || echo unknown)"
    echo "uname:            $(uname -a 2>/dev/null || echo unknown)"
    echo "gqlite_branch:    $(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
    echo "gqlite_commit:    $(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "gqlite_dirty:     $(git -C "$REPO_ROOT" diff --quiet 2>/dev/null && echo no || echo yes)"
    echo "rustc:            $(rustc --version 2>&1 || echo missing)"
    echo "python:           $(python --version 2>&1 || echo missing)"
    # Per-system versions — pinned in requirements.txt but capturing
    # the resolved version per run guards against accidental drift
    # (e.g. a fresh `pip install` resolving an unpinned transitive).
    echo "graphqlite_ver:   $(python -c 'import importlib.metadata as m; print(m.version("graphqlite"))' 2>/dev/null || echo missing)"
    echo "kuzu_ver:         $(python -c 'import importlib.metadata as m; print(m.version("kuzu"))' 2>/dev/null || echo missing)"
    echo "psutil_ver:       $(python -c 'import importlib.metadata as m; print(m.version("psutil"))' 2>/dev/null || echo missing)"
    echo "ldbc_dataset:     SF0.1"
} > "$OUT_DIR/run_info.txt"

# Systems registered here. Order is per-system bench order; doesn't
# affect comparison since compare_results.py sorts independently.
ALL_SYSTEMS=(gqlite graphqlite kuzu)

if [[ -n "$ONLY" ]]; then
    IFS=',' read -ra SYSTEMS <<<"$ONLY"
else
    SYSTEMS=("${ALL_SYSTEMS[@]}")
fi

cd "$REPO_ROOT"
echo "=== Cross-system bench — $TIMESTAMP ==="
echo "  results: $OUT_DIR"
echo "  ics:     ${IC_LIST[*]}"
echo "  iters:   $ITERS"
echo "  warmup:  $WARMUP"
echo "  systems: ${SYSTEMS[*]}"
[[ $REBUILD -eq 1 ]] && echo "  setup:   --rebuild-setup (force fresh per-system load)"
[[ $ABLATE -eq 1 ]] && echo "  ablate:  --ablate (gqlite: baseline + lazy-no-fold)"
echo ""

# ---- gqlite ablation modes -----------------------------------------
#
# When --ablate is set, gqlite runs in two modes: baseline (all opts
# on) and no-fold (GQLITE_DISABLE_INDEX_FOLD=1). The fold pass turns
# `MATCH (n {id:X})` into a single-NodeId pre-bind via the secondary
# index; without it the variable enters the LTJ join normally and
# gets scanned. This is the surgical "did the LTJ optimization buy
# anything" comparison — and it's the only gqlite-internal knob the
# external bench cares about. Two modes is all we need for the
# headline.
#
# Other gqlite-internal characterizations (lazy-vs-disk backend, the
# auto-index build itself, TripleIndex cache, ...) belong in the
# internal bench (bench/typecheck-bench, soon-to-be-broader). The
# external bench's job is to compare gqlite to other systems on
# user-facing query latency; gqlite-internal sub-knob ablations are
# off-charter here.
declare -A ABLATION_LABELS=(
    [baseline]="lazy-baseline"
    [no-fold]="lazy-no-fold"
)
declare -A ABLATION_ENV=(
    [baseline]=""
    [no-fold]="GQLITE_DISABLE_INDEX_FOLD=1"
)
declare -A ABLATION_ARGS=(
    [baseline]=""
    [no-fold]=""
)
ABLATION_MODES=(baseline no-fold)

# ---- Per-system loop ------------------------------------------------

# Per-system setup commands. Each is the path to a setup.py / no-op
# call that loads the system's native DB. gqlite's "setup" is the
# repo-root bench_setup binary which is shared across IC runs (it
# produces the .gdb that ldbc_bench reads).
# gqlite's setup is the workspace's `bench_setup` Rust binary. On
# Windows it can't be invoked from a non-admin MSYS shell because
# the binary name contains "setup", which triggers the OS's
# installer-detection heuristic — Windows demands UAC elevation
# (`ERROR_ELEVATION_REQUIRED` / "La operación solicitada requiere
# elevación", error 740). `cargo run` doesn't help — cargo just
# shells out to the .exe and Windows blocks it the same way. The
# clean fix is upstream (rename the binary OR add a Windows manifest
# that explicitly declares `requestedExecutionLevel asInvoker`); we
# don't carry that change in this PR.
#
# Workaround: gqlite's setup is treated as a user-managed prereq.
# `run_all.sh` doesn't try to invoke bench_setup; it just verifies
# that `bench/data/ldbc-sf0.1.gdb` exists. Setup time for gqlite is
# reported as "user-managed" — to capture a comparable number, run
# `cargo run --release --bin bench_setup -- --rebuild` from a
# non-MSYS shell (Windows: PowerShell or cmd.exe). On Linux/macOS
# this whole problem doesn't exist.
declare -A SETUP_CMD=(
    [graphqlite]="python $SCRIPT_DIR/graphqlite/setup.py"
    [kuzu]="python $SCRIPT_DIR/kuzu/setup.py"
)

declare -A REBUILD_FLAG=(
    [graphqlite]="--force"
    [kuzu]="--force"
)

# Per-system DB-existence checks. Setup is skipped if the marker
# already exists, unless --rebuild-setup is passed. These are just
# heuristics for "is the DB built?" — the runner does its own check
# and errors cleanly if the DB is genuinely missing.
declare -A SETUP_MARKER=(
    [gqlite]="$REPO_ROOT/bench/data/ldbc-sf0.1.gdb"
    [graphqlite]="$REPO_ROOT/bench/data/cross-system/graphqlite/ldbc-sf01.db"
    [kuzu]="$REPO_ROOT/bench/data/cross-system/kuzu/ldbc-sf01.db"
)

# Per-system runner commands (path is per-system; dispatch is per-runner).
# Each runner gets `<out_csv> --ic <n> --iters <N> --warmup <N>`.
declare -A RUNNER=(
    [gqlite]="bash $SCRIPT_DIR/gqlite/run.sh"
    [graphqlite]="python $SCRIPT_DIR/graphqlite/run.py"
    [kuzu]="python $SCRIPT_DIR/kuzu/run.py"
)

SETUP_TIMES_FILE="$OUT_DIR/setup_times.txt"
echo "system    elapsed_seconds    db_bytes        status" > "$SETUP_TIMES_FILE"

# Native-DB size of `marker`. All three current backends write a
# single file (gqlite's `.gdb`, graphqlite's SQLite `.db`, Kuzu's
# embedded `.db` — Kuzu's WAL/work data lives in a sibling
# `_kuzu_work/` we don't include because it isn't part of the loaded
# DB). The dir branch is here for forward-compat: future backends
# (e.g. RocksDB-based) ship multi-file directories. Returns "—" on
# missing/unreadable paths so the column always has a value.
db_size_bytes() {
    local p="$1"
    [[ -e "$p" ]] || { echo "—"; return; }
    if [[ -d "$p" ]]; then
        # `du -sb` works on MSYS bash + Linux; sums directory bytes.
        # POSIX `du` returns blocks not bytes, so prefer the GNU -b
        # extension (always available on bench hosts).
        du -sb "$p" 2>/dev/null | awk '{print $1}' || echo "—"
    else
        # Single file — `stat -c %s` is the portable size accessor.
        stat -c %s "$p" 2>/dev/null || echo "—"
    fi
}

for sys in "${SYSTEMS[@]}"; do
    runner_cmd="${RUNNER[$sys]:-}"
    marker="${SETUP_MARKER[$sys]:-}"
    if [[ -z "$runner_cmd" ]]; then
        echo "[SKIP] $sys: not registered in run_all.sh" \
            | tee -a "$OUT_DIR/skipped.log"
        continue
    fi

    echo "==================== $sys ===================="

    # --- Setup phase ---
    # gqlite is special: its setup binary can't be invoked from this
    # script on Windows (UAC), so it's user-managed — verify only.
    # Other systems have a setup_cmd we can invoke directly.
    setup_cmd="${SETUP_CMD[$sys]:-}"
    setup_status="ok"
    elapsed_s="0.00"
    if [[ -z "$setup_cmd" ]]; then
        # User-managed setup (e.g. gqlite's bench_setup).
        if [[ ! -e "$marker" ]]; then
            echo "[FAIL] $sys: $marker missing." \
                | tee -a "$OUT_DIR/skipped.log"
            echo "  Setup is user-managed for this system. Run:" >&2
            echo "    cargo run --release --bin bench_setup -- --rebuild" >&2
            echo "  from a non-MSYS shell (PowerShell/cmd on Windows)." >&2
            setup_status="fail"
        else
            setup_status="user-managed"
            echo "  setup: user-managed (verified $marker exists)"
        fi
    elif [[ $REBUILD -eq 1 || ! -e "$marker" ]]; then
        if [[ $REBUILD -eq 1 ]]; then
            extra_arg="${REBUILD_FLAG[$sys]:---force}"
            echo "  setup: $setup_cmd $extra_arg (forced rebuild)"
        else
            extra_arg=""
            echo "  setup: $setup_cmd (DB missing, building)"
        fi
        setup_log="$OUT_DIR/${sys}.setup.log"
        setup_start=$(date +%s%N)
        if ! eval "$setup_cmd $extra_arg" >"$setup_log" 2>&1; then
            setup_status="fail"
            echo "[FAIL] $sys setup failed; see $setup_log" \
                | tee -a "$OUT_DIR/skipped.log"
        fi
        setup_end=$(date +%s%N)
        elapsed_s=$(awk "BEGIN { printf \"%.2f\", ($setup_end - $setup_start) / 1e9 }")
    else
        setup_status="cached"
        echo "  setup: cached at $marker"
    fi
    db_bytes=$(db_size_bytes "$marker")
    printf "%-12s %-18s %-15s %s\n" "$sys" "$elapsed_s" "$db_bytes" "$setup_status" \
        >> "$SETUP_TIMES_FILE"

    if [[ "$setup_status" == "fail" ]]; then
        echo "  skipping IC runs for $sys — setup failed"
        continue
    fi

    # --- Per-IC bench phase ---
    # Ablation special-case: when --ablate is set AND we're running
    # gqlite, run the harness once per ablation mode (different
    # GQLITE_DISABLE_* env vars) and rewrite the `backend` CSV column
    # to distinguish the modes. ldbc_bench hard-codes "lazy" as the
    # backend label; awk does the rewrite without touching the
    # binary.
    #
    # Why post-process via awk and not pass a label flag through to
    # ldbc_bench: keeps the ablation logic confined to run_all.sh
    # and avoids touching gqlite's bench-runner code, which other
    # bench paths (the single-system ldbc_bench output) also depend
    # on.
    for ic in "${IC_LIST[@]}"; do
        echo "  --- ic$ic ---"
        if [[ "$sys" == "gqlite" && $ABLATE -eq 1 ]]; then
            for mode in "${ABLATION_MODES[@]}"; do
                label="${ABLATION_LABELS[$mode]}"
                env_setup="${ABLATION_ENV[$mode]}"
                extra_args="${ABLATION_ARGS[$mode]}"
                out_csv="$OUT_DIR/${sys}-${mode}.ic${ic}.csv"
                stderr_log="$OUT_DIR/${sys}-${mode}.ic${ic}.stderr.log"
                echo "    [ablation: $label] env: ${env_setup:-(none)} args: ${extra_args:-(none)}"
                if ! eval "$env_setup $runner_cmd $out_csv --ic $ic --iters $ITERS --warmup $WARMUP $extra_args" \
                        2>"$stderr_log"; then
                    echo "[FAIL] $sys $mode ic$ic runner returned non-zero" \
                        | tee -a "$OUT_DIR/skipped.log"
                    continue
                fi
                # Rewrite column 2 (`backend`) on every data row from
                # whatever ldbc_bench wrote (`lazy` or `disk`) to
                # `<label>`. Header passes through unchanged (NR==1
                # branch). awk rewrites in-place via a temp file.
                awk -F';' -v OFS=';' -v label="$label" \
                    'NR==1 {print; next} {$2 = label; print}' \
                    "$out_csv" > "$out_csv.tmp" \
                    && mv "$out_csv.tmp" "$out_csv"
            done
        else
            out_csv="$OUT_DIR/${sys}.ic${ic}.csv"
            stderr_log="$OUT_DIR/${sys}.ic${ic}.stderr.log"
            if ! eval "$runner_cmd $out_csv --ic $ic --iters $ITERS --warmup $WARMUP" \
                    2>"$stderr_log"; then
                echo "[FAIL] $sys ic$ic runner returned non-zero" \
                    | tee -a "$OUT_DIR/skipped.log"
            fi
        fi
    done

    echo ""
done

# ---- Concatenate ----------------------------------------------------

echo "--- Concatenating ---"
unified="$OUT_DIR/cross_system.csv"
header_written=0
produced=0
for csv in "$OUT_DIR"/*.csv; do
    [[ -f "$csv" ]] || continue
    base="$(basename "$csv")"
    # Skip the unified file itself if a previous run left one.
    [[ "$base" == "cross_system.csv" ]] && continue
    produced=$((produced + 1))
    if [[ $header_written -eq 0 ]]; then
        head -n 1 "$csv" > "$unified"
        header_written=1
    fi
    tail -n +2 "$csv" >> "$unified"
done

if [[ $produced -eq 0 ]]; then
    echo "  no per-system CSVs were produced — every runner failed or was skipped." >&2
    echo "  see $OUT_DIR/skipped.log" >&2
    echo ""
    echo "=== Done (with no results) — $OUT_DIR ==="
    exit 1
fi

echo "  unified: $unified ($(wc -l <"$unified") lines, $produced per-(sys,ic) CSVs)"

# ---- Setup-time summary ---------------------------------------------

echo ""
echo "--- Setup times ---"
column -t "$SETUP_TIMES_FILE" 2>/dev/null || cat "$SETUP_TIMES_FILE"

# ---- Comparison -----------------------------------------------------

echo ""
echo "--- Comparison ---"
if command -v python >/dev/null 2>&1; then
    # Pass the results dir as the second arg so compare_results.py can
    # also surface shape-verification failures from the per-system
    # *.stderr.log files. Without that, count-consistency passes can
    # mask per-column type mismatches (e.g. graphqlite returning
    # `"Person:933"` strings where the toml expected an int).
    python "$SCRIPT_DIR/compare_results.py" "$unified" "$OUT_DIR" \
        | tee "$OUT_DIR/comparison.txt"
else
    echo "  (python not on PATH; skipping comparison.py — run it manually:" >&2
    echo "   python $SCRIPT_DIR/compare_results.py $unified $OUT_DIR)" >&2
fi

END_EPOCH=$(date +%s)
echo "total_wall_seconds: $((END_EPOCH - START_EPOCH))" >> "$OUT_DIR/run_info.txt"

echo ""
echo "=== Done — results at $OUT_DIR ==="
