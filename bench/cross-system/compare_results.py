#!/usr/bin/env python3
"""Compare per-system latencies from a unified CSV.

Input: a single CSV in the schema emitted by `src/bin/ldbc_bench.rs`:

    query;backend;params;row;iter;result_count;elapsed_ns

The `query` column identifies the IC (e.g. `IC2`); the unified CSV
typically holds a single IC's results from a single run_all.sh
invocation. The `backend` column is the system label (`lazy` for
gqlite, `graphqlite-cypher` for graphqlite, etc.). `row` is the
params-row index. `params` is the raw pipe-joined param row from the
LDBC param file, used as the join key across systems.

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
    queries_seen: set[str] = set()
    # Per-system error tally. A CSV row with `result_count == -1` is a
    # sentinel emitted by a runner whose query failed (panic, error,
    # whatever); we count those separately and exclude them from the
    # latency / count summaries below.
    errors_by_system: dict[str, int] = defaultdict(int)
    error_rows_by_system: dict[str, set[int]] = defaultdict(set)

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
            queries_seen.add(r.get("query", ""))
            raw_params_by_row.setdefault(row_idx, r["params"])
            if rc < 0:
                errors_by_system[backend] += 1
                error_rows_by_system[backend].add(row_idx)
                continue
            by_cell[(row_idx, backend)].append((elapsed, rc))

    if not by_cell and not errors_by_system:
        print("no rows found in input -- nothing to compare.", file=sys.stderr)
        return 1

    # Systems set is the union of "any successful row" and "any errored row",
    # so a system that errored on every param row still shows up in tables
    # (with a column of dashes) instead of vanishing silently.
    systems = sorted(
        {b for (_, b) in by_cell.keys()} | set(errors_by_system.keys())
    )
    rows = sorted(
        {r for (r, _) in by_cell.keys()}
        | {r for s in error_rows_by_system.values() for r in s}
    )
    query_label = sorted(queries_seen).pop() if len(queries_seen) == 1 else (
        ",".join(sorted(queries_seen)) if queries_seen else "?"
    )

    # ---- 0. Errored-row tally per system ----
    # Surface this BEFORE the per-cell table because a high error count means
    # the latency numbers below are over a partial sample. Per-system
    # divergences (e.g. graphlite/DIVERGENCES.md) document why a system
    # might error.
    print(f"=== Errored param rows [{query_label}] (sentinel result_count = -1) ===")
    print()
    if not errors_by_system:
        print("  none -- every system answered every param row.")
    else:
        for s in systems:
            n_iters = errors_by_system.get(s, 0)
            n_rows = len(error_rows_by_system.get(s, set()))
            if n_iters == 0:
                continue
            erows = sorted(error_rows_by_system.get(s, set()))
            print(
                f"  {s:<22} {n_rows} param row(s) errored "
                f"({n_iters} sentinel iter(s)): rows={erows}"
            )
    print()

    # ---- 1. Per-cell summary ----
    print(f"=== Per-cell summary [{query_label}] (latency, ms; result_count) ===")
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
    print(f"=== Result-count consistency [{query_label}] (per params_row across systems) ===")
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
    print(f"=== Side-by-side latency [{query_label}] (median ms) ===")
    print()
    header = ["params_row"] + systems
    print("  " + "  ".join(f"{h:>14}" for h in header))
    print("  " + "-" * (16 * len(header)))
    for rk in rows:
        cells = [f"{rk:>14}"]
        for s in systems:
            samples = by_cell.get((rk, s), [])
            if not samples:
                cells.append(f"{'--':>14}")
            else:
                elapsed_ms = [e / 1_000_000.0 for e, _ in samples]
                med = statistics.median(elapsed_ms)
                cells.append(f"{med:>14.2f}")
        print("  " + "  ".join(cells))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
