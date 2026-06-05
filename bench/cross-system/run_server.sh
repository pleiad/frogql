#!/usr/bin/env bash
# Server driver for the cross-system bench: frogQL (gqlite) vs Kuzu vs
# Grafeo, on EVERY IC frogQL implements, measuring both LATENCY and
# PEAK MEMORY, with a hard 10 GiB per-runner cap (a query that exceeds
# it is killed and recorded as a memory_error rather than taking the
# box down).
#
# This is the "run it on a separate Linux server" entry point. It:
#   1. verifies / builds the gqlite release binaries (frogql,
#      ldbc_bench, bench_setup — bench_setup shells out to frogql to
#      import the CSVs into the .gdb);
#   2. downloads + builds the LDBC SF0.1 dataset (bench_setup) if the
#      .gdb / CSVs are missing — Linux only, no Windows UAC issue;
#   3. installs the Python deps for the external engines if asked;
#   4. hands off to run_all.sh, which loads each external engine's
#      native DB once and runs every requested IC under the memory
#      monitor (see _lib/memrun.py);
#   5. tees all output to results/<timestamp>.server.log.
#
# Defaults are tuned for an unattended server run. Everything after the
# recognized flags is passed straight through to run_all.sh.
#
# Usage (typical, unattended):
#   nohup bench/cross-system/run_server.sh --install-deps \
#       > /tmp/frogql-bench.out 2>&1 &
#   tail -f /tmp/frogql-bench.out
#
# Flags (server-driver-specific; all others pass through to run_all.sh):
#   --systems a,b,c     systems to bench   (default: gqlite,kuzu,grafeo)
#   --ics 1,2,3         IC subset          (default: all implemented)
#   --iters N           measured iters     (default: 10)
#   --warmup N          warmup iters       (default: 2)
#   --mem-limit-gb G    per-runner RSS cap (default: 10)
#   --timeout-s N       per-(system,IC) wall-clock cap (default: 600;
#                       0 = none). A hung/runaway external query is killed
#                       and recorded as a timeout instead of stalling the run.
#   --install-deps      pip-install the external engines first
#                       (bench/cross-system/install_python_deps.sh)
#   --rebuild-data      force re-download/rebuild of the LDBC dataset
#   --rebuild-setup     force re-load of each external engine's DB
#                       (passed through to run_all.sh)
#   -h | --help         show this help
#
# Prereqs the script will NOT do for you:
#   - a working Rust toolchain (`cargo`) on PATH;
#   - a Python ≥ 3.11 on PATH as `python` (tomllib is stdlib there);
#   - enough disk for the LDBC SF0.1 dataset + 3 native DB copies.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

SYSTEMS="gqlite,kuzu,grafeo"
ICS=""                 # empty => let run_all.sh use its all-implemented default
ITERS=10
WARMUP=2
MEM_LIMIT_GB=10
TIMEOUT_S=600          # per-(system,IC) wall-clock cap. Bounds a hung/runaway
                       # external query (a single IC sweep) so the whole run
                       # can't stall for hours. 0 disables it.
INSTALL_DEPS=0
REBUILD_DATA=0
PASSTHROUGH=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --systems)       SYSTEMS="$2"; shift 2 ;;
        --ics|--ic)      ICS="$2"; shift 2 ;;
        --iters)         ITERS="$2"; shift 2 ;;
        --warmup)        WARMUP="$2"; shift 2 ;;
        --mem-limit-gb)  MEM_LIMIT_GB="$2"; shift 2 ;;
        --timeout-s)     TIMEOUT_S="$2"; shift 2 ;;
        --install-deps)  INSTALL_DEPS=1; shift ;;
        --rebuild-data)  REBUILD_DATA=1; shift ;;
        -h|--help)
            awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
            exit 0 ;;
        # Anything else (e.g. --rebuild-setup, --ablate, --sample-ms)
        # is forwarded verbatim to run_all.sh.
        *) PASSTHROUGH+=("$1"); shift ;;
    esac
done

cd "$REPO_ROOT"

GDB="bench/data/ldbc-sf0.1.gdb"
CSV_DIR="bench/data/ldbc-sf0.1"

