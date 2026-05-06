#!/usr/bin/env python3
"""Run a chosen IC against Kuzu (kuzudb on PyPI) for every
substitution-param row, emitting per-iter CSV in the cross-system
schema.

Output schema (matches src/bin/ldbc_bench.rs):
    query;backend;params;row;iter;result_count;elapsed_ns

`backend` is fixed to `kuzu-cypher`. `params` is the raw pipe-joined
param row from the LDBC params file. `row` is the 0-based param-row
index.

Per-IC inputs (all derived from --ic <n>):
    bench/ldbc-queries/ic<n>.toml         — query metadata
    bench/cross-system/kuzu/ic<n>.cypher  — Kuzu translation
    bench/data/substitution_parameters-sf0.1/.../<toml.params_file>

Shared input (one DB across all ICs):
    bench/data/cross-system/kuzu/ldbc-sf01.db — full LDBC SF0.1
    loaded by setup.py. The runner is IC-specific; setup is not.

Prereq: setup.py has been run so the LDBC DB exists. The runner
errors out cleanly if the DB is missing — it does NOT auto-invoke
setup, because the cross-system orchestrator (`run_all.sh`) is
responsible for ordering setup-then-run per system.

Usage:
    python run.py <out_csv> [--ic <n>] [--iters N] [--warmup N]
"""

from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

try:
    import kuzu
except ImportError:
    sys.stderr.write(
        "kuzu not installed. From this directory:\n"
        "  pip install -r requirements.txt\n"
    )
    sys.exit(1)


HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent.parent

# Row-hashing helper shared with the graphqlite runner; mirrors the
# Rust canonicalization in src/bin/ldbc_bench.rs so all three runners
# produce byte-identical blobs (and thus identical sha256 hashes) for
# the same logical row set.
sys.path.insert(0, str(HERE.parent / "_lib"))
from row_hash import canonicalize_and_hash, append_rows_jsonl  # noqa: E402

PARAMS_DIR = (
    REPO_ROOT
    / "bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1"
)
DB_DIR = REPO_ROOT / "bench/data/cross-system/kuzu"
DB_NAME = "ldbc-sf01.db"
LDBC_QUERIES_DIR = REPO_ROOT / "bench/ldbc-queries"
BACKEND_LABEL = "kuzu-cypher"


def load_toml(path: Path) -> dict:
    import tomllib
    with path.open("rb") as f:
        return tomllib.load(f)


def shape_of_value(v) -> str:
    """Mirror of `shape_of_value` in src/bin/ldbc_bench.rs — keep in
    sync. Kuzu returns Python-native types from RETURN clauses
    (int / float / str / None / list), so the standard mapping
    applies. INT64 becomes Python int, STRING becomes str, etc.
    """
    if v is None:
        return "n"
    if isinstance(v, bool):
        return "b"
    if isinstance(v, int):
        return "i"
    if isinstance(v, float):
        return "f"
    if isinstance(v, str):
        return "s"
    if isinstance(v, list):
        return "l"
    return "r"


def shape_of_rows(rows: list[list], n_columns: int) -> str:
    if not rows:
        return "empty"
    cols: list[set[str]] = [set() for _ in range(n_columns)]
    for r in rows:
        for i in range(min(n_columns, len(r))):
            cols[i].add(shape_of_value(r[i]))
    return ",".join("/".join(sorted(s)) for s in cols)


def load_query(path: Path) -> str:
    """Read the Cypher query, stripping leading // comment lines so
    they don't get sent to the engine.
    """
    out_lines = []
    in_comment_block = True
    for line in path.read_text(encoding="utf-8").splitlines():
        if in_comment_block and (line.startswith("//") or not line.strip()):
            continue
        in_comment_block = False
        out_lines.append(line)
    return "\n".join(out_lines).strip()


def load_params(path: Path) -> tuple[list[str], list[list[str]]]:
    with path.open(encoding="utf-8") as f:
        lines = [ln.rstrip("\n\r") for ln in f if ln.strip()]
    if len(lines) < 2:
        raise RuntimeError(f"params file too short: {path}")
    header = lines[0].split("|")
    data = [ln.split("|") for ln in lines[1:]]
    return header, data


def coerce(s: str) -> int | str:
    try:
        return int(s)
    except ValueError:
        return s


def derive_columns(return_columns: list[str]) -> list[str]:
    return [c.replace(".", "_") for c in return_columns]


