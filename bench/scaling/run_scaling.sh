#!/usr/bin/env bash
# Scaling sweep — froGQL latency / open-time / RSS across LDBC scale
# factors, lazy vs disk backend.
#
# Motivation (ISWC review R2.6): "it is not clear how well the engine
# scales for large-scale graphs beyond ~5M edges". This sweep runs a
# representative IC subset at increasing scale factors and records,
# per (SF, backend):
#   - per-iter query latency        (ldbc_bench CSV)
#   - open-time phase breakdown     (FROGQL_TRACE_OPEN=1 stderr)
#   - peak process RSS              (memrun process-group sampler;
#                                    Linux only — reports -1 on macOS)
#
# Scale factors: SF0.1 (327K nodes / 1.5M edges) is the cross-system
# baseline. SF3 / SF10 have official LDBC substitution params at the
# canonical URL. SF1 has a dataset but NO published params — skip it
# (or supply your own via ldbc_bench --params-dir).
#
# Usage:
#   bench/scaling/run_scaling.sh [--sfs 0.1,3] [--ics 1,2,6,9,13]
#       [--iters 5] [--warmup 1] [--backends lazy,disk]
#       [--mem-limit-gb 28] [--skip-setup]
#
# Each SF must have been fetched+built by bench_setup first unless the
# script does it for you (it invokes bench_setup per SF when the .gdb
# is missing; pass --skip-setup to forbid that, e.g. on an offline box).
#
# Output: bench/scaling/results/<timestamp>/
#   sf<SF>.<backend>.csv          per-iter latency rows (ldbc_bench schema)
#   sf<SF>.<backend>.stderr.log   ldbc_bench stderr incl. open trace
#   sf<SF>.<backend>.open.txt     extracted FROGQL_TRACE_OPEN phase lines
#   sf<SF>.<backend>.mem.txt      memrun peak-RSS summary
#   run_info.txt                  host, commit, args, machine specs
#
# Default ICs: 1 (BFS shortest), 2 (index-fold + ORDER BY top-k),
# 6 (multi-hop join), 9 (large scan + sort), 13 (pair shortest path).
# One heap-heavy + one index-driven + two join shapes + one path shape
# — enough to expose how each cost class scales.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
XSYS_LIB="$REPO_ROOT/bench/cross-system/_lib"

SFS_ARG="0.1,3"
ICS_ARG="1,2,6,9,13"
ITERS=5
WARMUP=1
BACKENDS_ARG="lazy,disk"
MEM_LIMIT_GB=28
SKIP_SETUP=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --sfs)          SFS_ARG="$2"; shift 2 ;;
        --ics|--ic)     ICS_ARG="$2"; shift 2 ;;
        --iters)        ITERS="$2"; shift 2 ;;
        --warmup)       WARMUP="$2"; shift 2 ;;
        --backends)     BACKENDS_ARG="$2"; shift 2 ;;
        --mem-limit-gb) MEM_LIMIT_GB="$2"; shift 2 ;;
        --skip-setup)   SKIP_SETUP=1; shift ;;
        -h|--help)
            awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
            exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

IFS=',' read -ra SF_LIST <<<"$SFS_ARG"
IFS=',' read -ra BACKEND_LIST <<<"$BACKENDS_ARG"

cd "$REPO_ROOT"

for bin in ldbc_bench bench_setup; do
    if [[ ! -x "./target/release/$bin" ]]; then
        echo "building $bin..." >&2
        cargo build --release --bin "$bin" --features bench >&2
    fi
done

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
OUT_DIR="$SCRIPT_DIR/results/$TIMESTAMP"
mkdir -p "$OUT_DIR"

# Machine specs — same capture as cross-system run_all.sh.
if [[ "$(uname -s)" == "Darwin" ]]; then
    CPU_MODEL=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo unknown)
    CPU_CORES=$(sysctl -n hw.ncpu 2>/dev/null || echo unknown)
    RAM_GB=$(awk "BEGIN{printf \"%.1f\", $(sysctl -n hw.memsize 2>/dev/null || echo 0)/1073741824}")
