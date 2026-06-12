#!/usr/bin/env python3
"""Run a chosen IC against Neo4j (community, via Docker — see
docker.sh) for every substitution-param row, emitting per-iter CSV in
the cross-system schema.

Output schema (matches src/bin/ldbc_bench.rs):
    query;backend;params;row;iter;result_count;elapsed_ns

`backend` is fixed to `neo4j-cypher`. `params` is the raw pipe-joined
param row from the LDBC params file. `row` is the 0-based param-row
index.

Per-IC inputs (all derived from --ic <n>):
    bench/ldbc-queries/ic<n>.toml          — query metadata
    bench/cross-system/neo4j/ic<n>.cypher  — Neo4j translation
    bench/data/substitution_parameters-sf0.1/.../<toml.params_file>

Shared input (one server across all ICs): the `frogql-bench-neo4j`
container loaded by setup.py. The runner errors out cleanly if the
server is unreachable or empty — it does NOT auto-invoke setup
(run_all.sh owns the setup-then-run ordering per system).

Latency is measured around the full query execution: `session.run` +
draining the result cursor (`list(result)`), i.e. the complete
round-trip a user pays per query. One driver + one session are reused
across all params/iters — the same way kuzu/run.py reuses its
Connection.

Structured columns (lists / maps, e.g. IC1's friendUniversities,
IC7's latestLike, IC12's tagNames) are encoded into froGQL's
`Value` Debug format (`List([Str("a"), ...])` /
`Record({"k": Int(1)})`, map keys sorted) before hashing, because
the canonical row-hash blob embeds structured cells via Rust's
`{:?}`. This is a faithful re-encoding of the same logical value —
scalar cells pass through untouched. See _encode_cell below and
DIVERGENCES.md.

Usage:
    python run.py <out_csv> [--ic <n>] [--iters N] [--warmup N]
Env:
    NEO4J_URI / NEO4J_USER / NEO4J_PASSWORD
    (defaults bolt://localhost:7687, neo4j, benchbench)
"""

from __future__ import annotations

import argparse
import os
import sys
import time
from pathlib import Path

try:
    from neo4j import GraphDatabase
except ImportError:
    sys.stderr.write(
        "neo4j driver not installed. From this directory:\n"
        "  pip install -r requirements.txt\n"
    )
    sys.exit(1)


HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent.parent

# Row-hashing helper shared with the other Python runners; mirrors the
# Rust canonicalization in src/bin/ldbc_bench.rs.
sys.path.insert(0, str(HERE.parent / "_lib"))
from row_hash import canonicalize_and_hash, append_rows_jsonl  # noqa: E402

PARAMS_DIR = (
    REPO_ROOT
    / "bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1"
)
LDBC_QUERIES_DIR = REPO_ROOT / "bench/ldbc-queries"
BACKEND_LABEL = "neo4j-cypher"

NEO4J_URI = os.environ.get("NEO4J_URI", "bolt://localhost:7687")
NEO4J_USER = os.environ.get("NEO4J_USER", "neo4j")
NEO4J_PASSWORD = os.environ.get("NEO4J_PASSWORD", "benchbench")


def load_toml(path: Path) -> dict:
    import tomllib
    with path.open("rb") as f:
        return tomllib.load(f)


def shape_of_value(v) -> str:
    """Mirror of `shape_of_value` in src/bin/ldbc_bench.rs — keep in
    sync. The Neo4j driver returns Python-native types (int / float /
    str / None / bool / list / dict)."""
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


# ---- froGQL Value-Debug encoding for structured cells ------------------

_RUST_ESCAPE_SPECIAL = {
    "\\": "\\\\", '"': '\\"',
    "\n": "\\n", "\r": "\\r", "\t": "\\t", "\0": "\\0",
}

# Unicode general categories Rust's `char::escape_debug` treats as
# non-printable (escaped as `\u{hex}`): control, format, surrogate,
# private-use, unassigned, and the separator categories except the
# ASCII space itself. The practical LDBC case is U+00A0 NBSP inside
# message content (`\u{a0}` in gqlite's blob).
_RUST_NONPRINTABLE_CATS = {"Cc", "Cf", "Cs", "Co", "Cn", "Zl", "Zp", "Zs"}


