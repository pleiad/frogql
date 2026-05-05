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
    bench/data/cross-system/kuzu/ic<n>.db — pre-loaded DB

Prereq: setup.py has been run (or will be auto-run) so the DB exists.

Usage:
    python run.py <out_csv> [--ic <n>] [--iters N] [--warmup N]
"""

from __future__ import annotations

import argparse
import sys
import subprocess
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

PARAMS_DIR = (
    REPO_ROOT
    / "bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1"
)
DB_DIR = REPO_ROOT / "bench/data/cross-system/kuzu"
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


def verify_shape(actual: str, expected: str) -> str | None:
    a = [set(c.split("/")) for c in actual.split(",")]
    e = [set(c.split("/")) for c in expected.split(",")]
    if len(a) != len(e):
        return f"column count: actual={len(a)}, expected={len(e)}"
    for i, (ac, ec) in enumerate(zip(a, e)):
        if not ac.issubset(ec):
            extras = sorted(ac - ec)
            return f"col {i}: actual {sorted(ac)} not subset of expected {sorted(ec)} (extra: {extras})"
    return None


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
    """Execute, return (column_names, data_rows). Kuzu's `execute`
    returns a QueryResult with `has_next()` / `get_next()` iteration
    and `get_column_names()`. We materialize all rows because the
    runner needs `len(rows)` and shape-of-rows.
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
    db_path = DB_DIR / f"ic{ic}.db"

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

    expected_shape = toml.get("expected_shape")
    columns = derive_columns(toml.get("return_columns", []))
    query_label = f"IC{ic}"

    if not db_path.exists():
        sys.stderr.write(
            f"  kuzu db missing: {db_path}\n"
            f"  running setup.py to build it (one-time, ~minute)...\n"
        )
        rc = subprocess.run(
            [sys.executable, str(HERE / "setup.py"), "--ic", str(ic)],
            cwd=str(REPO_ROOT),
        ).returncode
        if rc != 0:
            sys.stderr.write(f"  setup.py failed with code {rc}\n")
            return rc

    query = load_query(query_file)
    header, params_rows = load_params(params_file)
    sys.stderr.write(
        f"  kuzu ic{ic}: {len(params_rows)} param rows × {args.iters} iters "
        f"(+ {args.warmup} warmup)\n"
    )

    db = kuzu.Database(str(db_path))
    conn = kuzu.Connection(db)

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

            actual_shape = shape_of_rows(iter0_rows or [], len(columns))
            actual_count = len(iter0_rows or [])
            if expected_shape is None:
                status = "no-expected"
            else:
                why = verify_shape(actual_shape, expected_shape)
                status = "ok" if why is None else f'fail reason="{why}"'
            sys.stderr.write(
                f"  SHAPE row={row_idx} count={actual_count} "
                f"shape={actual_shape} status={status}\n"
            )
            sys.stderr.write(
                f"  row {row_idx}: rc={actual_count} "
                f"last_iter_ms={elapsed_ns / 1e6:.2f}\n"
            )

    sys.stderr.write(f"  done -> {args.out_csv}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
