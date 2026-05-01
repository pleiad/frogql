#!/usr/bin/env bash
# Run our IC2 (gqlite, three GraphAccess backends) and emit per-iter
# CSV in the cross-system schema.
#
# Output schema (matches src/bin/ldbc_bench.rs at line 665):
#   query;backend;params;row;iter;result_count;elapsed_ns
#
# Each backend produces the same 15 params × N iters rows; backend
# column distinguishes them. compare_results.py uses (params, row)
# as the join key across systems.
#
# Usage:
#   bench/cross-system/gqlite/run.sh <out_csv> [--iters N] [--warmup N]
#
# Prereq: ./target/release/bench_setup has been run, so
# bench/data/ldbc-sf0.1.gdb and bench/data/ldbc-sf0.1/...CSVs exist.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 <out_csv> [--iters N] [--warmup N]" >&2
    exit 1
fi

OUT_CSV="$1"; shift

# Forwarded flags. The row cap is baked into ic2.toml's query
# (LIMIT 20, spec-faithful) so we don't pass one here.
ITERS=10
WARMUP=2
while [[ $# -gt 0 ]]; do
    case "$1" in
        --iters)  ITERS="$2"; shift 2 ;;
        --warmup) WARMUP="$2"; shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

# Resolve repo root from this script's location, so the script works
# regardless of where the orchestrator runs it from.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
cd "$REPO_ROOT"

GDB="bench/data/ldbc-sf0.1.gdb"

if [[ ! -f "$GDB" ]]; then
    echo "missing $GDB — run ./target/release/bench_setup first" >&2
    exit 1
fi

# Build the binary if it isn't already.
if [[ ! -x ./target/release/ldbc_bench ]]; then
    cargo build --release --bin ldbc_bench >&2
fi

# One backend: `lazy` (LazyGraphStore — topology in RAM, labels/props
# from disk via LRU page cache). Memory and Disk are dropped: memory
# rebuilds from CSV every startup (not a persistence comparison), and
# disk loads more into RAM than lazy. Lazy is gqlite's default mode
# and the only one the cross-system claim needs.
echo "  --- gqlite/lazy ---" >&2
./target/release/ldbc_bench "$GDB" \
    --ic 2 --backend lazy \
    --iters "$ITERS" --warmup "$WARMUP" \
    > "$OUT_CSV"

echo "  done -> $OUT_CSV" >&2