def _escape_rust_str(s: str) -> str:
    """Match Rust's `{:?}` for str (escape_debug): backslash, double
    quote, the named control escapes, and `\\u{hex}` for
    non-printables. Printable Unicode passes through unchanged."""
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
    """Python value → froGQL `Value` Debug string (the form
    `format!(\"{:?}\")` produces in src/bin/ldbc_bench.rs for
    structured cells). Record keys are sorted because Value::Record
    is a BTreeMap."""
    if v is None:
        return "Null"
    if isinstance(v, bool):
        return "Bool(true)" if v else "Bool(false)"
    if isinstance(v, int):
        return f"Int({v})"
    if isinstance(v, float):
        return f"Float({v!r})"
    if isinstance(v, str):
        # rstrip mirrors the top-level cell normalization in
        # row_hash.canonicalize_cell ("loaders disagree on trailing
        # whitespace") — gqlite's CSV loader trims trailing blanks,
        # ours stores the field verbatim, and inside a structured
        # cell the canonicalizer can't normalize for us.
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
    """Scalars pass through (row_hash canonicalizes them identically
    on both sides); lists/dicts become the Rust Debug string that the
    gqlite blob embeds for Value::List / Value::Record cells."""
    if isinstance(v, (list, dict)):
        return _encode_value(v)
    return v


def encode_rows_for_hash(rows: list[list]) -> list[list]:
    return [[_encode_cell(v) for v in r] for r in rows]


# ---- query / params loading -------------------------------------------

def load_query(path: Path) -> str:
    """Read the Cypher query, stripping leading // comment lines."""
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


def run_query(session, query: str, bindings: dict) -> list[list]:
    """Execute and fully drain. `record.values()` returns cells in
    RETURN-clause order, which is the canonical positional form the
    row hash needs."""
    result = session.run(query, bindings)
    return [list(rec.values()) for rec in result]


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
            f"  ic{ic}.toml status is {toml.get('status')!r}, not "
            f"'implemented'. Skipping.\n"
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

    query = load_query(query_file)
    header, params_rows = load_params(params_file)
    sys.stderr.write(
        f"  neo4j ic{ic}: {len(params_rows)} param rows × {args.iters} iters "
        f"(+ {args.warmup} warmup)\n"
    )

    # RSS sampling — note this is the CLIENT process; the engine runs
    # in the Docker container, so unlike the embedded systems this
    # mostly tracks driver-side result buffers. Kept for schema parity.
    try:
        import psutil
        _proc = psutil.Process()
        _rss_baseline_mib = _proc.memory_info().rss / (1024 * 1024)
        sys.stderr.write(f"RSS baseline: {_rss_baseline_mib:.1f} MiB\n")
    except ImportError:
        _proc = None
        _rss_baseline_mib = 0.0
        sys.stderr.write("RSS baseline: psutil not installed (skipping)\n")

    driver = GraphDatabase.driver(NEO4J_URI, auth=(NEO4J_USER, NEO4J_PASSWORD))
    try:
        driver.verify_connectivity()
    except Exception as e:
        sys.stderr.write(
            f"  cannot reach Neo4j at {NEO4J_URI}: {e}\n"
            f"  start it with:  bash {HERE / 'docker.sh'} up\n"
            f"  then load it:   python {HERE / 'setup.py'}\n"
        )
        return 1

    session = driver.session(database="neo4j")
    n_nodes = session.run("MATCH (n) RETURN count(n) AS c").single()["c"]
    if n_nodes == 0:
        sys.stderr.write(
            f"  neo4j database at {NEO4J_URI} is EMPTY.\n"
            f"  load it first:  python {HERE / 'setup.py'}\n"
        )
        return 1

    peak_rss_mib = _rss_baseline_mib

    rows_jsonl = args.out_csv.with_suffix(".rows.jsonl")
    if rows_jsonl.exists():
        rows_jsonl.unlink()  # fresh per run

    with args.out_csv.open("w", encoding="utf-8", newline="") as out:
        out.write("query;backend;params;row;iter;result_count;elapsed_ns\n")

        for row_idx, raw_row in enumerate(params_rows):
            # The neo4j driver maps `$name` in the query to plain
            # `name` keys in the params dict (no `$` prefix) — same
            # convention as Kuzu.
            param_dict = {col: coerce(val) for col, val in zip(header, raw_row)}
            joined = "|".join(raw_row)

            for _ in range(args.warmup):
                run_query(session, query, param_dict)

            iter0_rows = None
            elapsed_ns = 0
            for n in range(args.iters):
                t = time.perf_counter_ns()
                rows = run_query(session, query, param_dict)
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
            # Structured cells re-encoded into froGQL's Value Debug
            # form (see module docstring); scalar cells untouched.
            rows_blob, row_hash = canonicalize_and_hash(
                encode_rows_for_hash(iter0_rows or [])
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

    session.close()
    driver.close()

    if _proc is not None:
        sys.stderr.write(
            f"Peak RSS during query loop: {peak_rss_mib:.1f} MiB "
            f"(+{peak_rss_mib - _rss_baseline_mib:.1f} MiB over baseline; "
            f"client-side only — engine RSS lives in the container)\n"
        )

    sys.stderr.write(f"  done -> {args.out_csv}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
