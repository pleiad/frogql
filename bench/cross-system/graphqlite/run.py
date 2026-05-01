#!/usr/bin/env python3
"""Run IC2 against graphqlite for every substitution-param row,
emitting per-iter CSV in the cross-system schema.

Output schema (matches src/bin/ldbc_bench.rs at line 665):
    query;backend;params;row;iter;result_count;elapsed_ns

The `backend` column is fixed to `graphqlite-cypher`. `params` is the
raw pipe-joined param row from the LDBC params file (used as the
join key against gqlite's CSV in compare_results.py). `row` is the
0-based param-row index.

Prereq: setup.py has been run, so
`bench/data/cross-system/graphqlite/ic2.db` exists.

Usage:
    python run.py <out_csv> [--iters N] [--warmup N]

The runner *will* call setup.py for you on the first invocation if
the .db is missing — that's a one-time cost (~few minutes), cached
on disk afterward.
"""

from __future__ import annotations

import argparse
import sys
import subprocess
import time
from pathlib import Path

try:
    from graphqlite import Graph
except ImportError:
    sys.stderr.write(
        "graphqlite not installed. From this directory:\n"
        "  pip install -r requirements.txt\n"
    )
    sys.exit(1)


HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent.parent

DB_PATH = REPO_ROOT / "bench/data/cross-system/graphqlite/ic2.db"
PARAMS_FILE = (
    REPO_ROOT
    / "bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1/interactive_2_param.txt"
)
QUERY_FILE = HERE / "ic2.cypher"
IC2_TOML = REPO_ROOT / "bench/ldbc-queries/ic2.toml"
BACKEND_LABEL = "graphqlite-cypher"

# IC2 RETURN columns in positional order. graphqlite's Cypher dialect
# returns dicts keyed by AS-aliases (`friend_id`, ...); gqlite returns
# positional `Vec<Value>`. We index dicts by these aliases so the
# shape check is positional and dialect-independent.
IC2_COLUMNS = [
    "friend_id",
    "friend_firstName",
    "friend_lastName",
    "c_id",
    "c_content",
    "c_creationDate",
]


def shape_of_value(v) -> str:
    """Mirror of `shape_of_value` in src/bin/ldbc_bench.rs — keep in sync."""
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


def shape_of_rows(rows: list, columns: list[str]) -> str:
    """Per-column type-set across all rows. Mirrors `shape_of_result`
    in src/bin/ldbc_bench.rs: each column's distinct types join with
    `/`, columns join with `,`. Example: `i,s,s,i,n/s,i` for IC2 when
    `c.content` carries both Null and Str across the result set.
    """
    if not rows:
        return "empty"
    cols: list[set[str]] = [set() for _ in columns]
    for r in rows:
        for i, c in enumerate(columns):
            cols[i].add(shape_of_value(r.get(c)))
    return ",".join("/".join(sorted(s)) for s in cols)


def verify_shape(actual: str, expected: str) -> str | None:
    """Mirror of `verify_shape` in src/bin/ldbc_bench.rs. Returns
    None if `actual ⊆ expected` per column, else a short diagnosis.
    """
    a = [set(c.split("/")) for c in actual.split(",")]
    e = [set(c.split("/")) for c in expected.split(",")]
    if len(a) != len(e):
        return f"column count: actual={len(a)}, expected={len(e)}"
    for i, (ac, ec) in enumerate(zip(a, e)):
        if not ac.issubset(ec):
            extras = sorted(ac - ec)
            return f"col {i}: actual {sorted(ac)} not ⊆ expected {sorted(ec)} (extra: {extras})"
    return None


def load_expected_shape(toml_path: Path) -> str | None:
    """Pull `expected_shape` from the IC's toml so the Python runner
    verifies against the same contract the Rust runner uses. Returns
    None if the field is absent.
    """
    import tomllib

    with toml_path.open("rb") as f:
        return tomllib.load(f).get("expected_shape")


