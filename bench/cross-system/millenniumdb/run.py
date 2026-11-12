#!/usr/bin/env python3
"""Run a chosen IC against MillenniumDB (GQL model) for every
substitution-param row, emitting per-iter CSV in the cross-system schema.

Output schema (matches src/bin/ldbc_bench.rs):
    query;backend;params;row;iter;result_count;elapsed_ns

`backend` is fixed to `millenniumdb-gql`. `params` is the raw pipe-joined
param row from the LDBC params file. `row` is the 0-based param-row index.

Per-IC inputs (all derived from --ic <n>):
    bench/ldbc-queries/ic<n>.toml              — query metadata
    bench/cross-system/millenniumdb/ic<n>.gql  — MillenniumDB GQL translation
    bench/data/substitution_parameters-sf0.1/.../<toml.params_file>

Shared input (one DB across all ICs):
    bench/data/cross-system/millenniumdb/ldbc-sf01.mdb — built by setup.py.

MillenniumDB is a server, not a library. The runner starts
`mdb server <db>` as its own child (same process group, so _lib/memrun.py
counts the server's RSS and kills it on a limit breach), sends each query
over HTTP (`POST /`, TSV result) and stops the server on exit. Each IC
pays one server start, outside the timed loop; the warmup iterations
absorb the cold page cache, as for every other system.

Why HTTP and not the official Python driver (`millenniumdb-driver`,
websocket): at commit d67f3e7 the server segfaults on the second query of
a driver session, so the driver cannot run a benchmark loop. HTTP is the
interface MillenniumDB's own test suite and `scripts/query` use. The
timed region covers the request, the full response and its parsing.

The TSV writer prints a string as `"<raw>"` with no escaping, NULL as a
bare `NULL`, numbers bare, and a list as `"[...]"`. Parsing that is only
unambiguous while no stored string holds a tab, a quote or a newline;
setup.py's SF0.1 dump holds none (checked), and a row whose cell count
does not match the header fails loudly rather than being misread.

Usage:
    python run.py <out_csv> [--ic <n>] [--iters N] [--warmup N]
    MDB_BIN=/path/to/mdb python run.py ...      # if mdb is not on PATH
"""

from __future__ import annotations

import argparse
import http.client
import re
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "_lib"))
from mdb_common import DB_DIR, DB_NAME, REPO_ROOT, find_mdb  # noqa: E402
from row_hash import append_rows_jsonl, canonicalize_and_hash  # noqa: E402

PARAMS_DIR = (
    REPO_ROOT
    / "bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1"
)
LDBC_QUERIES_DIR = REPO_ROOT / "bench/ldbc-queries"
BACKEND_LABEL = "millenniumdb-gql"
# Server-side per-query timeout. The orchestrator owns the real wall-clock
# cap (--timeout-s); this only has to be longer than it, so a slow query
# is killed by the harness and recorded as a timeout, not cut short here.
SERVER_QUERY_TIMEOUT_S = 3600


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
    """Read the GQL query, stripping the leading comment block (// or --)."""
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
    return lines[0].split("|"), [ln.split("|") for ln in lines[1:]]


def coerce(s: str) -> int | str:
    try:
        return int(s)
    except ValueError:
        return s


def inline_params(query: str, header: list[str], raw_row: list[str]) -> str:
    """Substitute every `$<col>` with its literal, as frogql's ldbc_bench and
    the grafeo runner do, so every engine receives the same literal query.
    Ints inline raw; strings are single-quoted. MillenniumDB's parser rejects
    the ISO `''` escape, so a string holding a quote or a backslash is
    refused rather than escaped some way the engine may read differently
    (no SF0.1 parameter contains either). Longest name first, with a
    non-identifier guard, so `$start` cannot shadow `$startDate`.

    Derived params (`$endDate` = startDate + durationDays days, in epoch
    millis) are computed here, because the toml spells that arithmetic with
    `DURATION(...)`, which these translations do not use.
    """
    values = {col: coerce(val) for col, val in zip(header, raw_row)}
    if "startDate" in values and "durationDays" in values:
        values["endDate"] = values["startDate"] + values["durationDays"] * 86_400_000
    out = query
    for col in sorted(values, key=len, reverse=True):
        v = values[col]
        if isinstance(v, int):
            literal = str(v)
        else:
            if "'" in v or "\\" in v:
                raise ValueError(f"cannot inline parameter {col}={v!r}: quote or backslash")
            literal = f"'{v}'"
        out = re.sub(
            r"\$" + re.escape(col) + r"(?![A-Za-z0-9_])",
            lambda _m, lit=literal: lit,
            out,
        )
    return out


