#!/usr/bin/env bash
# Install the Python dependencies for every Python-runner cross-system
# subdir in one go.
#
# Usage:
#   bash bench/cross-system/install_python_deps.sh           # everything
#   bash bench/cross-system/install_python_deps.sh kuzu      # one system
#   bash bench/cross-system/install_python_deps.sh kuzu graphqlite
#
# What it does: for each named subdir (or every subdir with a
# requirements.txt if no args are given), runs `pip install -r
# <subdir>/requirements.txt` against the active Python. No venv
# magic — installs into whichever Python is on PATH. If you want
# isolation, activate a venv first.
#
# Why this exists: each per-system subdir has its own
# requirements.txt because the systems' Python deps don't share
# anything (gqlitedb, kuzu, graphqlite — different wheels, different
# versions, no overlap). A new contributor coming to the bench
# shouldn't have to discover each subdir's requirements.txt
# separately; one command should set up everything.
#
# Cargo-based systems (gqlite, graphlite) aren't handled here —
# `cargo build --release` in the repo root + each system's own
# Cargo.toml handles those.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

if [[ $# -eq 0 ]]; then
    # Every subdir that has a requirements.txt
    mapfile -t subdirs < <(find "$SCRIPT_DIR" -mindepth 2 -maxdepth 2 \
        -name requirements.txt -printf '%h\n' | sort)
else
    subdirs=()
    for sys in "$@"; do
        subdirs+=("$SCRIPT_DIR/$sys")
    done
fi

if [[ ${#subdirs[@]} -eq 0 ]]; then
    echo "no per-system subdirs with requirements.txt found in $SCRIPT_DIR" >&2
    exit 1
fi

py=$(command -v python || command -v python3 || true)
if [[ -z "$py" ]]; then
    echo "no Python on PATH; install Python or activate a venv first" >&2
    exit 1
fi

echo "using Python: $py"
echo "$py --version: $($py --version 2>&1)"
echo ""

failed=()
for d in "${subdirs[@]}"; do
    req="$d/requirements.txt"
    if [[ ! -f "$req" ]]; then
        echo "[SKIP] $(basename "$d") — no requirements.txt"
        continue
    fi
    echo "--- $(basename "$d") ---"
    if "$py" -m pip install -r "$req"; then
        echo "[OK] $(basename "$d")"
    else
        echo "[FAIL] $(basename "$d")"
        failed+=("$(basename "$d")")
    fi
    echo ""
done

if [[ ${#failed[@]} -gt 0 ]]; then
    echo "Failed: ${failed[*]}" >&2
    exit 1
fi
echo "All Python deps installed."
