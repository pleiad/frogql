#!/usr/bin/env python3
"""Two-panel cross-system plot: query latency (top) + peak memory (bottom).

Reads two artifacts from a results/<timestamp>/ dir:
  - cross_system.csv  (per-iter latency; schema
        query;backend;params;row;iter;result_count;elapsed_ns)
  - memory.csv        (per-(system,IC) peak RSS; schema
        system_ic;status;peak_rss_mib;limit_mib;wall_seconds;exit_code)

The harness measures wall-clock query time and peak resident memory (not
CPU-utilisation %), so the top panel is median query latency. Bars are
grouped by IC, coloured per system. Failed/timed-out runs are annotated
rather than drawn as a misleading height. Writes PNG + SVG.

Usage:
    python plot_cpu_mem.py [results/<timestamp>] [--out <prefix>] [--linear]

With no dir arg it picks the newest results/<timestamp>/ that has both files.

Requires matplotlib + pandas.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

try:
    import pandas as pd
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.patches import Patch, Rectangle
except ImportError as e:
    sys.stderr.write(f"missing dep ({e}). Install:\n  pip install matplotlib pandas\n")
    sys.exit(1)

HERE = Path(__file__).resolve().parent

# Display label + colour per system. Latency uses the `backend` column;
# memory uses the `system_ic` prefix — both normalise to the same label.
LAT_LABEL = {
    "lazy": "froGQL", "lazy-baseline": "froGQL", "disk": "froGQL (disk)",
    "memory": "froGQL (mem)", "kuzu-cypher": "Kuzu", "grafeo-gql": "Grafeo",
    "graphqlite-cypher": "GraphQLite",
}
MEM_LABEL = {"gqlite": "froGQL", "kuzu": "Kuzu", "grafeo": "Grafeo",
             "graphqlite": "GraphQLite"}
COLOR = {"froGQL": "#2c8a3e", "Kuzu": "#c2792e", "Grafeo": "#9b59b6",
         "GraphQLite": "#3b6ea5", "froGQL (disk)": "#1f5e2b",
         "froGQL (mem)": "#56a86a"}
SYS_ORDER = ["froGQL", "Kuzu", "Grafeo", "GraphQLite"]
# Failure markers: a hatched block fills the system's slot so a missing
# result is unmistakable (vs. a tiny tick at the axis floor). Colour + label
# per failure kind.
FAIL_STYLE = {
    "OOM": ("#b00020", "OOM"),       # killed for exceeding the RSS cap
    "ERR": ("#c0392b", "ERR"),       # runner/translation error
    "TIMEOUT": ("#e67e22", "TIMEOUT"),  # wall-clock cap hit
    "n/a": ("#9e9e9e", "n/a"),       # no data / not run
}


def ic_num(s: str) -> int:
    return int("".join(c for c in str(s) if c.isdigit()) or 0)


def latest_dir() -> Path:
    cands = sorted(p.parent for p in (HERE / "results").glob("*/memory.csv")
                   if (p.parent / "cross_system.csv").is_file())
    if not cands:
        sys.stderr.write("no results dir with both cross_system.csv + memory.csv\n")
        sys.exit(1)
    return cands[-1]


def grouped_bars(ax, ics, systems, value_of, fmt, log):
    n = max(len(systems), 1)
    bw = 0.8 / n
    for i, s in enumerate(systems):
        xs = [j + (i - (n - 1) / 2) * bw for j in range(len(ics))]
        for x, ic in zip(xs, ics):
            v, note = value_of(ic, s)
            if v is not None and v == v and v > 0:
                ax.bar(x, v, width=bw, color=COLOR.get(s, "#888"))
                ax.annotate(fmt(v), (x, v), ha="center", va="bottom",
                            fontsize=6.5, rotation=0)
            elif note:
                # Failed / timed-out / missing: fill the slot with a hatched
                # block + bold label so it reads as "no result here", not as a
                # near-zero bar. Blended transform (x in data, y in axes
                # fraction) keeps the block the same visible size on a log or
                # linear axis.
                fc, txt = FAIL_STYLE.get(note, FAIL_STYLE["n/a"])
                trans = ax.get_xaxis_transform()
                # Full-height faint band: spans 0→1 in axes fraction so it
                # cannot be misread as a value bar (no real bar reaches the
                # top). Label centred vertically reads as a slot annotation.
                ax.add_patch(Rectangle(
                    (x - bw / 2, 0.0), bw, 1.0, transform=trans,
                    facecolor=fc, alpha=0.10, edgecolor=fc, linestyle=":",
                    hatch="////", linewidth=0.8, zorder=0.5))
                ax.text(x, 0.5, txt, transform=trans, ha="center",
                        va="center", fontsize=8, fontweight="bold",
                        color=fc, rotation=90, zorder=3)
    ax.set_xticks(range(len(ics)))
    ax.set_xticklabels([f"IC{ic}" for ic in ics])
    if log:
        ax.set_yscale("log")
    ax.spines[["top", "right"]].set_visible(False)
    ax.grid(axis="y", alpha=0.25)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("results_dir", nargs="?", type=Path, default=None)
    ap.add_argument("--out", type=Path, default=None)
    ap.add_argument("--linear", action="store_true",
                    help="linear latency axis (default log)")
    args = ap.parse_args()

    rdir = args.results_dir or latest_dir()
    lat_csv, mem_csv = rdir / "cross_system.csv", rdir / "memory.csv"
    for f in (lat_csv, mem_csv):
        if not f.is_file():
            sys.stderr.write(f"not found: {f}\n")
            return 1

    # --- latency: median ms per (IC, system) ---
    lat = pd.read_csv(lat_csv, sep=";")
    lat["ms"] = lat["elapsed_ns"] / 1e6
    lat["system"] = lat["backend"].map(lambda b: LAT_LABEL.get(b, b))
    lat["ic"] = lat["query"].map(ic_num)
    lat_med = lat.groupby(["ic", "system"])["ms"].median()

    # --- memory: peak RSS per (IC, system) + status for annotations ---
    mem = pd.read_csv(mem_csv, sep=";")
    mem["system"] = mem["system_ic"].map(lambda s: MEM_LABEL.get(s.split(".")[0], s))
    mem["ic"] = mem["system_ic"].map(lambda s: ic_num(s.split(".")[1]))
    mem_rss = {(r.ic, r.system): r.peak_rss_mib for r in mem.itertuples()}
    mem_status = {(r.ic, r.system): r.status for r in mem.itertuples()}

    ics = sorted(set(lat["ic"]) | set(mem["ic"]))
    present = set(lat["system"]) | set(mem["system"])
    systems = [s for s in SYS_ORDER if s in present] + \
              [s for s in present if s not in SYS_ORDER]

    # Map run_all.sh / memrun status strings to a short marker label.
    NOTE = {"timeout": "TIMEOUT", "runner_error": "ERR",
            "oom": "OOM", "memory_error": "OOM"}

    def lat_val(ic, s):
        v = lat_med.get((ic, s))
        st = mem_status.get((ic, s))
        return (float(v) if v is not None else None), NOTE.get(st, "" if v is not None else "n/a")

    def mem_val(ic, s):
        st = mem_status.get((ic, s))
        if st == "ok":
            return mem_rss.get((ic, s)), ""
        return None, NOTE.get(st, "n/a")

    fig, (ax1, ax2) = plt.subplots(
        2, 1, figsize=(max(8, 1.5 * len(ics) + 2), 8), sharex=True)

    grouped_bars(ax1, ics, systems, lat_val, lambda v: f"{v:.0f}", log=not args.linear)
    ax1.set_ylabel("median query latency (ms)"
                   + ("" if args.linear else " — log"))
    ax1.set_title("LDBC SNB Interactive (SF0.1) — cross-system latency & peak memory")

    grouped_bars(ax2, ics, systems, mem_val, lambda v: f"{v:.0f}", log=False)
    ax2.set_ylabel("peak RSS (MiB)")
    ax2.set_xlabel("query")

    handles = [Patch(facecolor=COLOR.get(s, "#888"), label=s) for s in systems]
    # Append a legend entry for each failure kind actually present, so the
    # hatched blocks are self-documenting.
    present_fails = {NOTE.get(st) for st in mem_status.values()
                     if st != "ok" and NOTE.get(st)}
    for label in ("ERR", "TIMEOUT", "OOM"):
        if label in present_fails:
            fc, txt = FAIL_STYLE.get(label, ("#9e9e9e", label))
            handles.append(Patch(facecolor=fc, alpha=0.18, edgecolor=fc,
                                 hatch="////", label=txt))
    ax1.legend(handles=handles, frameon=False, ncol=len(handles), fontsize=9,
               loc="upper left")
    fig.tight_layout()

    out = args.out or (rdir / "cpu_mem")
    fig.savefig(out.with_suffix(".png"), dpi=150)
    fig.savefig(out.with_suffix(".svg"))
    sys.stderr.write(f"wrote {out.with_suffix('.png')}\n      {out.with_suffix('.svg')}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