_INT_CELL = re.compile(r"^-?[0-9]+$")


def parse_cell(s: str):
    """One TSV cell as MillenniumDB's GQL writer prints it."""
    if s == "NULL":
        return None
    if len(s) >= 2 and s[0] == '"' and s[-1] == '"':
        return s[1:-1]
    if s == "true":
        return True
    if s == "false":
        return False
    if _INT_CELL.match(s):
        return int(s)
    try:
        return float(s)
    except ValueError:
        # A bare node/edge id (`_n12`) or anything else unexpected: the
        # translation projected something that is not a value.
        raise ValueError(f"unparseable result cell {s!r}") from None


def parse_tsv(body: str) -> list[dict]:
    lines = body.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    if not lines:
        raise RuntimeError("empty response (no header line)")
    header = lines[0].split("\t")
    rows = []
    for ln in lines[1:]:
        cells = ln.split("\t")
        if len(cells) != len(header):
            raise RuntimeError(
                f"result row has {len(cells)} cells for {len(header)} columns: {ln[:200]!r}"
            )
        rows.append({h: parse_cell(c) for h, c in zip(header, cells)})
    return rows


def _ic13_path_length(rows: list[dict]) -> list[dict]:
    """IC13 asks for the length of the shortest path. MillenniumDB parses
    `PATH_LENGTH(p)` but has no implementation behind it: the server
    segfaults (exit 139). The translation returns the path's edge group
    `e` instead (printed `[_u1,_u2,...]`), and its length is the answer.
    Done inside the timed region, so the time still covers producing the
    number."""
    out = []
    for r in rows:
        inner = r["e"].strip()[1:-1]
        out.append({"shortestPathLength": len(inner.split(",")) if inner else 0})
    return out


# IC -> function applied to the driver's rows, inside the timed region.
# Only for computations the engine cannot express; see DIVERGENCES.md.
POSTPROCESS = {13: _ic13_path_length}


