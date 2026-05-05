#!/usr/bin/env bash
# Cross-system bench — orchestrator.
#
# Invokes every per-system runner in turn for a chosen IC, captures
# each runner's CSV into a timestamped results dir, then runs
# compare_results.py to produce a side-by-side comparison table.
#
# Usage:
#   bench/cross-system/run_all.sh [--ic <n>] [--iters N] [--warmup N] [--only <list>]
#
# Default --ic is 2. Each invocation runs one IC against every
# (selected) system. For a multi-IC sweep, invoke this script once
# per IC — each run lands in its own timestamped dir.
#
# --only takes a comma-separated subset of system names, e.g.
#   --only gqlite        # just our system (debugging)
#   --only gqlite,graphqlite
# Default runs every system whose runner exists and is ready.
#
# Output: bench/cross-system/results/<timestamp>/
#   gqlite.csv
#   graphqlite.csv          (if integrated)
#   graphlite.csv           (if integrated)
#   auksys_gqlite.csv       (if integrated; gqlite.org / gqlitedb on PyPI)
#   webbery_gqlite.csv      (if integrated; SKIPPED.md otherwise)
#   cross_system.csv        (concatenation of all the above)
#   comparison.txt          (compare_results.py output)
#   run_info.txt            (metadata: timestamp, host, ic, etc.)
#
# Prereq: ./target/release/bench_setup has been run so the LDBC SF0.1
# dataset (~17 MiB) is downloaded and built into ldbc-sf0.1.gdb.
# Each external system's runner has its own additional setup
# requirements; see the corresponding subdir's README/run script.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RESULTS_ROOT="$SCRIPT_DIR/results"

IC=2
ITERS=10
WARMUP=2
ONLY=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --ic)     IC="$2"; shift 2 ;;
        --iters)  ITERS="$2"; shift 2 ;;
        --warmup) WARMUP="$2"; shift 2 ;;
        --only)   ONLY="$2"; shift 2 ;;
        -h|--help)
            awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
            exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUT_DIR="$RESULTS_ROOT/$TIMESTAMP"
mkdir -p "$OUT_DIR"
START_EPOCH=$(date +%s)

# Capture run-environment metadata up-front so any future re-analysis
# of the timestamped results dir knows which gqlite revision, host,
# and toolchain produced these numbers. Best-effort across platforms;
# missing fields surface as empty values, not errors.
{
    echo "timestamp:        $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "ic:               $IC"
    echo "host:             $(hostname 2>/dev/null || echo unknown)"
    echo "uname:            $(uname -a 2>/dev/null || echo unknown)"
    echo "gqlite_branch:    $(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
    echo "gqlite_commit:    $(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "gqlite_dirty:     $(git -C "$REPO_ROOT" diff --quiet 2>/dev/null && echo no || echo yes)"
    echo "rustc:            $(rustc --version 2>&1 || echo missing)"
    echo "python:           $(python --version 2>&1 || echo missing)"
    echo "ldbc_dataset:     SF0.1"
} > "$OUT_DIR/run_info.txt"

# Systems registered here. Order matters only for cosmetic per-system
# stderr output; the comparison script sorts independently.
ALL_SYSTEMS=(gqlite graphqlite graphlite auksys_gqlite webbery_gqlite)

# Filter via --only.
if [[ -n "$ONLY" ]]; then
    IFS=',' read -ra SYSTEMS <<<"$ONLY"
else
    SYSTEMS=("${ALL_SYSTEMS[@]}")
fi

cd "$REPO_ROOT"
echo "=== Cross-system IC$IC bench — $TIMESTAMP ==="
echo "  results: $OUT_DIR"
echo "  ic:      $IC"
echo "  iters:   $ITERS"
echo "  warmup:  $WARMUP"
echo "  systems: ${SYSTEMS[*]}"
echo ""

