#!/usr/bin/env python3
"""Run a chosen IC against Grafeo (GrafeoDB/grafeo on PyPI, GQL-native)
for every substitution-param row, emitting per-iter CSV in the
cross-system schema.

Output schema (matches src/bin/ldbc_bench.rs):
    query;backend;params;row;iter;result_count;elapsed_ns

`backend` is fixed to `grafeo-gql`. `params` is the raw pipe-joined
param row from the LDBC params file. `row` is the 0-based param-row
index.

Per-IC inputs (all derived from --ic <n>):
    bench/ldbc-queries/ic<n>.toml       — query metadata
    bench/cross-system/grafeo/ic<n>.gql — Grafeo GQL translation
    bench/data/substitution_parameters-sf0.1/.../<toml.params_file>

Shared input (one DB across all ICs):
    bench/data/cross-system/grafeo/ldbc-sf01.grafeo — full LDBC SF0.1
    loaded by setup.py. The runner is IC-specific; setup is not.

Grafeo's user-facing API is `db.execute(query, params).to_list()`,
which returns a list of dicts keyed by RETURN alias (same shape as
graphqlite). The engine keeps an internal plan cache, so repeated
calls with the same query string don't re-parse.

Usage:
    python run.py <out_csv> [--ic <n>] [--iters N] [--warmup N]
"""

from __future__ import annotations

import argparse
import re
import sys
import time
from pathlib import Path

try:
    import grafeo
except ImportError:
    sys.stderr.write(
        "grafeo not installed. From this directory:\n"
        "  pip install -r requirements.txt\n"
    )
    sys.exit(1)


HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent.parent

# Row-hashing helper shared with the other runners; mirrors the Rust
# canonicalization in src/bin/ldbc_bench.rs so every runner produces
# byte-identical blobs (identical sha256) for the same logical rows.
sys.path.insert(0, str(HERE.parent / "_lib"))
from row_hash import canonicalize_and_hash, append_rows_jsonl  # noqa: E402

PARAMS_DIR = (
    REPO_ROOT
    / "bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1"
)
DB_DIR = REPO_ROOT / "bench/data/cross-system/grafeo"
DB_NAME = "ldbc-sf01.grafeo"
LDBC_QUERIES_DIR = REPO_ROOT / "bench/ldbc-queries"
BACKEND_LABEL = "grafeo-gql"


def load_toml(path: Path) -> dict:
    import tomllib
    with path.open("rb") as f:
        return tomllib.load(f)


def shape_of_value(v) -> str:
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
    if not rows:
        return "empty"
    cols: list[set[str]] = [set() for _ in columns]
    for r in rows:
        for i, c in enumerate(columns):
            cols[i].add(shape_of_value(r.get(c)))
    return ",".join("/".join(sorted(s)) for s in cols)


def load_query(path: Path) -> str:
    """Read the GQL query, stripping leading comment lines (// or --)."""
    out_lines = []
    in_comment_block = True
    for line in path.read_text(encoding="utf-8").splitlines():
        if in_comment_block and (
            line.startswith("//") or line.startswith("--") or not line.strip()
        ):
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


def inline_params(query: str, header: list[str], raw_row: list[str]) -> str:
    """Substitute every ``$<col>`` placeholder with its literal value so the
    engine never receives a bound parameter.

    Grafeo 0.5.x does not yet support a bound ``$param`` inside a WHERE filter
    (``GRAFEO-X001: Parameters not yet supported in filters``). This is the
    same textual-substitution path frogql's ``ldbc_bench`` already uses, so it
    is benchmark-consistent. A literal and a bound param with the same value
    produce the same rows — the row-hash oracle still guards equivalence.

    Ints inline raw; strings are single-quoted with ISO ``''`` escaping. Names
    are replaced longest-first with a trailing non-identifier guard so ``$start``
    cannot shadow ``$startDate``.
    """
    out = query
    for col, val in sorted(zip(header, raw_row), key=lambda kv: -len(kv[0])):
        v = coerce(val)
        literal = str(v) if isinstance(v, int) else "'" + str(v).replace("'", "''") + "'"
        out = re.sub(
            r"\$" + re.escape(col) + r"(?![A-Za-z0-9_])",
            lambda _m, lit=literal: lit,  # lambda avoids backref interpretation
            out,
        )
    return out


