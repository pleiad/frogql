#!/usr/bin/env python3
"""Run a chosen IC against graphqlite for every substitution-param row,
emitting per-iter CSV in the cross-system schema.

Output schema (matches src/bin/ldbc_bench.rs):
    query;backend;params;row;iter;result_count;elapsed_ns

The `backend` column is fixed to `graphqlite-cypher`. `params` is the
raw pipe-joined param row from the LDBC params file (used as the
join key against gqlite's CSV in compare_results.py). `row` is the
0-based param-row index.

Per-IC inputs (all derived from the IC number):
    bench/ldbc-queries/ic<n>.toml         — query metadata
    bench/cross-system/graphqlite/ic<n>.cypher  — cypher translation
    bench/data/substitution_parameters-sf0.1/.../<toml.params_file>
    bench/data/cross-system/graphqlite/ic<n>.db — pre-loaded DB

Prereq: setup.py has been run so the LDBC DB exists. The runner
errors out cleanly if the DB is missing — the cross-system
orchestrator (`run_all.sh`) is responsible for ordering setup-then-run
per system.

Usage:
    python run.py <out_csv> [--ic <n>] [--iters N] [--warmup N]
"""

from __future__ import annotations

import argparse
import sys
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

# Row-hashing helper shared with the Kuzu runner; mirrors the Rust
# canonicalization in src/bin/ldbc_bench.rs so all three runners
# produce byte-identical blobs (and thus identical sha256 hashes)
# for the same logical row set.
sys.path.insert(0, str(HERE.parent / "_lib"))
from row_hash import canonicalize_and_hash, append_rows_jsonl  # noqa: E402

PARAMS_DIR = (
    REPO_ROOT
    / "bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1"
)
DB_DIR = REPO_ROOT / "bench/data/cross-system/graphqlite"
DB_NAME = "ldbc-sf01.db"
LDBC_QUERIES_DIR = REPO_ROOT / "bench/ldbc-queries"
BACKEND_LABEL = "graphqlite-cypher"


def load_toml(path: Path) -> dict:
    import tomllib
    with path.open("rb") as f:
        return tomllib.load(f)


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
    """Best-effort int coercion (LDBC params are usually ints).
    Falls back to string for anything else.
    """
    try:
        return int(s)
    except ValueError:
        return s


def derive_columns(return_columns: list[str]) -> list[str]:
    """Cypher AS-aliases follow `dot.path` → `dot_path`. The toml's
    `return_columns` field is the source of truth for both order and
    naming; we just translate the syntax."""
    return [c.replace(".", "_") for c in return_columns]


def derive_columns_from_cypher(query: str) -> list[str]:
    """Fallback when the toml has no `return_columns` (IC1/7/12/13 don't
    define one). Parse the FINAL `RETURN ... ` clause of the cypher and
    return its AS-aliases in order. graphqlite returns row dicts keyed by
    the RETURN alias in RETURN order, so this matches the dict keys.

    Splits on top-level commas only (depth-tracked over ()/[]/{}), cuts at
    a top-level ORDER BY / LIMIT, and reads the trailing `AS <alias>` of
    each item (falling back to the raw item text when un-aliased)."""
    import re
    idx = query.upper().rfind("RETURN")
    tail = query[idx + len("RETURN"):]
    # cut at top-level ORDER BY / LIMIT (depth 0)
    depth = 0
    cut = len(tail)
    i = 0
    up = tail.upper()
    while i < len(tail):
        ch = tail[i]
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif depth == 0:
            if up[i:i + 8] == "ORDER BY":
                cut = i
                break
            if up[i:i + 5] == "LIMIT" and (i == 0 or not up[i - 1].isalnum()):
                cut = i
                break
        i += 1
    tail = tail[:cut]
    # split on depth-0 commas
    items, depth, cur = [], 0, ""
    for ch in tail:
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        if ch == "," and depth == 0:
            items.append(cur)
            cur = ""
        else:
            cur += ch
    items.append(cur)
    cols = []
    for it in items:
        it = it.strip()
        if not it:
            continue
        m = re.search(r"\bAS\s+([A-Za-z_][A-Za-z0-9_]*)\s*$", it, re.I)
        cols.append(m.group(1) if m else it)
    return cols


# ---- froGQL Value-Debug encoding for structured (list/record) cells ----
# graphqlite returns Python lists/dicts for RECORD/COLLECT cells (IC1/7/12).
# gqlite's canonical row blob embeds those via Rust's `{:?}` Debug form
# (`List([Str("a")])`, `Record({...})` with sorted keys). Re-encode so the
# hashes match. Scalar cells pass through untouched. Byte-mirror of
# neo4j/run.py::_encode_value.
_RUST_ESCAPE_SPECIAL = {
    "\\": "\\\\", '"': '\\"',
    "\n": "\\n", "\r": "\\r", "\t": "\\t", "\0": "\\0",
}
_RUST_NONPRINTABLE_CATS = {"Cc", "Cf", "Cs", "Co", "Cn", "Zl", "Zp", "Zs"}


def _escape_rust_str(s: str) -> str:
    import unicodedata
    out = []
    for ch in s:
        if ch in _RUST_ESCAPE_SPECIAL:
            out.append(_RUST_ESCAPE_SPECIAL[ch])
        elif ch != " " and unicodedata.category(ch) in _RUST_NONPRINTABLE_CATS:
            out.append(f"\\u{{{ord(ch):x}}}")
        else:
            out.append(ch)
    return "".join(out)


