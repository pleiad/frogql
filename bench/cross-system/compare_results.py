#!/usr/bin/env python3
"""Compare per-system IC2 latencies from a unified CSV.

Input: a single CSV in the schema emitted by `src/bin/ldbc_bench.rs`:

    query;backend;params;row;iter;result_count;elapsed_ns

The `backend` column is the system label (`lazy` for gqlite,
`graphqlite-cypher` for graphqlite, etc.). `row` is the params-row
index (0..14 for IC2). `params` is the raw pipe-joined param row from
the LDBC param file, used as the join key across systems.

Result-shape verification is logged by each runner to stderr (search
for `SHAPE` lines in the per-system stderr output) and cross-checked
inline by `run_all.sh`. It is not stored or displayed here.

Output (stdout):
1. Per-(system, params_row) latency table — median + p95 + iter count
   + result_count.
2. Result-count consistency check — for each params_row, do all systems
   agree? Disagreement is a per-system query-translation bug; flagged
   with WARN.
3. Side-by-side latency comparison — one row per params_row, one
   column per system, median latency.

Usage:
    python compare_results.py <unified_csv>
"""

from __future__ import annotations

import csv
import statistics
import sys
from collections import defaultdict
from pathlib import Path


def percentile(samples: list[float], p: float) -> float:
    """Linear-interpolation percentile, p in [0, 100]."""
    if not samples:
        return float("nan")
    s = sorted(samples)
    if len(s) == 1:
        return s[0]
    rank = (p / 100.0) * (len(s) - 1)
    lo = int(rank)
    hi = min(lo + 1, len(s) - 1)
    frac = rank - lo
    return s[lo] * (1 - frac) + s[hi] * frac


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <unified_csv>", file=sys.stderr)
        return 2

    path = Path(sys.argv[1])
    if not path.is_file():
        print(f"not a file: {path}", file=sys.stderr)
        return 2

    by_cell: dict[tuple[int, str], list[tuple[int, int]]] = defaultdict(list)
    raw_params_by_row: dict[int, str] = {}

    with path.open() as f:
        reader = csv.DictReader(f, delimiter=";")
        for r in reader:
            try:
                row_idx = int(r["row"])
                elapsed = int(r["elapsed_ns"])
                rc = int(r["result_count"])
            except (KeyError, ValueError) as e:
                print(f"  ! malformed row, skipping: {e} :: {r}", file=sys.stderr)
                continue
            backend = r["backend"]
            by_cell[(row_idx, backend)].append((elapsed, rc))
            raw_params_by_row.setdefault(row_idx, r["params"])

    if not by_cell:
        print("no rows found in input — nothing to compare.", file=sys.stderr)
        return 1

    systems = sorted({b for (_, b) in by_cell.keys()})
    rows = sorted({r for (r, _) in by_cell.keys()})

    # ---- 1. Per-cell summary ----
    print("=== Per-cell summary (latency, ms; result_count) ===")
    print()
    header = ["params_row", "system", "iters", "median_ms", "p95_ms", "rc"]
    widths = [10, 18, 6, 10, 10, 4]
    print("  " + "  ".join(f"{h:<{w}}" for h, w in zip(header, widths)))
    print("  " + "-" * (sum(widths) + 2 * len(widths)))
    for rk in rows:
        for s in systems:
            samples = by_cell.get((rk, s), [])
            if not samples:
                continue
            elapsed_ms = [e / 1_000_000.0 for e, _ in samples]
            rcs = {rc for _, rc in samples}
            rc_str = str(next(iter(rcs))) if len(rcs) == 1 else f"!{sorted(rcs)}"
            med = statistics.median(elapsed_ms)
            p95 = percentile(elapsed_ms, 95)
            print(
                f"  {rk:<{widths[0]}}  {s:<{widths[1]}}  "
                f"{len(samples):<{widths[2]}}  "
                f"{med:<{widths[3]}.2f}  {p95:<{widths[4]}.2f}  "
                f"{rc_str:<{widths[5]}}"
            )

    # ---- 2. Result-count consistency check ----
    print()
    print("=== Result-count consistency (per params_row across systems) ===")
    print()
    mismatches: list[int] = []
    for rk in rows:
        rcs_per_sys: dict[str, object] = {}
        for s in systems:
            samples = by_cell.get((rk, s), [])
            if not samples:
                continue
            rcs = {rc for _, rc in samples}
            rcs_per_sys[s] = next(iter(rcs)) if len(rcs) == 1 else f"!{sorted(rcs)}"
        unique = set(rcs_per_sys.values())
        if len(unique) <= 1:
            rc = next(iter(unique)) if unique else "n/a"
            print(f"  OK   row {rk}: count={rc}")
        else:
            mismatches.append(rk)
            print(f"  WARN row {rk}: COUNT DISAGREES -- {rcs_per_sys}")

    if mismatches:
        print()
        print(
            f"  WARN count mismatches in {len(mismatches)}/{len(rows)} "
            f"rows: {mismatches}. Per-system query-translation bug; "
            f"latency comparison not trustworthy until fixed."
        )

    # ---- 3. Side-by-side latency table ----
    print()
    print("=== Side-by-side latency (median ms) ===")
    print()
    header = ["params_row"] + systems
    print("  " + "  ".join(f"{h:>14}" for h in header))
    print("  " + "-" * (16 * len(header)))
    for rk in rows:
        cells = [f"{rk:>14}"]
        for s in systems:
            samples = by_cell.get((rk, s), [])
            if not samples:
                cells.append(f"{'—':>14}")
            else:
                elapsed_ms = [e / 1_000_000.0 for e, _ in samples]
                med = statistics.median(elapsed_ms)
                cells.append(f"{med:>14.2f}")
        print("  " + "  ".join(cells))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