def run_query(conn, query: str, bindings: dict) -> tuple[list[str], list[list]]:
    """Execute, return (column_names, data_rows). Kuzu's
    `Connection.execute(query_string, params)` is the documented
    user-facing API; the engine maintains an internal plan cache so
    repeated calls with the same query string don't re-parse on
    every iteration. (Note: Kuzu has a `PreparedStatement` API too,
    but it's officially deprecated in 0.11.x — we deliberately use
    the recommended single-call form so the bench reflects what end
    users would actually write.)

    Iteration: `QueryResult.has_next()` / `.get_next()` materializes
    rows one at a time across the PyO3 boundary. We collect into a
    list because the runner needs `len(rows)` and the shape-of-rows
    type signature.
    """
    result = conn.execute(query, bindings)
    cols = result.get_column_names()
    rows = []
    while result.has_next():
        rows.append(result.get_next())
    return cols, rows


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("out_csv", type=Path)
    ap.add_argument("--ic", type=int, default=2)
    ap.add_argument("--iters", type=int, default=10)
    ap.add_argument("--warmup", type=int, default=2)
    args = ap.parse_args()

    if args.iters < 1:
        sys.stderr.write("--iters must be >= 1\n")
        return 1

    ic = args.ic
    toml_path = LDBC_QUERIES_DIR / f"ic{ic}.toml"
    query_file = HERE / f"ic{ic}.cypher"
    db_path = DB_DIR / DB_NAME

    if not toml_path.is_file():
        sys.stderr.write(f"  toml missing: {toml_path}\n")
        return 1
    if not query_file.is_file():
        sys.stderr.write(
            f"  cypher translation missing: {query_file}\n"
            f"  (write it as a translation of {toml_path})\n"
        )
        return 1

    toml = load_toml(toml_path)
    if toml.get("status") != "implemented":
        sys.stderr.write(
            f"  ic{ic}.toml status is {toml.get('status')!r}, not 'implemented'. "
            f"Skipping.\n"
        )
        return 0

    params_file = PARAMS_DIR / toml["params_file"]
    if not params_file.is_file():
        sys.stderr.write(
            f"  params file missing: {params_file}\n"
            f"  run ./target/release/bench_setup from the repo root first.\n"
        )
        return 1

    columns = derive_columns(toml.get("return_columns", []))
    query_label = f"IC{ic}"

    if not db_path.exists():
        sys.stderr.write(
            f"  kuzu db missing: {db_path}\n"
            f"  run setup.py first:\n"
            f"    python {HERE / 'setup.py'}\n"
            f"  (the cross-system orchestrator does this automatically;\n"
            f"   if you're seeing this manually, you bypassed run_all.sh)\n"
        )
        return 1

    query = load_query(query_file)
    header, params_rows = load_params(params_file)
    sys.stderr.write(
        f"  kuzu ic{ic}: {len(params_rows)} param rows × {args.iters} iters "
        f"(+ {args.warmup} warmup)\n"
    )

    # RSS sampling — see graphqlite/run.py for rationale; same shape.
    try:
        import psutil
        _proc = psutil.Process()
        _rss_baseline_mib = _proc.memory_info().rss / (1024 * 1024)
        sys.stderr.write(f"RSS baseline: {_rss_baseline_mib:.1f} MiB\n")
    except ImportError:
        _proc = None
        _rss_baseline_mib = 0.0
        sys.stderr.write("RSS baseline: psutil not installed (skipping)\n")

    db = kuzu.Database(str(db_path))
    conn = kuzu.Connection(db)

    if _proc is not None:
        cur = _proc.memory_info().rss / (1024 * 1024)
        sys.stderr.write(
            f"  RSS after open: {cur:.1f} MiB (+{cur - _rss_baseline_mib:.1f} MiB)\n"
        )

    peak_rss_mib = _rss_baseline_mib

    # Row-equivalence dump path: sibling JSONL alongside the CSV.
    # `<out_csv stem>.rows.jsonl`. compare_results.py uses these for
    # human diff when hashes mismatch across systems.
    rows_jsonl = args.out_csv.with_suffix(".rows.jsonl")
    if rows_jsonl.exists():
        rows_jsonl.unlink()  # fresh per run

    with args.out_csv.open("w", encoding="utf-8", newline="") as out:
        out.write("query;backend;params;row;iter;result_count;elapsed_ns\n")

        for row_idx, raw_row in enumerate(params_rows):
            # Kuzu's parameter dict keys do NOT include the `$` prefix
            # — the engine maps `$name` in the query to `name` in the
            # dict.
            param_dict = {col: coerce(val) for col, val in zip(header, raw_row)}
            joined = "|".join(raw_row)

            for _ in range(args.warmup):
                run_query(conn, query, param_dict)

            iter0_rows = None
            elapsed_ns = 0
            for n in range(args.iters):
                t = time.perf_counter_ns()
                _hdr, rows = run_query(conn, query, param_dict)
                elapsed_ns = time.perf_counter_ns() - t
                out.write(
                    f"{query_label};{BACKEND_LABEL};{joined};{row_idx};{n};"
                    f"{len(rows)};{elapsed_ns}\n"
                )
                if n == 0:
                    iter0_rows = rows

            if _proc is not None:
                cur = _proc.memory_info().rss / (1024 * 1024)
                if cur > peak_rss_mib:
                    peak_rss_mib = cur

            actual_shape = shape_of_rows(iter0_rows or [], len(columns))
            actual_count = len(iter0_rows or [])
            # Row-content hash for the cross-system row-equivalence
            # oracle. Kuzu's `result.get_next()` returns positional
            # lists — pass them straight through to the canonicalizer.
            #
            # Single ROW line: count + shape (informational) + hash
            # (the strong cross-system oracle). The hash subsumes
            # any per-column-type shape contract — see Rust mirror.
            rows_blob, row_hash = canonicalize_and_hash(iter0_rows or [])
            sys.stderr.write(
                f"  ROW row={row_idx} count={actual_count} "
                f"shape={actual_shape} hash={row_hash}\n"
            )
            append_rows_jsonl(
                rows_jsonl,
                ic,
                joined,
                row_idx,
                actual_count,
                rows_blob,
                row_hash,
            )
            sys.stderr.write(
                f"  row {row_idx}: rc={actual_count} "
                f"last_iter_ms={elapsed_ns / 1e6:.2f}\n"
            )

    if _proc is not None:
        sys.stderr.write(
            f"Peak RSS during query loop: {peak_rss_mib:.1f} MiB "
            f"(+{peak_rss_mib - _rss_baseline_mib:.1f} MiB over baseline)\n"
        )

    sys.stderr.write(f"  done -> {args.out_csv}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