def derive_columns(return_columns: list[str]) -> list[str]:
    return [c.replace(".", "_") for c in return_columns]


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
    query_file = HERE / f"ic{ic}.gql"
    db_path = DB_DIR / DB_NAME

    if not toml_path.is_file():
        sys.stderr.write(f"  toml missing: {toml_path}\n")
        return 1
    if not query_file.is_file():
        sys.stderr.write(
            f"  gql translation missing: {query_file}\n"
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
            f"  grafeo db missing: {db_path}\n"
            f"  run setup.py first:\n"
            f"    python {HERE / 'setup.py'}\n"
            f"  (the cross-system orchestrator does this automatically;\n"
            f"   if you're seeing this manually, you bypassed run_all.sh)\n"
        )
        return 1

    query = load_query(query_file)
    header, params_rows = load_params(params_file)
    sys.stderr.write(
        f"  grafeo ic{ic}: {len(params_rows)} param rows × {args.iters} iters "
        f"(+ {args.warmup} warmup)\n"
    )

    try:
        import psutil
        _proc = psutil.Process()
        _rss_baseline_mib = _proc.memory_info().rss / (1024 * 1024)
        sys.stderr.write(f"RSS baseline: {_rss_baseline_mib:.1f} MiB\n")
    except ImportError:
        _proc = None
        _rss_baseline_mib = 0.0
        sys.stderr.write("RSS baseline: psutil not installed (skipping)\n")

    db = grafeo.GrafeoDB.open(str(db_path))

    if _proc is not None:
        cur = _proc.memory_info().rss / (1024 * 1024)
        sys.stderr.write(
            f"  RSS after open: {cur:.1f} MiB (+{cur - _rss_baseline_mib:.1f} MiB)\n"
        )

    peak_rss_mib = _rss_baseline_mib

    rows_jsonl = args.out_csv.with_suffix(".rows.jsonl")
    if rows_jsonl.exists():
        rows_jsonl.unlink()

    with args.out_csv.open("w", encoding="utf-8", newline="") as out:
        out.write("query;backend;params;row;iter;result_count;elapsed_ns\n")

        for row_idx, raw_row in enumerate(params_rows):
            # Inline the params as literals rather than binding them: Grafeo
            # rejects $-params inside WHERE filters, and frogql's runner inlines
            # too, so this keeps the comparison consistent. Built once per row.
            inlined = inline_params(query, header, raw_row)
            joined = "|".join(raw_row)

            for _ in range(args.warmup):
                db.execute(inlined, {}).to_list()

            iter0_result = None
            elapsed_ns = 0
            for n in range(args.iters):
                t = time.perf_counter_ns()
                result = db.execute(inlined, {}).to_list()
                elapsed_ns = time.perf_counter_ns() - t
                out.write(
                    f"{query_label};{BACKEND_LABEL};{joined};{row_idx};{n};"
                    f"{len(result)};{elapsed_ns}\n"
                )
                if n == 0:
                    iter0_result = result

            if _proc is not None:
                cur = _proc.memory_info().rss / (1024 * 1024)
                if cur > peak_rss_mib:
                    peak_rss_mib = cur

            # Column order for the canonical row encoding. The toml's
            # `return_columns` is the source of truth when present; some
            # ICs (e.g. IC1/IC7/IC12/IC13) omit it, in which case we fall
            # back to the projection order Grafeo returns in the result
            # dicts. Passing the wrong/empty column list would hash empty
            # cells, so this fallback is required for those ICs to produce
            # a meaningful cross-system row hash.
            eff_columns = columns
            if not eff_columns and iter0_result:
                first = iter0_result[0]
                if isinstance(first, dict):
                    eff_columns = list(first.keys())

            actual_shape = shape_of_rows(iter0_result or [], eff_columns)
            actual_count = len(iter0_result or [])
            rows_blob, row_hash = canonicalize_and_hash(
                iter0_result or [], eff_columns or None
            )
            sys.stderr.write(
                f"  ROW row={row_idx} count={actual_count} "
                f"shape={actual_shape} hash={row_hash}\n"
            )
            append_rows_jsonl(
                rows_jsonl, ic, joined, row_idx, actual_count,
                rows_blob, row_hash,
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
