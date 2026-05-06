#!/usr/bin/env bash
# Run a chosen IC against gqlite (lazy or disk backend) and emit
# per-iter CSV in the cross-system schema.
#
# Output schema (matches src/bin/ldbc_bench.rs):
#   query;backend;params;row;iter;result_count;elapsed_ns
#
# Usage:
#   bench/cross-system/gqlite/run.sh <out_csv> [--ic <n>] [--iters N] [--warmup N] [--backend lazy|disk]
#
# Default --ic is 2, --backend is lazy. ldbc_bench reads
# bench/ldbc-queries/ic<n>.toml and skips ICs whose status is not
# "implemented".
#
# Prereq: ./target/release/bench_setup has been run, so
# bench/data/ldbc-sf0.1.gdb and bench/data/ldbc-sf0.1/...CSVs exist.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <out_csv> [--ic <n>] [--iters N] [--warmup N]" >&2
    exit 1
fi

OUT_CSV="$1"; shift

IC=2
ITERS=10
WARMUP=2
BACKEND=lazy
while [[ $# -gt 0 ]]; do
    case "$1" in
        --ic)      IC="$2"; shift 2 ;;
        --iters)   ITERS="$2"; shift 2 ;;
        --warmup)  WARMUP="$2"; shift 2 ;;
        --backend) BACKEND="$2"; shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
cd "$REPO_ROOT"

GDB="bench/data/ldbc-sf0.1.gdb"

if [[ ! -f "$GDB" ]]; then
    echo "missing $GDB — run ./target/release/bench_setup first" >&2
    exit 1
fi

if [[ ! -x ./target/release/ldbc_bench ]]; then
    cargo build --release --bin ldbc_bench >&2
fi

echo "  --- gqlite/$BACKEND ic$IC ---" >&2
# Row-equivalence dump: ldbc_bench appends a per-(params_row) envelope
# to this JSONL so compare_results.py can diff actual rows when hashes
# disagree. Sibling of the CSV; matches the Python runners' shape.
ROWS_JSONL="${OUT_CSV%.csv}.rows.jsonl"
rm -f "$ROWS_JSONL"  # fresh per run
GQLITE_BENCH_ROWS_JSONL="$ROWS_JSONL" ./target/release/ldbc_bench "$GDB" \
    --ic "$IC" --backend "$BACKEND" \
    --iters "$ITERS" --warmup "$WARMUP" \
    > "$OUT_CSV"

echo "  done -> $OUT_CSV" >&2
