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
4. Memory footprint (only when a results dir is given) — peak RSS
   per (system, IC) parsed from the runners' stderr logs.
5. Row-content equivalence (only when a results dir is given) —
   per (IC, params_row), do all systems produce byte-identical
   canonical rows? Each runner sha256-hashes its iter-0 result
   and emits `ROW row=N count=N shape=<...> hash=<hex>`; this
   section compares hashes across systems. With ORDER BY in every
   IC's toml the iter-0 result is deterministic, so any mismatch
   is a real per-system translation bug — diff the sibling
   `<system>.ic<n>.rows.jsonl` files to localize the drift. Hash
   subsumes the per-column-type shape check: any column-count or
   per-cell-type drift changes the hash, so a separate shape-
   verification section was retired.

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


def collect_rss(results_dir: Path) -> dict[str, tuple[float, float]]:
    """Extract `Peak RSS during query loop: X MiB (+Y MiB over baseline)`
    lines from gqlite stderr.log files. Returns {logical_name: (peak,
    delta_over_baseline)}. Skips logs without the RSS line (graphqlite
    + kuzu Python runners don't emit it).

    The Python systems' RSS would be dominated by interpreter + library
    state, which isn't comparable to gqlite's pure engine RSS, so we
    surface gqlite-only numbers. The interesting comparison anyway is
    *across gqlite ablation modes* (lazy-baseline vs no-auto-indexes
    vs no-fold vs disk) where the value-per-MiB tradeoff is internal
    to gqlite and apples-to-apples.
    """
    import re
    rss_re = re.compile(r"Peak RSS during query loop:\s+([\d.]+)\s+MiB\s+\(\+([\d.]+)\s+MiB")
    out: dict[str, tuple[float, float]] = {}
    for log in sorted(results_dir.glob("*.stderr.log")):
        name = log.stem.replace(".stderr", "")
        try:
            for line in log.open(encoding="utf-8", errors="replace"):
                m = rss_re.search(line)
                if m:
                    out[name] = (float(m.group(1)), float(m.group(2)))
                    break
        except OSError:
            continue
    return out


