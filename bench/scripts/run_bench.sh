#!/usr/bin/env bash
# Benchmark orchestration script for gqlrust on soc-LiveJournal1.
#
# Usage:
#   ./bench/scripts/run_bench.sh [--limit-edges N] [--query-limit N] [--timeout SECS]
#
# Steps:
#   1. Download dataset (if not present)
#   2. Convert to .gql format
#   3. Generate queries
#   4. Run all shape queries
#   5. Collect results
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_DIR="$SCRIPT_DIR/.."
PROJECT_DIR="$BENCH_DIR/.."
DATA_DIR="$BENCH_DIR/data"
QUERIES_DIR="$BENCH_DIR/queries"
RESULTS_DIR="$BENCH_DIR/results"

# Defaults
EDGE_LIMIT=""
QUERY_LIMIT=0        # 0 = unlimited
TIMEOUT=600          # seconds

# Parse args
while [[ $# -gt 0 ]]; do
    case "$1" in
        --limit-edges)  EDGE_LIMIT="--limit $2"; shift 2 ;;
        --query-limit)  QUERY_LIMIT="$2"; shift 2 ;;
        --timeout)      TIMEOUT="$2"; shift 2 ;;
        *)              echo "Unknown arg: $1"; exit 1 ;;
    esac
done

DATASET="$DATA_DIR/soc-LiveJournal1.txt"
DB_FILE="$DATA_DIR/soc-LiveJournal1.gql"

mkdir -p "$RESULTS_DIR"

# --- Step 1: Download ---
echo "=== Step 1: Download dataset ==="
bash "$SCRIPT_DIR/download_livejournal.sh"

# --- Step 2: Build release binaries ---
echo ""
echo "=== Step 2: Build release binaries ==="
cd "$PROJECT_DIR"
cargo build --release --bin convert_edgelist --bin bench_queries 2>&1 | tail -2

CONVERT="$PROJECT_DIR/target/release/convert_edgelist"
BENCH="$PROJECT_DIR/target/release/bench_queries"

# --- Step 3: Convert to .gql ---
echo ""
echo "=== Step 3: Convert dataset to .gql ==="
if [ -f "$DB_FILE" ]; then
    echo "Database already exists: $DB_FILE"
    echo "$(ls -lh "$DB_FILE" | awk '{print $5}') size"
else
    $CONVERT "$DATASET" "$DB_FILE" $EDGE_LIMIT
fi

# --- Step 4: Generate queries ---
echo ""
echo "=== Step 4: Generate queries ==="
python3 "$SCRIPT_DIR/generate_queries.py" "$QUERIES_DIR"

# --- Step 5: Run benchmarks ---
echo ""
echo "=== Step 5: Run benchmarks ==="

SHAPES="2-comb 3-path 4-path 1-tree 2-tree 3-cycle 4-cycle 3-clique 4-clique 2-3-lollipop 3-4-lollipop"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
SUMMARY_FILE="$RESULTS_DIR/summary_${TIMESTAMP}.csv"
echo "shape;query_id;result_count;elapsed_ns" > "$SUMMARY_FILE"

for shape in $SHAPES; do
    QUERY_FILE="$QUERIES_DIR/${shape}.gql"
    RESULT_FILE="$RESULTS_DIR/${shape}_${TIMESTAMP}.txt"

    if [ ! -f "$QUERY_FILE" ]; then
        echo "  SKIP: $shape (no query file)"
        continue
    fi

    echo "  Running $shape..."
    $BENCH "$DB_FILE" "$QUERY_FILE" --limit $QUERY_LIMIT --timeout $TIMEOUT \
        > "$RESULT_FILE" 2>"$RESULTS_DIR/${shape}_${TIMESTAMP}.log"

    # Append to summary with shape prefix
    while IFS= read -r line; do
        echo "${shape};${line}" >> "$SUMMARY_FILE"
    done < "$RESULT_FILE"
done

echo ""
echo "=== Results ==="
echo "Summary: $SUMMARY_FILE"
cat "$SUMMARY_FILE"
echo ""
echo "Done."