# Run each system. Missing/unimplemented runners are non-fatal; we
# log "SKIPPED" for them and continue. compare_results.py only
# considers CSVs that actually got produced.
for sys in "${SYSTEMS[@]}"; do
    runner=""
    case "$sys" in
        gqlite)         runner="$SCRIPT_DIR/gqlite/run.sh" ;;
        graphqlite)     runner="$SCRIPT_DIR/graphqlite/run.py" ;;
        graphlite)      runner="$SCRIPT_DIR/graphlite/run.sh" ;;  # wraps cargo run
        auksys_gqlite)  runner="$SCRIPT_DIR/auksys_gqlite/run.py" ;;
        webbery_gqlite) runner="$SCRIPT_DIR/webbery_gqlite/run.sh" ;;
    esac

    if [[ -z "$runner" || ! -e "$runner" ]]; then
        echo "[SKIP] $sys: runner not present at $runner" | tee -a "$OUT_DIR/skipped.log"
        continue
    fi

    echo "--- $sys ---"
    out_csv="$OUT_DIR/${sys}.csv"
    # Capture each runner's stderr to a per-system log. Runners emit
    # `SHAPE row=N count=N shape=<sig>` lines there, which we grep
    # for the shape-consistency summary at the end of this script.
    stderr_log="$OUT_DIR/${sys}.stderr.log"

    case "$sys" in
        graphqlite|auksys_gqlite)
                    python "$runner" "$out_csv" --ic "$IC" --iters "$ITERS" --warmup "$WARMUP" \
                        2>"$stderr_log" || \
            echo "[FAIL] $sys runner returned non-zero" | tee -a "$OUT_DIR/skipped.log" ;;
        *)          bash "$runner" "$out_csv" --ic "$IC" --iters "$ITERS" --warmup "$WARMUP" \
                        2>"$stderr_log" || \
            echo "[FAIL] $sys runner returned non-zero" | tee -a "$OUT_DIR/skipped.log" ;;
    esac
done

# Concatenate every produced per-system CSV (skipping per-system
# header lines so the unified file has exactly one header).
echo ""
echo "--- Concatenating ---"
unified="$OUT_DIR/cross_system.csv"
header_written=0
produced=0
for sys in "${SYSTEMS[@]}"; do
    csv="$OUT_DIR/${sys}.csv"
    [[ -f "$csv" ]] || continue
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

echo "  unified: $unified ($(wc -l <"$unified") lines, $produced systems)"

# Shape-verification summary. Each runner logs `SHAPE row=N
# count=N shape=<sig> status=<ok|fail|no-expected>` per params row;
# verification is independent per runner against the
# `expected_shape` field in the IC's toml. We just tally pass/fail
# counts here. A failure is a per-system query-translation bug; the
# stderr log has the per-row reason.
echo ""
echo "--- Shape verification ---"
for sys in "${SYSTEMS[@]}"; do
    log="$OUT_DIR/${sys}.stderr.log"
    [[ -f "$log" ]] || continue
    total=$(grep -c "^  SHAPE row=" "$log" || true)
    ok=$(grep -c "^  SHAPE row=.* status=ok$" "$log" || true)
    if [[ "$total" -eq 0 ]]; then
        echo "  $sys: no SHAPE lines (runner emitted none)"
        continue
    fi
    if [[ "$ok" -eq "$total" ]]; then
        echo "  $sys: $ok/$total rows passed shape verification"
    else
        echo "  $sys: $ok/$total rows passed; $((total - ok)) failed (see $log)"
    fi
done

# Run comparison.
echo ""
echo "--- Comparison ---"
if command -v python >/dev/null 2>&1; then
    python "$SCRIPT_DIR/compare_results.py" "$unified" \
        | tee "$OUT_DIR/comparison.txt"
else
    echo "  (python not on PATH; skipping comparison.py — run it manually:" >&2
    echo "   python $SCRIPT_DIR/compare_results.py $unified)" >&2
fi

END_EPOCH=$(date +%s)
echo "total_wall_seconds: $((END_EPOCH - START_EPOCH))" >> "$OUT_DIR/run_info.txt"

echo ""
echo "=== Done — results at $OUT_DIR ==="
