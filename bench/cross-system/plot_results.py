#!/usr/bin/env python3
"""Plot cross-system bench results into grouped bar charts.

Reads a `cross_system.csv` (the unified per-iter file `run_all.sh`
writes, schema `query;backend;params;row;iter;result_count;elapsed_ns`)
and renders median per-IC latency, grouped by system. Writes both PNG
(for quick viewing) and SVG (crisp for the website / paper).

Usage:
    python plot_results.py [results/<timestamp>/cross_system.csv] \
                           [--out bench/cross-system/results/latency] \
                           [--log]

With no CSV arg it picks the newest results/<timestamp>/cross_system.csv.
`--log` uses a log y-axis (useful when ICs span orders of magnitude).

Requires matplotlib + pandas:
    pip install matplotlib pandas
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

try:
    import pandas as pd
    import matplotlib
    matplotlib.use("Agg")  # headless
    import matplotlib.pyplot as plt
except ImportError as e:
    sys.stderr.write(f"missing dep ({e}). Install:\n  pip install matplotlib pandas\n")
    sys.exit(1)

HERE = Path(__file__).resolve().parent

# Friendly display names + a stable colour per system. Backend labels
# come from the CSV's `backend` column (set by each runner / ldbc_bench).
SYSTEM_LABEL = {
    "lazy": "froGQL",
    "lazy-baseline": "froGQL",
    "lazy-no-fold": "froGQL (no-fold)",
    "disk": "froGQL (disk)",
    "memory": "froGQL (mem)",
    "graphqlite-cypher": "GraphQLite",
    "kuzu-cypher": "Kuzu",
    "grafeo-gql": "Grafeo",
}
SYSTEM_COLOR = {
    "froGQL": "#2c8a3e",
    "froGQL (no-fold)": "#7fc08c",
    "froGQL (disk)": "#1f5e2b",
    "froGQL (mem)": "#56a86a",
    "GraphQLite": "#3b6ea5",
    "Kuzu": "#c2792e",
    "Grafeo": "#9b59b6",
}


def latest_csv() -> Path:
    results = HERE / "results"
    cands = sorted(results.glob("*/cross_system.csv"))
    if not cands:
        sys.stderr.write(
            f"no results found under {results}/. Run run_all.sh first, or pass a CSV path.\n"
        )
        sys.exit(1)
    return cands[-1]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("csv", nargs="?", type=Path, default=None)
    ap.add_argument("--out", type=Path, default=None,
                    help="output path prefix (writes <prefix>.png and .svg)")
    ap.add_argument("--log", action="store_true", help="log y-axis")
    args = ap.parse_args()

    csv_path = args.csv or latest_csv()
    if not csv_path.is_file():
        sys.stderr.write(f"not found: {csv_path}\n")
        return 1
    out_prefix = args.out or (csv_path.parent / "latency")

    df = pd.read_csv(csv_path, sep=";")
    df["ms"] = df["elapsed_ns"] / 1e6
    df["system"] = df["backend"].map(lambda b: SYSTEM_LABEL.get(b, b))

    # Median latency per (IC, system) across all param rows × iters.
    med = (df.groupby(["query", "system"])["ms"].median()
             .unstack("system"))
    # Order ICs numerically (IC2, IC5, ...) and systems by a stable order.
    med = med.reindex(sorted(med.index, key=lambda q: int(q.replace("IC", ""))))
    systems = [s for s in SYSTEM_COLOR if s in med.columns] + \
              [s for s in med.columns if s not in SYSTEM_COLOR]

    ics = list(med.index)
    n_sys = len(systems)
    bar_w = 0.8 / max(n_sys, 1)

    fig, ax = plt.subplots(figsize=(max(7, 1.6 * len(ics) + 2), 4.8))
    for i, sysname in enumerate(systems):
        xs = [j + (i - (n_sys - 1) / 2) * bar_w for j in range(len(ics))]
        ys = [med.loc[ic, sysname] if sysname in med.columns else 0 for ic in ics]
        bars = ax.bar(xs, ys, width=bar_w, label=sysname,
                      color=SYSTEM_COLOR.get(sysname, "#888"))
        for x, y in zip(xs, ys):
            if y and y == y:  # not NaN
                ax.annotate(f"{y:.1f}", (x, y), ha="center", va="bottom",
                            fontsize=7, rotation=0)

    ax.set_xticks(range(len(ics)))
    ax.set_xticklabels(ics)
    ax.set_ylabel("median latency (ms)" + (" — log scale" if args.log else ""))
    if args.log:
        ax.set_yscale("log")
    ax.set_title("LDBC SNB Interactive — cross-system median latency")
    ax.legend(frameon=False, ncol=min(n_sys, 4), fontsize=9)
    ax.spines[["top", "right"]].set_visible(False)
    ax.grid(axis="y", alpha=0.25)
    fig.tight_layout()

    png = out_prefix.with_suffix(".png")
    svg = out_prefix.with_suffix(".svg")
    fig.savefig(png, dpi=150)
    fig.savefig(svg)
    sys.stderr.write(f"wrote {png}\n      {svg}\n")
    # Also print the median table as text.
    sys.stderr.write("\nmedian ms per IC × system:\n")
    sys.stderr.write(med.round(2).to_string() + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
