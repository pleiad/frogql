#!/usr/bin/env python3
"""Run a chosen IC against DuckDB *in situ* — SQL directly over the raw
LDBC SF0.1 CSV files — for every substitution-param row, emitting
per-iter CSV in the cross-system schema.

Output schema (matches src/bin/ldbc_bench.rs):
    query;backend;params;row;iter;result_count;elapsed_ns

`backend` is fixed to `duckdb-sql`. `params` is the raw pipe-joined
param row from the LDBC params file. `row` is the 0-based param-row
index.

In-situ semantics (the whole point of this baseline): there is NO
setup.py and NO persistent database. The connection is in-memory and
every "table" is a VIEW over `read_csv(...)` of the pipe-delimited
LDBC CSVs, so each query execution re-scans the CSV files it touches.
This is the "I have a CSV dataset, I want answers NOW" workload: zero
ingest cost, full scan cost paid per query. Pairing this runner's
latency with ingest_bench.sh's ingest-cost table yields the break-even
analysis (breakeven.py): after how many queries does paying a graph
engine's ingest beat re-scanning the CSVs per query.

Per-IC inputs (all derived from --ic <n>):
    bench/ldbc-queries/ic<n>.toml      — query metadata (source of truth)
    bench/cross-system/duckdb/ic<n>.sql — SQL translation
    bench/data/substitution_parameters-sf0.1/.../<toml.params_file>

Shared input:
    bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter/
    — the raw LDBC CSVs (bench_setup downloads them).

Usage:
    python run.py <out_csv> [--ic <n>] [--iters N] [--warmup N]
"""

from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

try:
    import duckdb
except ImportError:
    sys.stderr.write(
        "duckdb not installed. From this directory:\n"
        "  pip install -r requirements.txt\n"
    )
    sys.exit(1)


HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent.parent

# Row-hashing helper shared with the other runners; mirrors the Rust
# canonicalization in src/bin/ldbc_bench.rs so all runners produce
# byte-identical blobs (and thus identical sha256 hashes) for the same
# logical row set.
sys.path.insert(0, str(HERE.parent / "_lib"))
from row_hash import canonicalize_and_hash, append_rows_jsonl  # noqa: E402

PARAMS_DIR = (
    REPO_ROOT
    / "bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1"
)
CSV_DIR = (
    REPO_ROOT
    / "bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter"
)
LDBC_QUERIES_DIR = REPO_ROOT / "bench/ldbc-queries"
BACKEND_LABEL = "duckdb-sql"


# ---------------------------------------------------------------------------
# View definitions: every LDBC entity/relation the SQL translations
# reference is a VIEW over read_csv of the raw pipe-delimited CSV.
# Explicit `columns={...}` disables type sniffing so the schema is
# deterministic (LDBC dates are epoch-millis BIGINTs under the
# LongDateFormatter; IDs are BIGINT). DuckDB's CSV reader maps an
# empty field to NULL by default, which mirrors the gqlite loader's
# "empty field => property absent" convention (COALESCE relies on it).
#
# Views (not CTAS / temp tables) are load-bearing: a view re-executes
# its read_csv on every query, so the CSV scan cost is paid per query.
# Materializing into in-memory tables would silently turn this into an
# ingest-based system and invalidate the break-even analysis.
# ---------------------------------------------------------------------------

def _csv(rel: str, cols: dict[str, str]) -> str:
    path = CSV_DIR / rel
    col_sql = ", ".join(f"'{name}': '{ty}'" for name, ty in cols.items())
    return (
        f"read_csv('{path}', delim='|', header=true, columns={{{col_sql}}})"
    )


def view_ddl() -> list[str]:
    ddl = []

    def v(name: str, body: str) -> None:
        ddl.append(f"CREATE VIEW {name} AS {body}")

    v("person", "SELECT * FROM " + _csv(
        "dynamic/person_0_0.csv",
        {"id": "BIGINT", "firstName": "VARCHAR", "lastName": "VARCHAR",
         "gender": "VARCHAR", "birthday": "BIGINT", "creationDate": "BIGINT",
         "locationIP": "VARCHAR", "browserUsed": "VARCHAR"}))
    v("comment", "SELECT * FROM " + _csv(
        "dynamic/comment_0_0.csv",
        {"id": "BIGINT", "creationDate": "BIGINT", "locationIP": "VARCHAR",
         "browserUsed": "VARCHAR", "content": "VARCHAR", "length": "BIGINT"}))
    v("post", "SELECT * FROM " + _csv(
        "dynamic/post_0_0.csv",
        {"id": "BIGINT", "imageFile": "VARCHAR", "creationDate": "BIGINT",
         "locationIP": "VARCHAR", "browserUsed": "VARCHAR",
         "language": "VARCHAR", "content": "VARCHAR", "length": "BIGINT"}))
    v("tag", "SELECT * FROM " + _csv(
        "static/tag_0_0.csv",
        {"id": "BIGINT", "name": "VARCHAR", "url": "VARCHAR"}))
    v("organisation", "SELECT * FROM " + _csv(
        "static/organisation_0_0.csv",
        {"id": "BIGINT", "type": "VARCHAR", "name": "VARCHAR",
         "url": "VARCHAR"}))
    v("place", "SELECT * FROM " + _csv(
        "static/place_0_0.csv",
        {"id": "BIGINT", "name": "VARCHAR", "url": "VARCHAR",
         "type": "VARCHAR"}))

    # person_knows_person stores each friendship ONCE (one direction).
    # GQL's ~[:knows]~ is undirected, so the `knows` view exposes the
    # symmetric closure via UNION ALL — both orientations of each edge.
    v("knows_raw", "SELECT * FROM " + _csv(
        "dynamic/person_knows_person_0_0.csv",
        {"src": "BIGINT", "dst": "BIGINT", "creationDate": "BIGINT"}))
    v("knows",
      "SELECT src, dst FROM knows_raw "
      "UNION ALL SELECT dst AS src, src AS dst FROM knows_raw")

    v("comment_hasCreator", "SELECT * FROM " + _csv(
        "dynamic/comment_hasCreator_person_0_0.csv",
        {"commentId": "BIGINT", "personId": "BIGINT"}))
    v("post_hasCreator", "SELECT * FROM " + _csv(
        "dynamic/post_hasCreator_person_0_0.csv",
        {"postId": "BIGINT", "personId": "BIGINT"}))
    v("post_hasTag", "SELECT * FROM " + _csv(
        "dynamic/post_hasTag_tag_0_0.csv",
        {"postId": "BIGINT", "tagId": "BIGINT"}))
    v("workAt", "SELECT * FROM " + _csv(
        "dynamic/person_workAt_organisation_0_0.csv",
        {"personId": "BIGINT", "organisationId": "BIGINT",
         "workFrom": "BIGINT"}))
    v("organisation_isLocatedIn", "SELECT * FROM " + _csv(
        "static/organisation_isLocatedIn_place_0_0.csv",
        {"organisationId": "BIGINT", "placeId": "BIGINT"}))
    return ddl