def derive_columns(return_columns: list[str]) -> list[str]:
    return [c.replace(".", "_") for c in return_columns]


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class MdbServer:
    """`mdb server` as a child process for the lifetime of one run."""

    def __init__(self, mdb: Path, db: Path, log_path: Path):
        self.port = free_port()
        self.log = log_path.open("w", encoding="utf-8")
        cmd = [
            str(mdb), "server", str(db),
            "--port", str(self.port),
            "--timeout", str(SERVER_QUERY_TIMEOUT_S),
            "--browser", "false",
        ]
        self.proc = subprocess.Popen(cmd, stdout=self.log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 300
        while True:
            if self.proc.poll() is not None:
                raise RuntimeError(
                    f"mdb server exited with {self.proc.returncode} during startup "
                    f"(log: {log_path})"
                )
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
                if s.connect_ex(("127.0.0.1", self.port)) == 0:
                    break
            if time.monotonic() > deadline:
                self.stop()
                raise RuntimeError(f"mdb server did not open port {self.port} in 300s")
            time.sleep(0.1)

    def alive(self) -> bool:
        return self.proc.poll() is None

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self.log.close()


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
            f"  (no MillenniumDB translation for IC{ic}; see DIVERGENCES.md)\n"
        )
        return 1

    toml = load_toml(toml_path)
    if toml.get("status") != "implemented":
        sys.stderr.write(
            f"  ic{ic}.toml status is {toml.get('status')!r}, not 'implemented'. Skipping.\n"
        )
        return 0

    params_file = PARAMS_DIR / toml["params_file"]
    if not params_file.is_file():
        sys.stderr.write(
            f"  params file missing: {params_file}\n"
            f"  run ./target/release/bench_setup from the repo root first.\n"
        )
        return 1
    if not db_path.exists():
        sys.stderr.write(
            f"  millenniumdb db missing: {db_path}\n"
            f"  run setup.py first:\n    python {HERE / 'setup.py'}\n"
        )
        return 1

    columns = derive_columns(toml.get("return_columns", []))
    query_label = f"IC{ic}"
    query = load_query(query_file)
    header, params_rows = load_params(params_file)
    mdb = find_mdb()
    sys.stderr.write(
        f"  millenniumdb ic{ic}: {len(params_rows)} param rows × {args.iters} iters "
        f"(+ {args.warmup} warmup), mdb={mdb}\n"
    )

    server_log = args.out_csv.with_suffix(".server.log")
    server = MdbServer(mdb, db_path, server_log)
    # The orchestrator's timeout sends SIGTERM to the process group; make
    # sure the server goes down with us on a plain SIGTERM too.
    signal.signal(signal.SIGTERM, lambda *_: (server.stop(), sys.exit(143)))
    sys.stderr.write(f"  mdb server up on port {server.port} (log: {server_log})\n")

    post = POSTPROCESS.get(ic)

    def run_once(q: str) -> list[dict]:
        conn = http.client.HTTPConnection("127.0.0.1", server.port, timeout=None)
        try:
            conn.request(
                "POST", "/", body=q.encode("utf-8"),
                headers={"Accept": "text/tab-separated-values"},
            )
            resp = conn.getresponse()
            text = resp.read().decode("utf-8")
        finally:
            conn.close()
        if resp.status != 200:
            raise RuntimeError(f"HTTP {resp.status}: {text[:500]}")
        rows = parse_tsv(text)
        return post(rows) if post else rows

    rows_jsonl = args.out_csv.with_suffix(".rows.jsonl")
    if rows_jsonl.exists():
        rows_jsonl.unlink()

    rc = 0
    try:
        with args.out_csv.open("w", encoding="utf-8", newline="") as out:
            out.write("query;backend;params;row;iter;result_count;elapsed_ns\n")
            for row_idx, raw_row in enumerate(params_rows):
                inlined = inline_params(query, header, raw_row)
                joined = "|".join(raw_row)

                for _ in range(args.warmup):
                    run_once(inlined)

                iter0_result = None
                elapsed_ns = 0
                for n in range(args.iters):
                    t = time.perf_counter_ns()
                    result = run_once(inlined)
                    elapsed_ns = time.perf_counter_ns() - t
                    out.write(
                        f"{query_label};{BACKEND_LABEL};{joined};{row_idx};{n};"
                        f"{len(result)};{elapsed_ns}\n"
                    )
                    if n == 0:
                        iter0_result = result
                out.flush()

                eff_columns = columns
                if not eff_columns and iter0_result:
                    eff_columns = list(iter0_result[0].keys())
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
                    rows_jsonl, ic, joined, row_idx, actual_count, rows_blob, row_hash,
                )
                sys.stderr.write(
                    f"  row {row_idx}: rc={actual_count} "
                    f"last_iter_ms={elapsed_ns / 1e6:.2f}\n"
                )
    except Exception as e:  # noqa: BLE001 — report and fail the cell
        crashed = "" if server.alive() else f" (mdb server died, exit {server.proc.returncode})"
        sys.stderr.write(f"  ERROR: {e}{crashed}\n")
        rc = 1
    finally:
        server.stop()

    sys.stderr.write(f"  done -> {args.out_csv}\n")
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
