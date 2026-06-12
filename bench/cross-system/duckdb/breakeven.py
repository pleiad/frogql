#!/usr/bin/env python3
"""Break-even analysis: ingest cost vs in-situ per-query cost.

Motivation (ISWC review): the real user workload is "I have a CSV
dataset, I want to answer queries on it". A graph engine pays an
ingest cost once, then answers each query fast; DuckDB in-situ pays
zero ingest but re-scans the CSVs on every query. The break-even
point is the number of queries after which the engine's ingest
investment pays off:

    break_even = ingest_seconds / (duckdb_median_s - engine_median_s)

per IC, for every engine whose per-query median beats DuckDB's. An
engine that is slower per query than the in-situ scan never breaks
even (its ingest cost is a pure loss for that IC).

Inputs:
    <ingest_csv>   output of bench/cross-system/ingest_bench.sh
                   (schema: system;elapsed_seconds;db_bytes;status)
    <results_dir>  a bench/cross-system/results/<timestamp>/ dir with
                   per-(system,IC) latency CSVs named <system>.ic<n>.csv
                   (schema: query;backend;params;row;iter;result_count;elapsed_ns).
                   The DuckDB baseline files must be present
                   (duckdb.ic<n>.csv) for an IC to be analyzed.

Medians are taken over ALL (param row, iter) samples in each CSV —
same statistic compare_results.py reports.

Usage:
    python breakeven.py <ingest_csv> <results_dir>
"""

from __future__ import annotations

import math
import re
import statistics
import sys
from pathlib import Path

FILE_RE = re.compile(r"^(?P<sys>[A-Za-z0-9_-]+)\.ic(?P<ic>\d+)\.csv$")


def read_ingest(path: Path) -> dict[str, float]:
    """system -> elapsed_seconds, for rows with status == ok."""
    out: dict[str, float] = {}
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as e:
        sys.stderr.write(f"cannot read ingest csv {path}: {e}\n")
        return out
    for ln in lines[1:]:
        if not ln.strip():
            continue
        parts = ln.split(";")
        if len(parts) < 4:
            sys.stderr.write(f"  malformed ingest line skipped: {ln!r}\n")
            continue
        system, elapsed, _bytes, status = parts[0], parts[1], parts[2], parts[3]
        if status.strip() != "ok":
            sys.stderr.write(
                f"  ingest row for {system!r} has status {status!r}; skipped\n"
            )
            continue
        try:
            out[system.strip()] = float(elapsed)
        except ValueError:
            sys.stderr.write(f"  bad elapsed_seconds for {system!r}; skipped\n")
    return out


def median_latency_s(path: Path) -> float | None:
    """Median elapsed_ns over every data row of a latency CSV, in s."""
    samples: list[int] = []
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return None
    for ln in lines[1:]:
        if not ln.strip():
            continue
        try:
            samples.append(int(ln.rsplit(";", 1)[1]))
        except (IndexError, ValueError):
            continue
    if not samples:
        return None
    return statistics.median(samples) / 1e9


def ingest_system_for(label: str) -> str:
    """Map a results-file system label to an ingest_bench system name.

    gqlite runs under per-backend labels (gqlite-lazy, gqlite-disk)
    but ingests once as `gqlite`. Other systems use their own name.
    """
    if label.startswith("gqlite"):
        return "gqlite"
    return label


def fmt_ms(s: float) -> str:
    return f"{s * 1e3:.1f}"


def main() -> int:
    if len(sys.argv) != 3:
        sys.stderr.write(f"usage: {sys.argv[0]} <ingest_csv> <results_dir>\n")
        return 1
    ingest_csv = Path(sys.argv[1])
    results_dir = Path(sys.argv[2])
    if not results_dir.is_dir():
        sys.stderr.write(f"results dir not found: {results_dir}\n")
        return 1

    ingest = read_ingest(ingest_csv)
    if not ingest:
        sys.stderr.write(
            "warning: no usable ingest rows — break-even column will be n/a\n"
        )

    # (system_label, ic) -> median seconds
    medians: dict[tuple[str, int], float] = {}
    for f in sorted(results_dir.iterdir()):
        m = FILE_RE.match(f.name)
        if not m:
            continue
        med = median_latency_s(f)
        if med is None:
            sys.stderr.write(f"  no samples in {f.name}; skipped\n")
            continue
        medians[(m.group("sys"), int(m.group("ic")))] = med

    duck_ics = sorted(ic for (s, ic) in medians if s == "duckdb")
    if not duck_ics:
        sys.stderr.write(
            f"no duckdb.ic<n>.csv files in {results_dir} — nothing to compare\n"
        )
        return 1

    header = (
        f"{'IC':<5} {'system':<16} {'median_ms':>10} {'duckdb_ms':>10} "
        f"{'saving_ms':>10} {'ingest_s':>9} {'break_even_queries':>19}"
    )
    print(header)
    print("-" * len(header))

    # Per-system accumulators for the all-ICs-combined summary.
    combined: dict[str, dict[str, float]] = {}

    for ic in duck_ics:
        duck_med = medians[("duckdb", ic)]
        others = sorted(s for (s, i) in medians if i == ic and s != "duckdb")
        if not others:
            print(f"IC{ic:<3} {'(no other systems)':<16} "
                  f"{'':>10} {fmt_ms(duck_med):>10}")
            continue
        for sys_label in others:
            med = medians[(sys_label, ic)]
            saving = duck_med - med
            ing_name = ingest_system_for(sys_label)
            ing = ingest.get(ing_name)

            acc = combined.setdefault(
                sys_label, {"engine_s": 0.0, "duck_s": 0.0, "n_ics": 0}
            )
            acc["engine_s"] += med
            acc["duck_s"] += duck_med
            acc["n_ics"] += 1

            if ing is None:
                be = "n/a (no ingest data)"
            elif saving <= 0:
                be = "never (not faster)"
            else:
                be = f"{math.ceil(ing / saving):,}"
            print(
                f"IC{ic:<3} {sys_label:<16} {fmt_ms(med):>10} "
                f"{fmt_ms(duck_med):>10} {fmt_ms(saving):>10} "
                f"{'' if ing is None else f'{ing:.1f}':>9} {be:>19}"
            )

    # Combined view: a round-robin workload over every IC duckdb has.
    print()
    print("Combined (sum of per-IC medians over the ICs both systems ran —")
    print("one round of the IC mix; break-even is in ROUNDS, not queries):")
    header2 = (
        f"{'system':<16} {'n_ics':>5} {'engine_s/round':>14} "
        f"{'duckdb_s/round':>14} {'break_even_rounds':>18}"
    )
    print(header2)
    print("-" * len(header2))
    for sys_label in sorted(combined):
        acc = combined[sys_label]
        saving = acc["duck_s"] - acc["engine_s"]
        ing = ingest.get(ingest_system_for(sys_label))
        if ing is None:
            be = "n/a (no ingest data)"
        elif saving <= 0:
            be = "never (not faster)"
        else:
            be = f"{math.ceil(ing / saving):,}"
        print(
            f"{sys_label:<16} {int(acc['n_ics']):>5} "
            f"{acc['engine_s']:>14.3f} {acc['duck_s']:>14.3f} {be:>18}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