log() { printf '\n=== %s ===\n' "$*"; }

# --- Tooling check ---------------------------------------------------
log "Environment"
command -v cargo  >/dev/null 2>&1 || { echo "FATAL: cargo not on PATH" >&2; exit 1; }
command -v python >/dev/null 2>&1 || { echo "FATAL: python not on PATH" >&2; exit 1; }
echo "host:   $(hostname 2>/dev/null || echo unknown)"
echo "uname:  $(uname -a 2>/dev/null || echo unknown)"
echo "cargo:  $(cargo --version 2>&1)"
echo "python: $(python --version 2>&1)"
echo "branch: $(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
echo "commit: $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
# Total RAM sanity-check vs the cap: a 10 GiB/runner cap on an 8 GiB box
# would have the kernel OOM-kill before memrun samples. Warn, don't fail.
if [[ -r /proc/meminfo ]]; then
    mem_kib=$(awk '/^MemTotal:/{print $2}' /proc/meminfo)
    mem_gib=$(awk "BEGIN{printf \"%.1f\", $mem_kib/1048576}")
    echo "ram:    ${mem_gib} GiB total"
    if awk "BEGIN{exit !($mem_kib/1048576 < $MEM_LIMIT_GB + 2)}"; then
        echo "  WARNING: total RAM (${mem_gib} GiB) is close to the" >&2
        echo "  ${MEM_LIMIT_GB} GiB/runner cap; the kernel may OOM-kill a" >&2
        echo "  runner before memrun's cap trips. Lower --mem-limit-gb." >&2
    fi
fi

# --- Python deps (optional) ------------------------------------------
if [[ $INSTALL_DEPS -eq 1 ]]; then
    log "Installing external-engine Python deps"
    bash "$SCRIPT_DIR/install_python_deps.sh"
fi

# --- gqlite release binaries -----------------------------------------
log "Building gqlite release binaries"
cargo build --release --bin frogql --bin ldbc_bench --bin bench_setup
echo "  frogql + ldbc_bench + bench_setup built."

# --- LDBC dataset ----------------------------------------------------
if [[ $REBUILD_DATA -eq 1 || ! -f "$GDB" || ! -d "$CSV_DIR" ]]; then
    log "Building LDBC SF0.1 dataset (bench_setup)"
    if [[ $REBUILD_DATA -eq 1 ]]; then
        ./target/release/bench_setup --rebuild
    else
        ./target/release/bench_setup
    fi
else
    log "LDBC dataset present"
    echo "  $GDB and $CSV_DIR exist (pass --rebuild-data to force)."
fi

# --- Run -------------------------------------------------------------
TS=$(date +%Y%m%d_%H%M%S)
SERVER_LOG="$SCRIPT_DIR/results/${TS}.server.log"
mkdir -p "$SCRIPT_DIR/results"

RUN_ARGS=(--only "$SYSTEMS" --iters "$ITERS" --warmup "$WARMUP"
          --mem-limit-gb "$MEM_LIMIT_GB" --timeout-s "$TIMEOUT_S")
[[ -n "$ICS" ]] && RUN_ARGS+=(--ics "$ICS")
RUN_ARGS+=("${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}")

log "Launching run_all.sh"
echo "  systems:   $SYSTEMS"
echo "  ics:       ${ICS:-<all implemented>}"
echo "  iters:     $ITERS (warmup $WARMUP)"
echo "  mem cap:   ${MEM_LIMIT_GB} GiB/runner"
echo "  time cap:  ${TIMEOUT_S}s/runner (0 = none)"
echo "  log:       $SERVER_LOG"
echo ""

# Tee the whole run so an unattended server run leaves a durable record
# next to the timestamped results dir run_all.sh creates.
bash "$SCRIPT_DIR/run_all.sh" "${RUN_ARGS[@]}" 2>&1 | tee "$SERVER_LOG"

log "Done"
echo "  server log: $SERVER_LOG"
echo "  results:    newest dir under $SCRIPT_DIR/results/"
echo "  memory:     <results>/memory.csv   (peak RSS + status per system+IC)"
echo "  latency:    <results>/comparison.txt"