def collect_row_hashes(
    results_dir: Path,
) -> dict[str, dict[tuple[str, int], tuple[int, str]]]:
    """For each `<system>.ic<n>.stderr.log`, parse `ROW row=N count=M
    shape=<...> hash=<hex>` lines emitted per params-row by the runner.

    Returns: `{logical_name: {(query, params_row_idx): (count, hex)}}`.
    `query` is normalized as `IC<n>` (the same form the CSV uses)
    inferred from the log's filename. compare's caller groups across
    systems by `(query, params_row_idx)` and flags any mismatch.

    Why a hash and not full rows: with ORDER BY in every IC's toml the
    iter-0 result is deterministic, so byte-equal canonical text →
    identical sha256 across systems. The full text is in the sibling
    `<system>.ic<n>.rows.jsonl` for diff on mismatch — the hash here
    is just the fast-equivalence check.
    """
    import re
    # Permissive: tolerate any whitespace+kv pairs between count and hash
    # (currently `shape=...` from each runner; future fields would slot
    # in without breaking the parser). The structural anchor is the
    # `hash=<hex>` at the end.
    row_re = re.compile(r"^  ROW row=(\d+) count=(\d+) .*hash=([0-9a-f]+)\s*$")
    name_re = re.compile(r"^(.+?)\.ic(\d+)$")
    out: dict[str, dict[tuple[str, int], tuple[int, str]]] = {}
    for log in sorted(results_dir.glob("*.stderr.log")):
        name = log.stem.replace(".stderr", "")
        m_name = name_re.match(name)
        if not m_name:
            # Non-(system).ic(N) shape — skip; we can't tell which IC.
            continue
        system = m_name.group(1)
        query = f"IC{m_name.group(2)}"
        try:
            with log.open(encoding="utf-8", errors="replace") as f:
                for line in f:
                    m = row_re.match(line.rstrip("\r\n"))
                    if not m:
                        continue
                    row_idx = int(m.group(1))
                    count = int(m.group(2))
                    hex_hash = m.group(3)
                    out.setdefault(system, {})[(query, row_idx)] = (count, hex_hash)
        except OSError:
            continue
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

    # by_cell is keyed by (query, row_idx, backend) so multi-IC unified
    # CSVs render as one section per IC. Without `query` in the key,
    # IC2's row 0 and IC3's row 0 (different params, different latencies)
    # would collapse into the same cell.
    by_cell: dict[tuple[str, int, str], list[tuple[int, int]]] = defaultdict(list)

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
            query = r.get("query", "?")
            backend = r["backend"]
            by_cell[(query, row_idx, backend)].append((elapsed, rc))

    if not by_cell:
        print("no rows found in input — nothing to compare.", file=sys.stderr)
        return 1

    queries = sorted({q for (q, _, _) in by_cell.keys()})

    for query_label in queries:
        # Filter to this IC only.
        ic_cells = {(rk, s): v for (q, rk, s), v in by_cell.items()
                    if q == query_label}
        systems = sorted({s for (_, s) in ic_cells.keys()})
        rows = sorted({r for (r, _) in ic_cells.keys()})

        # ---- 1. Per-cell summary ----
        print(f"=== Per-cell summary [{query_label}] (latency, ms; result_count) ===")
        print()
        header = ["params_row", "system", "iters", "median_ms", "p95_ms", "rc"]
        widths = [10, 18, 6, 10, 10, 4]
        print("  " + "  ".join(f"{h:<{w}}" for h, w in zip(header, widths)))
        print("  " + "-" * (sum(widths) + 2 * len(widths)))
        for rk in rows:
            for s in systems:
                samples = ic_cells.get((rk, s), [])
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
                samples = ic_cells.get((rk, s), [])
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
        header_row = ["params_row"] + systems
        print("  " + "  ".join(f"{h:>14}" for h in header_row))
        print("  " + "-" * (16 * len(header_row)))
        for rk in rows:
            cells = [f"{rk:>14}"]
            for s in systems:
                samples = ic_cells.get((rk, s), [])
                if not samples:
                    cells.append(f"{'—':>14}")
                else:
                    elapsed_ms = [e / 1_000_000.0 for e, _ in samples]
                    med = statistics.median(elapsed_ms)
                    cells.append(f"{med:>14.2f}")
            print("  " + "  ".join(cells))
        print()

    # ---- 4. Memory footprint (only when results_dir is given) ----
    # Preferred source: memory.csv, written by run_all.sh from the
    # memrun.py monitor that wraps every runner. It is authoritative
    # (process-group peak RSS sampled from /proc, no psutil needed) and
    # carries the cap status — including any memory_error rows where a
    # runner was killed for exceeding the cap. Fall back to the older
    # psutil-stderr scrape only when memory.csv is absent.
    mem_csv = results_dir / "memory.csv" if results_dir is not None else None
    if mem_csv is not None and mem_csv.exists():
        print()
        print("=== Memory + cap (peak RSS per system+IC, from memrun) ===")
        print()
        rows = []
        killed = []
        with mem_csv.open(encoding="utf-8") as f:
            next(f, None)  # header
            for line in f:
                parts = line.rstrip("\n").split(";")
                if len(parts) < 6:
                    continue
                sys_ic, status, peak_mib, limit_mib, wall_s, _exit = parts[:6]
                rows.append((sys_ic, status, peak_mib, limit_mib, wall_s))
                if status == "memory_error":
                    killed.append(sys_ic)
        print(f"  {'system.ic':<28}  {'status':<13}  {'peak MiB':>9}  "
              f"{'cap MiB':>9}  {'wall s':>8}")
        print("  " + "-" * 76)
        for sys_ic, status, peak_mib, limit_mib, wall_s in sorted(rows):
            print(f"  {sys_ic:<28}  {status:<13}  {peak_mib:>9}  "
                  f"{limit_mib:>9}  {wall_s:>8}")
        if killed:
            print()
            print(f"  !! {len(killed)} runner(s) hit the memory cap and were "
                  f"killed (memory_error):")
            for k in killed:
                print(f"       {k}")
        print()
        print("  peak MiB is the runner's whole process-group resident set")
        print("  (engine + DB + interpreter for the Python runners; ~Rust")
        print("  binary RSS for gqlite). The cap is enforced by SIGKILL.")
    elif results_dir is not None:
        rss = collect_rss(results_dir)
        if rss:
            print()
            print("=== Memory footprint (peak RSS during query loop, all ICs) ===")
            print()
            # Sort by name for stable ordering. Per-(system, IC) is the
            # right granularity because each runner emits one RSS line.
            print(f"  {'system':<32}  {'peak (MiB)':>11}  {'over baseline':>15}")
            print("  " + "-" * 64)
            for name in sorted(rss):
                peak, delta = rss[name]
                print(f"  {name:<32}  {peak:>11.1f}  {delta:>+15.1f}")
            print()
            print("  Notes: 'over baseline' subtracts the runner's at-startup")
            print("  RSS (mostly Python interpreter + library imports for")
            print("  graphqlite/kuzu; ~9 MiB for gqlite's Rust binary), so")
            print("  the delta is roughly 'engine + DB state' across runners.")

    # ---- 5. Row-content equivalence (hash) ----
    # The strongest oracle: every IC's toml has ORDER BY, so iter-0
    # rows are deterministic across systems. A byte-equal canonical
    # encoding → identical sha256 hashes. Mismatch = real
    # translation drift (a wrong column, a missing predicate, an
    # ordering bug).
    #
    # Section is keyed by (query, params_row) — for each such cell we
    # require ALL systems that have CSV measurements to also have ROW
    # hashes; a system present in the CSV but missing from the ROW
    # log is a partial-run failure (runner crashed mid-IC, stderr
    # buffer lost, harness change skipped emit). Surface it as MISSING
    # rather than silently reporting "1/1 systems agree".
    if results_dir is not None:
        print()
        print("=== Row-content equivalence (per (IC, params_row), all systems) ===")
        print()
        per_system = collect_row_hashes(results_dir)

        # Set of systems we EXPECT to see a hash for, derived from
        # the CSV column 2 grouped by query. A system listed here but
        # missing from per_system[s][(q, r)] is flagged.
        expected_systems_per_query: dict[str, set[str]] = {}
        for (q, _r, sys_name), _samples in by_cell.items():
            expected_systems_per_query.setdefault(q, set()).add(sys_name)

        if not per_system:
            print("  (no ROW lines found in *.stderr.log — nothing to check.")
            print("   The expected stderr line is `  ROW row=N count=N "
                  "shape=<sig> hash=<hex>`;")
            print("   verify each runner's stderr.log has it. Older binaries"
                  " that emit SHAPE/HASH separately are not parsed here.)")
        else:
            # Index by (query, row) → {system: (count, hash)}.
            by_cell_h: dict[tuple[str, int], dict[str, tuple[int, str]]] = {}
            for sys_name, per_row in per_system.items():
                for (q, r), (count, h) in per_row.items():
                    by_cell_h.setdefault((q, r), {})[sys_name] = (count, h)

            # Map a CSV `backend` column or a stderr-log filename
            # stem to the runner's logical system identity. The CSV
            # column and the stderr filename use DIFFERENT identifiers
            # for the same runner: e.g. graphqlite's CSV backend is
            # `graphqlite-cypher` but its stderr filename is
            # `graphqlite.icN.stderr.log` → both must normalize to
            # `graphqlite` so the missing-system check sees them as
            # the same runner. gqlite ablation modes are similar:
            # CSV `lazy-no-fold` and stderr `gqlite.icN.stderr.log`
            # both map to `gqlite` (one physical run rewrites the CSV
            # column post-hoc; the stderr stays with the runner name).
            def _normalize(s: str) -> str:
                # gqlite has two name spaces. The CSV `backend` column
                # uses `lazy-baseline` / `lazy-no-fold` / `disk` / etc.
                # (set by ldbc_bench, distinguishes ablation modes).
                # The stderr filename stems use `gqlite-baseline` /
                # `gqlite-no-fold` (set by run_all.sh, distinguishes
                # ablation modes for human-readable filenames). Both
                # have to collapse to the same logical identity so the
                # cross-source check ("CSV said this system measured;
                # did its stderr emit a hash?") doesn't flag every
                # ablation cell as MISS.
                if s in (
                    "lazy",
                    "lazy-baseline",
                    "lazy-no-fold",
                    "disk",
                    "disk-baseline",
                    "memory",
                    "gqlite-baseline",
                    "gqlite-no-fold",
                ):
                    return "gqlite"
                if s == "graphqlite-cypher":
                    return "graphqlite"
                if s == "kuzu-cypher":
                    return "kuzu"
                if s == "grafeo-gql":
                    return "grafeo"
                return s

            queries_h = sorted({q for (q, _) in by_cell_h})
            mismatches_total = 0  # no consensus — all hashes distinct
            consensus_total = 0   # majority + ≥1 outlier
            agree_total = 0       # full agreement
            missing_total = 0
            for q in queries_h:
                cells = sorted(
                    [(r, vmap) for (qq, r), vmap in by_cell_h.items() if qq == q]
                )
                print(f"  [{q}]")
                expected_norm = {_normalize(s) for s in expected_systems_per_query.get(q, set())}
                for r, vmap in cells:
                    actual_norm = {_normalize(s) for s in vmap.keys()}
                    missing = expected_norm - actual_norm
                    hashes = {h for (_c, h) in vmap.values()}

                    if missing:
                        missing_total += 1
                        print(f"    MISS row {r}: hashes from "
                              f"{sorted(actual_norm)} but CSV has "
                              f"measurements from {sorted(expected_norm)}; "
                              f"missing: {sorted(missing)}. The runner(s) "
                              f"likely crashed mid-IC or never emitted ROW "
                              f"lines — check the stderr.log.")
                    elif len(hashes) == 1:
                        # All expected systems agree → strongest possible signal.
                        agree_total += 1
                        h = next(iter(hashes))
                        n_systems = len(vmap)
                        print(f"    OK   row {r}: {n_systems}/{n_systems} "
                              f"systems agree (hash={h[:12]}...)")
                    else:
                        # Compute consensus: which hash do most systems agree
                        # on? If there's a strict majority (more than half),
                        # report it as a "consensus with outliers" — a much
                        # weaker but still informative signal than full
                        # agreement. The graphqlite int64-projection bug on
                        # IC2 and the row-count divergences on IC5/IC6/IC9
                        # all manifest as "3 systems agree byte-for-byte,
                        # graphqlite differs"; surfacing that explicitly
                        # avoids burying byte-identical gqlite-vs-kuzu
                        # agreement under generic "HASH DISAGREES" noise.
                        from collections import Counter
                        hash_counts = Counter(h for (_c, h) in vmap.values())
                        majority_hash, majority_n = hash_counts.most_common(1)[0]
                        n_systems = len(vmap)
                        majority_systems = sorted(
                            s for s, (_c, h) in vmap.items()
                            if h == majority_hash
                        )
                        outlier_systems = sorted(
                            s for s, (_c, h) in vmap.items()
                            if h != majority_hash
                        )
                        if majority_n >= 2 and majority_n > n_systems - majority_n:
                            consensus_total += 1
                            print(
                                f"    WARN row {r}: CONSENSUS with outliers "
                                f"({majority_n}/{n_systems} agree on "
                                f"{majority_hash[:12]}...): "
                                f"majority={majority_systems}, "
                                f"outliers={outlier_systems}"
                            )
                            for s in outlier_systems:
                                count, h = vmap[s]
                                print(f"           outlier {s}: count={count} "
                                      f"hash={h[:12]}...")
                        else:
                            mismatches_total += 1
                            print(
                                f"    WARN row {r}: NO CONSENSUS — "
                                f"all {n_systems} hashes differ"
                            )
                            for s in sorted(vmap):
                                count, h = vmap[s]
                                print(f"           {s}: count={count} "
                                      f"hash={h[:12]}...")
                        print(f"           diff actual rows in:")
                        for s in sorted(vmap):
                            print(f"             {results_dir}/{s}.{q.lower()}.rows.jsonl")
            print()
            total_cells = agree_total + consensus_total + mismatches_total + missing_total
            if mismatches_total == 0 and missing_total == 0 and consensus_total == 0:
                print(f"  OK row-content equivalence: {agree_total}/{total_cells} "
                      f"(IC, params_row) cells agreed across systems.")
            else:
                parts = []
                if agree_total:
                    parts.append(f"{agree_total} fully agreed")
                if consensus_total:
                    parts.append(f"{consensus_total} consensus-with-outliers")
                if mismatches_total:
                    parts.append(f"{mismatches_total} no-consensus")
                if missing_total:
                    parts.append(f"{missing_total} had missing-system hashes")
                print(f"  Row-content equivalence: {'; '.join(parts)} "
                      f"(of {total_cells} total cells).")
                if mismatches_total:
                    print("  Mismatches: with ORDER BY in the toml the iter-0 "
                          "results are deterministic, so any disagreement is "
                          "a real per-system translation bug — diff the listed")
                    print("  *.rows.jsonl files to see which row/cell drifted.")
                if missing_total:
                    print("  Missing-system hashes mean a runner produced "
                          "latency CSV rows but no ROW stderr line for that "
                          "(IC, row); usually a mid-IC crash. Check the "
                          "*.stderr.log for the missing system.")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
