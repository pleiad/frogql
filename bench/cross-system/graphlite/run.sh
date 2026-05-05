#!/usr/bin/env bash
# Run a chosen IC against GraphLite-AI/GraphLite and emit per-iter CSV
# in the cross-system schema.
#
# Output schema (matches src/bin/ldbc_bench.rs):
#   query;backend;params;row;iter;result_count;elapsed_ns
#
# Usage:
#   bench/cross-system/graphlite/run.sh <out_csv> [--ic <n>] [--iters N] [--warmup N]
#
# Default --ic is 2. The runner reads the IC's toml + the local
# ic<n>.gql translation file, opens the pre-loaded Sled DB at
# bench/data/cross-system/graphlite/ic<n>.db/, and runs the query for
# each substitution-param row.
#
# Prereq: ./target/release/bench_setup has been run, AND `cargo run
# --release --bin graphlite-setup -- --ic <n>` has loaded the data
# subset for this IC into the Sled DB. The runner errors out cleanly
# if the DB is missing.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <out_csv> [--ic <n>] [--iters N] [--warmup N]" >&2
    exit 1
fi

OUT_CSV="$1"; shift

IC=2
ITERS=10
WARMUP=2
while [[ $# -gt 0 ]]; do
    case "$1" in
        --ic)     IC="$2"; shift 2 ;;
        --iters)  ITERS="$2"; shift 2 ;;
        --warmup) WARMUP="$2"; shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

# GraphLite is included with documented divergences — see DIVERGENCES.md.
# We pass an early heads-up so the operator knows what they're looking
# at; the runner itself counts errored param rows in CSV via sentinel
# (result_count = -1) and reports them in stderr. The orchestrator's
# SKIPPED.md detection is intentionally NOT used here — we want the
# runner to proceed and let compare_results.py surface the error count
# alongside per-row latency.
if [[ -f "$SCRIPT_DIR/DIVERGENCES.md" ]]; then
    echo "  graphlite: running with documented divergences (see $SCRIPT_DIR/DIVERGENCES.md)" >&2
fi

# Convert the OUT_CSV to absolute path before changing dirs (OUT_CSV
# may be relative to the orchestrator's CWD).
case "$OUT_CSV" in
    /*) ABS_OUT="$OUT_CSV" ;;
    *)  ABS_OUT="$(pwd)/$OUT_CSV" ;;
esac

cd "$SCRIPT_DIR"

# Build release binary if missing. Standalone Cargo.toml — its target
# dir is local to the graphlite/ subdir, NOT the repo's main target/.
if [[ ! -x ./target/release/graphlite-run ]] \
   || [[ ! -x ./target/release/graphlite-setup ]]; then
    cargo build --release --bin graphlite-run --bin graphlite-setup >&2
fi

DB_DIR="$REPO_ROOT/bench/data/cross-system/graphlite/ic${IC}.db"
if [[ ! -d "$DB_DIR" ]]; then
    echo "  graphlite db missing: $DB_DIR" >&2
    echo "  building (one-time, ~minutes)..." >&2
    ./target/release/graphlite-setup --ic "$IC" >&2 || {
        echo "  graphlite-setup failed; see stderr above" >&2
        exit 1
    }
fi

echo "  --- graphlite ic$IC ---" >&2
./target/release/graphlite-run "$ABS_OUT" \
    --ic "$IC" --iters "$ITERS" --warmup "$WARMUP"

echo "  done -> $ABS_OUT" >&2