def _encode_value(v) -> str:
    if v is None:
        return "Null"
    if isinstance(v, bool):
        return "Bool(true)" if v else "Bool(false)"
    if isinstance(v, int):
        return f"Int({v})"
    if isinstance(v, float):
        return f"Float({v!r})"
    if isinstance(v, str):
        return f'Str("{_escape_rust_str(v.rstrip())}")'
    if isinstance(v, list):
        return "List([" + ", ".join(_encode_value(x) for x in v) + "])"
    if isinstance(v, dict):
        inner = ", ".join(
            f'"{_escape_rust_str(k)}": {_encode_value(v[k])}'
            for k in sorted(v)
        )
        return "Record({" + inner + "})"
    return repr(v)


def _encode_cell(v):
    """Scalars pass through (row_hash canonicalizes them identically on
    both sides); lists/dicts become the Rust Debug string the gqlite blob
    embeds for Value::List / Value::Record cells."""
    if isinstance(v, (list, dict)):
        return _encode_value(v)
    return v


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("out_csv", type=Path)
    ap.add_argument("--ic", type=int, default=2,
                    help="LDBC IC number (default: 2)")
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

    query_label = f"IC{ic}"

    if not db_path.exists():
        sys.stderr.write(
            f"  graphqlite db missing: {db_path}\n"
            f"  run setup.py first:\n"
            f"    python {HERE / 'setup.py'}\n"
            f"  (the cross-system orchestrator does this automatically;\n"
            f"   if you're seeing this manually, you bypassed run_all.sh)\n"
        )
        return 1

    query = load_query(query_file)

    # Column order for the row hash. Prefer the toml's `return_columns`
    # (IC2/3/4/5/6/8/9/11 define one); fall back to parsing the cypher's
    # final RETURN aliases when it doesn't (IC1/7/12/13). graphqlite returns
    # row dicts keyed by RETURN alias in RETURN order, so both agree.
    rc = toml.get("return_columns", [])
    columns = derive_columns(rc) if rc else derive_columns_from_cypher(query)

    header, params_rows = load_params(params_file)
    sys.stderr.write(
        f"  graphqlite ic{ic}: {len(params_rows)} param rows × {args.iters} iters "
        f"(+ {args.warmup} warmup)\n"
    )

    # RSS sampling: snapshot baseline (Python interpreter + import
    # cost) BEFORE opening the DB so the "delta over baseline" mostly
    # captures DB+engine state, not Python runtime. Uses psutil
    # because it's cross-platform; resource.getrusage isn't on
    # Windows. Same line format gqlite emits → compare_results.py
    # parses uniformly.
    try:
        import psutil
        _proc = psutil.Process()
        _rss_baseline_mib = _proc.memory_info().rss / (1024 * 1024)
        sys.stderr.write(f"RSS baseline: {_rss_baseline_mib:.1f} MiB\n")
    except ImportError:
        _proc = None
        _rss_baseline_mib = 0.0
        sys.stderr.write("RSS baseline: psutil not installed (skipping)\n")

    g = Graph(str(db_path))

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
            param_dict = {col: coerce(val) for col, val in zip(header, raw_row)}
            joined = "|".join(raw_row)

            for _ in range(args.warmup):
                g.query(query, param_dict)

            iter0_result = None
            for n in range(args.iters):
                t = time.perf_counter_ns()
                result = g.query(query, param_dict)
                elapsed_ns = time.perf_counter_ns() - t
                out.write(
                    f"{query_label};{BACKEND_LABEL};{joined};{row_idx};{n};"
                    f"{len(result)};{elapsed_ns}\n"
                )
                if n == 0:
                    iter0_result = result

            # Sample RSS after each row (cheap; psutil reads /proc or
            # the Win32 API once). Tracks peak for the run.
            if _proc is not None:
                cur = _proc.memory_info().rss / (1024 * 1024)
                if cur > peak_rss_mib:
                    peak_rss_mib = cur

            actual_shape = shape_of_rows(iter0_result, columns)
            actual_count = len(iter0_result)
            # Row-content hash for the cross-system row-equivalence
            # oracle. graphqlite returns rows as dicts keyed by alias;
            # canonicalize positionally per `columns` so the encoding
            # matches gqlite/Kuzu (which return positional rows).
            #
            # Single ROW line carries count + shape (informational
            # smell-check) + hash (the strong cross-system oracle).
            # The hash subsumes the per-column-type shape contract
            # the runner used to verify against `expected_shape` in
            # the toml — any column-count or per-cell-type drift
            # changes the hash. Shape stays as human context only.
            # Project each dict row to a positional list in `columns`
            # order, re-encoding any list/dict cell into froGQL's Value
            # Debug form (see _encode_cell) so structured columns (IC1's
            # friendUniversities, IC7's latestLike, IC12's tagNames) hash
            # identically to gqlite's blob. Scalar cells pass through.
            positional = [
                [_encode_cell(r.get(c)) for c in columns]
                for r in (iter0_result or [])
            ]
            rows_blob, row_hash = canonicalize_and_hash(positional)
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
                f"  row {row_idx}: rc={len(result)} "
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