def load_toml(path: Path) -> dict:
    import tomllib
    with path.open("rb") as f:
        return tomllib.load(f)


def shape_of_value(v) -> str:
    """Mirror of `shape_of_value` in src/bin/ldbc_bench.rs — keep in
    sync. DuckDB's fetchall() returns Python-native types (int / float
    / str / None / list), so the standard mapping applies.
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


def shape_of_rows(rows: list, n_columns: int) -> str:
    if not rows:
        return "empty"
    cols: list[set[str]] = [set() for _ in range(n_columns)]
    for r in rows:
        for i in range(min(n_columns, len(r))):
            cols[i].add(shape_of_value(r[i]))
    return ",".join("/".join(sorted(s)) for s in cols)


def load_query(path: Path) -> str:
    """Read the SQL query, stripping leading -- comment lines so they
    don't get sent to the engine.
    """
    out_lines = []
    in_comment_block = True
    for line in path.read_text(encoding="utf-8").splitlines():
        if in_comment_block and (line.startswith("--") or not line.strip()):
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


def run_query(conn, query: str, bindings: dict) -> tuple[list[str], list]:
    """Execute, return (column_names, data_rows). DuckDB's
    `Connection.execute(sql, params)` binds `$name` placeholders from
    a dict whose keys do NOT include the `$` prefix — same convention
    as Kuzu. fetchall() returns tuples of Python-native values.
    """
    cur = conn.execute(query, bindings)
    cols = [d[0] for d in cur.description]
    rows = cur.fetchall()
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
    query_file = HERE / f"ic{ic}.sql"

    if not toml_path.is_file():
        sys.stderr.write(f"  toml missing: {toml_path}\n")
        return 1
    if not query_file.is_file():
        sys.stderr.write(
            f"  SQL translation missing: {query_file}\n"
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

    if not CSV_DIR.is_dir():
        sys.stderr.write(
            f"  LDBC CSV dir missing: {CSV_DIR}\n"
            f"  run ./target/release/bench_setup from the repo root first.\n"
            f"  (in-situ baseline: the CSVs ARE the database — no setup.py)\n"
        )
        return 1

    columns = derive_columns(toml.get("return_columns", []))
    query_label = f"IC{ic}"

    query = load_query(query_file)
    header, params_rows = load_params(params_file)
    sys.stderr.write(
        f"  duckdb ic{ic}: {len(params_rows)} param rows × {args.iters} iters "
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

    # In-memory connection; the only state is view DDL (pure metadata,
    # no data is loaded — each query re-scans the CSVs it references).
    conn = duckdb.connect()
    t_views = time.perf_counter_ns()
    for stmt in view_ddl():
        conn.execute(stmt)
    sys.stderr.write(
        f"  view DDL (metadata only): "
        f"{(time.perf_counter_ns() - t_views) / 1e6:.1f} ms\n"
    )

    if _proc is not None:
        cur = _proc.memory_info().rss / (1024 * 1024)
        sys.stderr.write(
            f"  RSS after open: {cur:.1f} MiB (+{cur - _rss_baseline_mib:.1f} MiB)\n"
        )

    peak_rss_mib = _rss_baseline_mib

    # Row-equivalence dump path: sibling JSONL alongside the CSV.
    rows_jsonl = args.out_csv.with_suffix(".rows.jsonl")
    if rows_jsonl.exists():
        rows_jsonl.unlink()  # fresh per run

    with args.out_csv.open("w", encoding="utf-8", newline="") as out:
        out.write("query;backend;params;row;iter;result_count;elapsed_ns\n")

        for row_idx, raw_row in enumerate(params_rows):
            # DuckDB's parameter dict keys do NOT include the `$`
            # prefix — the engine maps `$name` in the query to `name`
            # in the dict. Same convention as Kuzu.
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
            # oracle. fetchall() returns positional tuples — pass them
            # straight through to the canonicalizer.
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
