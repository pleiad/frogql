#!/usr/bin/env bash
# Download and extract the soc-LiveJournal1 dataset from SNAP.
# Output: bench/data/soc-LiveJournal1.txt (tab-separated edge list)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DATA_DIR="$SCRIPT_DIR/../data"
URL="https://snap.stanford.edu/data/soc-LiveJournal1.txt.gz"
GZ_FILE="$DATA_DIR/soc-LiveJournal1.txt.gz"
OUT_FILE="$DATA_DIR/soc-LiveJournal1.txt"

mkdir -p "$DATA_DIR"

if [ -f "$OUT_FILE" ]; then
    echo "Dataset already exists at $OUT_FILE"
    echo "$(wc -l < "$OUT_FILE") lines"
    exit 0
fi

echo "Downloading soc-LiveJournal1 from SNAP..."
curl -L -o "$GZ_FILE" "$URL"

echo "Extracting..."
gunzip "$GZ_FILE"

echo "Done. Dataset at $OUT_FILE"
echo "$(wc -l < "$OUT_FILE") lines"