else
    CPU_MODEL=$(awk -F: '/model name/{sub(/^ /,"",$2); print $2; exit}' /proc/cpuinfo 2>/dev/null || echo unknown)
    CPU_CORES=$(nproc 2>/dev/null || echo unknown)
    RAM_GB=$(awk '/MemTotal/{printf "%.1f", $2/1048576}' /proc/meminfo 2>/dev/null || echo unknown)
fi

{
    echo "timestamp:     $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "sfs:           ${SF_LIST[*]}"
    echo "ics:           $ICS_ARG"
    echo "iters:         $ITERS"
    echo "warmup:        $WARMUP"
    echo "backends:      ${BACKEND_LIST[*]}"
    echo "mem_limit_gb:  $MEM_LIMIT_GB"
    echo "host:          $(hostname 2>/dev/null || echo unknown)"
    echo "uname:         $(uname -a 2>/dev/null || echo unknown)"
    echo "cpu_model:     $CPU_MODEL"
    echo "cpu_cores:     $CPU_CORES"
    echo "ram_gb:        $RAM_GB"
    echo "commit:        $(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
} > "$OUT_DIR/run_info.txt"

MEMRUN=(python "$XSYS_LIB/memrun.py" --limit-gb "$MEM_LIMIT_GB" --interval 0.05)

for sf in "${SF_LIST[@]}"; do
    GDB="bench/data/ldbc-sf${sf}.gdb"
    PARAMS="bench/data/substitution_parameters-sf${sf}/substitution_parameters-sf${sf}"

    if [[ ! -f "$GDB" ]]; then
        if [[ $SKIP_SETUP -eq 1 ]]; then
            echo "[SKIP] sf$sf: $GDB missing and --skip-setup set" >&2
            continue
        fi
        echo "=== sf$sf: fetching + building via bench_setup (may download GBs) ==="
        ./target/release/bench_setup --sf "$sf"
    fi
    if [[ ! -d "$PARAMS" ]]; then
        echo "[SKIP] sf$sf: no substitution params at $PARAMS." >&2
        echo "       LDBC publishes params for 0.1/3/10/30/...; sf1 has none." >&2
        continue
    fi

    for be in "${BACKEND_LIST[@]}"; do
        base="sf${sf}.${be}"
        echo "==================== sf$sf / $be ===================="
        out_csv="$OUT_DIR/${base}.csv"
        stderr_log="$OUT_DIR/${base}.stderr.log"
        mem_out="$OUT_DIR/${base}.mem.txt"
        # FROGQL_TRACE_OPEN gives the per-phase open breakdown on stderr;
        # rows JSONL enables row-equivalence diffing across SFs/backends.
        if ! FROGQL_TRACE_OPEN=1 \
             FROGQL_BENCH_ROWS_JSONL="$OUT_DIR/${base}.rows.jsonl" \
             "${MEMRUN[@]}" --peak-out "$mem_out" --label "$base" -- \
             ./target/release/ldbc_bench "$GDB" \
                 --ic "$ICS_ARG" --backend "$be" \
                 --iters "$ITERS" --warmup "$WARMUP" \
                 --params-dir "$PARAMS" \
                 > "$out_csv" 2>"$stderr_log"; then
            echo "[FAIL] $base — see $stderr_log" | tee -a "$OUT_DIR/skipped.log"
            continue
        fi
        grep -E "pager open|string table|topology|catalog load|secondary index|^  loaded|RSS after open|TripleIndex built" \
            "$stderr_log" | head -20 > "$OUT_DIR/${base}.open.txt" || true
        echo "  rows: $(($(wc -l <"$out_csv") - 1))  peak: $(awk -F= '/^peak_rss_mib=/{print $2}' "$mem_out" 2>/dev/null || echo '?') MiB"
    done
done

echo ""
echo "=== Done — results at $OUT_DIR ==="
echo "Per-(SF, backend) latency CSVs + open traces + peak RSS are in that dir."
