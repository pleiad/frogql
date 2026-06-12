#!/usr/bin/env bash
# Ingest-cost bench — measures CSV → ready-to-query time per system.
#
# Motivation (ISWC review R4.2): the real user workload is "I have a
# CSV dataset, I want to answer queries on it" — so the evaluation
# should price the ingest, not just the per-query latency. This script
# produces the ingest-cost table: for each system, a FRESH load of the
# full LDBC SF0.1 CSVs into its native format, wall-clocked, plus the
# resulting on-disk size.
#
# Every system is force-rebuilt (cached DBs are ignored) so the number
# is a true cold-ingest cost. gqlite's ingest is the same
# `frogql --import-ldbc-csv` path bench_setup uses, but written to a
# scratch file so the canonical bench/data/ldbc-sf0.1.gdb is untouched.
#
# Usage:
#   bench/cross-system/ingest_bench.sh [--only gqlite,kuzu,...] [--keep]
#
# Output: bench/cross-system/results/ingest_<timestamp>.csv
#   system;elapsed_seconds;db_bytes;status
# plus a pretty table on stdout. DuckDB (in-situ CSV baseline) has no
# ingest by design — its cost shows up per-query instead; see
# duckdb/README.md and the break-even analysis.
#
# Prereq: bench_setup has been run (CSVs present), cargo build --release,
# and the per-system Python deps are installed (install_python_deps.sh).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CSV_DIR="$REPO_ROOT/bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter"
SCRATCH_DIR="$REPO_ROOT/bench/data/cross-system/ingest-scratch"

ONLY=""
KEEP=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --only) ONLY="$2"; shift 2 ;;
        --keep) KEEP=1; shift ;;
        -h|--help)
            awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
            exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

if [[ ! -d "$CSV_DIR" ]]; then
    echo "missing LDBC CSVs at $CSV_DIR — run ./target/release/bench_setup first" >&2
    exit 1
fi

ALL_SYSTEMS=(gqlite graphqlite kuzu grafeo)
# neo4j is opt-in via --only: it needs a running server (docker) and
# its ingest goes over bolt, so it only makes sense when the server is up.
if [[ -n "$ONLY" ]]; then
    IFS=',' read -ra SYSTEMS <<<"$ONLY"
else
    SYSTEMS=("${ALL_SYSTEMS[@]}")
fi

mkdir -p "$SCRATCH_DIR" "$SCRIPT_DIR/results"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUT_CSV="$SCRIPT_DIR/results/ingest_${TIMESTAMP}.csv"
echo "system;elapsed_seconds;db_bytes;status" > "$OUT_CSV"

cd "$REPO_ROOT"

# Returns bytes of a file or directory ("—" if missing).
db_size_bytes() {
    local p="$1"
    [[ -e "$p" ]] || { echo "—"; return; }
    if [[ -d "$p" ]]; then
        if du -sb "$p" >/dev/null 2>&1; then
            du -sb "$p" | awk '{print $1}'
        else
            # macOS du has no -b; -k kilobytes × 1024.
            du -sk "$p" | awk '{print $1 * 1024}'
        fi
    else
        stat -c %s "$p" 2>/dev/null || stat -f %z "$p" 2>/dev/null || echo "—"
    fi
}

# now_ns: portable nanosecond-ish timestamp (macOS date has no %N).
now_ms() { python -c 'import time; print(int(time.time()*1000))'; }

run_one() {
    local sys="$1" cmd="$2" artifact="$3"
    echo "==================== $sys ===================="
    echo "  cmd: $cmd"
    local t0 t1 status="ok"
    t0=$(now_ms)
    # stdin from /dev/null: the frogql binary drops into its interactive
    # REPL after `--import-ldbc-csv` finishes (same as `bench_setup`,
    # which uses Stdio::null() for exactly this reason). Without the
    # redirect the REPL blocks on the terminal and the import "hangs"
    # after completing. Harmless for the Python loaders (they don't read
    # stdin).
    if ! eval "$cmd" < /dev/null > "$SCRIPT_DIR/results/ingest_${TIMESTAMP}.${sys}.log" 2>&1; then
        status="fail"
        echo "  [FAIL] see results/ingest_${TIMESTAMP}.${sys}.log"
    fi
    t1=$(now_ms)
    local elapsed bytes
    elapsed=$(awk "BEGIN{printf \"%.2f\", ($t1 - $t0)/1000}")
    bytes=$(db_size_bytes "$artifact")
    echo "  ${elapsed}s, ${bytes} bytes ($status)"
    echo "${sys};${elapsed};${bytes};${status}" >> "$OUT_CSV"
}

for sys in "${SYSTEMS[@]}"; do
    case "$sys" in
        gqlite)
            scratch_gdb="$SCRATCH_DIR/ldbc-sf01-ingest.gdb"
            rm -f "$scratch_gdb"
            if [[ ! -x ./target/release/frogql ]]; then
                cargo build --release --bin frogql >&2
            fi
            run_one gqlite \
                "./target/release/frogql '$scratch_gdb' --import-ldbc-csv '$CSV_DIR' --no-typecheck" \
                "$scratch_gdb"
            [[ $KEEP -eq 0 ]] && rm -f "$scratch_gdb"
            ;;
        graphqlite)
            run_one graphqlite \
                "python '$SCRIPT_DIR/graphqlite/setup.py' --force" \
                "$REPO_ROOT/bench/data/cross-system/graphqlite/ldbc-sf01.db"
            ;;
        kuzu)
            run_one kuzu \
                "python '$SCRIPT_DIR/kuzu/setup.py' --force" \
                "$REPO_ROOT/bench/data/cross-system/kuzu/ldbc-sf01.db"
            ;;
        grafeo)
            run_one grafeo \
                "python '$SCRIPT_DIR/grafeo/setup.py' --force" \
                "$REPO_ROOT/bench/data/cross-system/grafeo/ldbc-sf01.grafeo"
            ;;
        neo4j)
            run_one neo4j \
                "python '$SCRIPT_DIR/neo4j/setup.py' --force" \
                "—"
            ;;
        *)
            echo "[SKIP] unknown system: $sys" ;;
    esac
done

echo ""
echo "--- Ingest costs (CSV → ready-to-query, LDBC SF0.1) ---"
column -t -s';' "$OUT_CSV"
echo ""
echo "csv: $OUT_CSV"
