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
for `SHAPE` lines in the per-system stderr output). When a results
directory is passed alongside the unified CSV, this script also
scans those `*.stderr.log` files and surfaces shape-verification
failures as a fourth output section. Skipping this step (running
without a results dir) preserves the original behaviour.

Output (stdout):
1. Per-(system, params_row) latency table — median + p95 + iter count
   + result_count.
2. Result-count consistency check — for each params_row, do all systems
   agree? Disagreement is a per-system query-translation bug; flagged
   with WARN.
3. Side-by-side latency comparison — one row per params_row, one
   column per system, median latency.
4. Shape-verification summary (only when a results dir is given) —
   per-system pass/fail counts based on `SHAPE row=N status=...`
   lines in the stderr.log files. Catches per-column type
   mismatches that result_count alone misses.

Usage:
    python compare_results.py <unified_csv> [<results_dir>]
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


def collect_shape_status(results_dir: Path) -> dict[str, tuple[int, int, list[str]]]:
    """For each `<name>.stderr.log` in `results_dir`, return the count of
    `SHAPE row=N status=ok` lines and the list of failure reasons.
    Returns: {logical_name: (ok_count, total_count, [unique_failure_reasons])}.
    """
    import re
    shape_re = re.compile(r"^  SHAPE row=(\d+) count=\d+ shape=(\S+) status=(.+)$")
    out: dict[str, tuple[int, int, list[str]]] = {}
    for log in sorted(results_dir.glob("*.stderr.log")):
        name = log.stem.replace(".stderr", "")
        # Drop trailing `.icN` so e.g. `gqlite-baseline.ic2` becomes
        # `gqlite-baseline` for the summary line.
        name = re.sub(r"\.ic\d+$", "", name)
        ok = 0
        total = 0
        reasons: list[str] = []
        try:
            with log.open(encoding="utf-8", errors="replace") as f:
                for line in f:
                    m = shape_re.match(line.rstrip("\r\n"))
                    if not m:
                        continue
                    total += 1
                    status = m.group(3)
                    if status == "ok":
                        ok += 1
                    else:
                        reasons.append(status)
        except OSError:
            continue
        if total > 0:
            # Deduplicate reasons to keep the summary readable.
            unique = []
            seen = set()
            for r in reasons:
                if r in seen:
                    continue
                seen.add(r)
                unique.append(r)
            out[name] = (ok, total, unique)
    return out


def main() -> int:
    if len(sys.argv) not in (2, 3):
        print(f"usage: {sys.argv[0]} <unified_csv> [<results_dir>]", file=sys.stderr)
        return 2

    path = Path(sys.argv[1])
    if not path.is_file():
        print(f"not a file: {path}", file=sys.stderr)
        return 2

    results_dir: Path | None = None
    if len(sys.argv) == 3:
        results_dir = Path(sys.argv[2])
        if not results_dir.is_dir():
            print(f"not a directory: {results_dir}", file=sys.stderr)
            return 2

    by_cell: dict[tuple[int, str], list[tuple[int, int]]] = defaultdict(list)
    raw_params_by_row: dict[int, str] = {}
    queries_seen: set[str] = set()

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
            queries_seen.add(r.get("query", ""))

    if not by_cell:
        print("no rows found in input — nothing to compare.", file=sys.stderr)
        return 1

    systems = sorted({b for (_, b) in by_cell.keys()})
    rows = sorted({r for (r, _) in by_cell.keys()})
    query_label = sorted(queries_seen).pop() if len(queries_seen) == 1 else (
        ",".join(sorted(queries_seen)) if queries_seen else "?"
    )

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
                cells.append(f"{'—':>14}")
            else:
                elapsed_ms = [e / 1_000_000.0 for e, _ in samples]
                med = statistics.median(elapsed_ms)
                cells.append(f"{med:>14.2f}")
        print("  " + "  ".join(cells))

    # ---- 4. Shape verification (only when results_dir is given) ----
    if results_dir is not None:
        print()
        print(f"=== Shape verification [{query_label}] (per-system pass/fail) ===")
        print()
        statuses = collect_shape_status(results_dir)
        if not statuses:
            print("  (no SHAPE lines found in *.stderr.log — nothing to check)")
        else:
            any_failed = False
            for name in sorted(statuses):
                ok, total, reasons = statuses[name]
                if ok == total:
                    print(f"  OK   {name}: {ok}/{total} rows passed")
                else:
                    any_failed = True
                    print(f"  WARN {name}: {ok}/{total} rows passed; "
                          f"{total - ok} failed")
                    for r in reasons[:3]:
                        print(f"         reason: {r}")
                    if len(reasons) > 3:
                        print(f"         ... and {len(reasons) - 3} more "
                              "distinct reason(s)")
            if any_failed:
                print()
                print("  WARN at least one system returned columns whose "
                      "types don't match the canonical shape from "
                      "bench/ldbc-queries/<ic>.toml. count consistency "
                      "alone won't catch this; the per-system query "
                      "translation needs fixing.")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