def load_query(path: Path) -> str:
    """Read the Cypher query, stripping leading // comment lines so
    they don't get sent to the engine. (graphqlite likely tolerates
    them, but cleaner to strip than to depend on that.)
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
    """Read the LDBC pipe-delimited params file. Returns
    (header_columns, [data_rows]).
    """
    with path.open(encoding="utf-8") as f:
        lines = [ln.rstrip("\n\r") for ln in f if ln.strip()]
    if len(lines) < 2:
        raise RuntimeError(f"params file too short: {path}")
    header = lines[0].split("|")
    data = [ln.split("|") for ln in lines[1:]]
    return header, data


def coerce(s: str) -> int | str:
    """Best-effort int coercion (LDBC IC2 params are ints — personId,
    maxDate). Falls back to string for anything else, so the function
    is reusable for other ICs later."""
    try:
        return int(s)
    except ValueError:
        return s


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("out_csv", type=Path)
    ap.add_argument("--iters", type=int, default=10)
    ap.add_argument("--warmup", type=int, default=2)
    args = ap.parse_args()

    if args.iters < 1:
        sys.stderr.write("--iters must be >= 1\n")
        return 1

    if not DB_PATH.exists():
        sys.stderr.write(
            f"  graphqlite db missing: {DB_PATH}\n"
            f"  running setup.py to build it (one-time, ~minutes)...\n"
        )
        rc = subprocess.run(
            [sys.executable, str(HERE / "setup.py")],
            cwd=str(REPO_ROOT),
        ).returncode
        if rc != 0:
            sys.stderr.write(f"  setup.py failed with code {rc}\n")
            return rc
    if not PARAMS_FILE.is_file():
        sys.stderr.write(
            f"  params file missing: {PARAMS_FILE}\n"
            f"  run ./target/release/bench_setup from the repo root first.\n"
        )
        return 1

    query = load_query(QUERY_FILE)
    expected_shape = load_expected_shape(IC2_TOML)
    header, params_rows = load_params(PARAMS_FILE)
    sys.stderr.write(
        f"  graphqlite: {len(params_rows)} param rows × {args.iters} iters "
        f"(+ {args.warmup} warmup)\n"
    )

    g = Graph(str(DB_PATH))

    with args.out_csv.open("w", encoding="utf-8", newline="") as out:
        # Same header line ldbc_bench emits.
        out.write("query;backend;params;row;iter;result_count;elapsed_ns\n")

        for row_idx, raw_row in enumerate(params_rows):
            param_dict = {col: coerce(val) for col, val in zip(header, raw_row)}
            joined = "|".join(raw_row)

            # Warmup iters discarded; not written to CSV. The bench
            # uses warmup to absorb any first-call JIT/cache effects
            # the engine has.
            for _ in range(args.warmup):
                g.query(query, param_dict)

            # Shape work runs after the iter loop, never between timed iters.
            iter0_result = None
            for n in range(args.iters):
                t = time.perf_counter_ns()
                result = g.query(query, param_dict)
                elapsed_ns = time.perf_counter_ns() - t
                out.write(
                    f"IC2;{BACKEND_LABEL};{joined};{row_idx};{n};"
                    f"{len(result)};{elapsed_ns}\n"
                )
                if n == 0:
                    iter0_result = result
            # Pin shape and count to iter 0; non-determinism would surface
            # as both moving together, not one alone.
            actual_shape = shape_of_rows(iter0_result, IC2_COLUMNS)
            actual_count = len(iter0_result)
            if expected_shape is None:
                status = "no-expected"
            else:
                why = verify_shape(actual_shape, expected_shape)
                status = "ok" if why is None else f'fail reason="{why}"'
            sys.stderr.write(
                f"  SHAPE row={row_idx} count={actual_count} shape={actual_shape} status={status}\n"
            )

            sys.stderr.write(
                f"  row {row_idx}: rc={len(result)} "
                f"last_iter_ms={elapsed_ns / 1e6:.2f}\n"
            )

    sys.stderr.write(f"  done -> {args.out_csv}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
