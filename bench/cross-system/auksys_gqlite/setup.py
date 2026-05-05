#!/usr/bin/env python3
"""Load the IC2-relevant subset of LDBC SNB SF0.1 CSVs into an
auksys/gqlite SQLite-backed database (gqlitedb on PyPI, gqlite.org).

IC2 only references these node/edge types:
  - Person nodes (id, firstName, lastName)
  - Comment nodes (id, creationDate, content)
  - Post nodes (id, creationDate, content)
  - knows edges (Person—Person, both directions materialized)
  - hasCreator edges (Comment→Person, Post→Person)

Output: `bench/data/cross-system/auksys_gqlite/ic2.db`. Idempotent;
pass --force to rebuild.

Loading idiom — taken from auksys/gqlite's own pokec benchmark
(`crates/gqlitedb/benches/common/pokec.rs` calls
`execute_oc_query(import_query)` with the whole content of
`pokec_*_import.cypher` as one string). The idiom is a SINGLE big
`CREATE` statement with comma-separated patterns. Variables bound
in node patterns (`(p_933:Person {id: 933, ...})`) are reused in
subsequent edge patterns (`(p_933)-[:knows]->(p_1129)`) WITHIN THE
SAME STATEMENT — no MATCH lookup needed, so the load is O(N) instead
of the O(N²) you get if you split nodes and edges into separate
statements that have to look up endpoints by property value.

We still chunk into ~10K-pattern statements to keep memory bounded
and to give progress feedback. Within each chunk we emit nodes and
THEIR adjacent edges together so cross-chunk edges still need a
MATCH — that's a small fraction of total edges in LDBC, kept
proportional via the chunk grouping. For SF0.1 we just emit all
nodes in chunk 1, all edges referencing them in chunks 2..N (where
the variable names are gone), but since we only emit edges in the
same chunk as both their endpoint variables, we can't do that
across chunks. So the simplest correct version is: ONE CHUNK per
load_*. We rely on gqlitedb being able to digest a multi-MB CREATE
(their own pokec_small_import.cypher is 132K lines / multi-MB).

Variable naming: `p_<ldbc_id>` for Person, `c_<ldbc_id>` for Comment,
`po_<ldbc_id>` for Post. Tracked because edges later reference these.

Usage:
  python setup.py [--ic 2] [--force] [--csv-dir <path>] [--db <path>]
"""

from __future__ import annotations

import argparse
import csv
import os
import sys
import time
from pathlib import Path

try:
    import gqlite  # noqa: F401
    if not hasattr(gqlite, "connect") or not hasattr(gqlite, "Connection"):
        raise ImportError("wrong gqlite package")
except ImportError:
    sys.stderr.write(
        "auksys/gqlite not importable. From this directory:\n"
        "  pip install -r requirements.txt\n"
        "(distribution is `gqlitedb` on PyPI; module is `gqlite`. Do NOT\n"
        " install the unrelated `gqlite` package — that's a GraphQL HTTP\n"
        " client that just shares the name.)\n"
    )
    sys.exit(1)


HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent.parent

DEFAULT_CSV_DIR = REPO_ROOT / "bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter/dynamic"
DEFAULT_DB_DIR = REPO_ROOT / "bench/data/cross-system/auksys_gqlite"

SUPPORTED_ICS = {2}


def quote(s: str) -> str:
    """Escape a string for inclusion as a Cypher single-quoted literal.
    Uses backslash-escape for both `\\` and `'` (auksys/gqlite's lexer
    follows OpenCypher's standard escape; doesn't accept SQL-style `''`).
    """
    out = ["'"]
    for c in s:
        if c == "\\":
            out.append("\\\\")
        elif c == "'":
            out.append("\\'")
        elif c == "\n":
            out.append("\\n")
        elif c == "\r":
            out.append("\\r")
        else:
            out.append(c)
    out.append("'")
    return "".join(out)


def emit_node(buf: list[str], var: str, label: str, props: dict) -> None:
    parts = []
    for k, v in props.items():
        if isinstance(v, str):
            parts.append(f"{k}: {quote(v)}")
        else:
            parts.append(f"{k}: {v}")
    buf.append(f"  ({var}:{label} {{{', '.join(parts)}}})")


def emit_edge(buf: list[str], src_var: str, rel: str, dst_var: str) -> None:
    buf.append(f"  ({src_var})-[:{rel}]->({dst_var})")


def read_csv_dict(path: Path):
    with path.open(encoding="utf-8") as f:
        yield from csv.DictReader(f, delimiter="|")


def read_csv_rows(path: Path):
    with path.open(encoding="utf-8") as f:
        reader = csv.reader(f, delimiter="|")
        next(reader, None)  # header
        for r in reader:
            if r:
                yield r


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--ic", type=int, default=2)
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--csv-dir", type=Path, default=DEFAULT_CSV_DIR)
    ap.add_argument("--db", type=Path, default=None)
    args = ap.parse_args()

    if args.ic not in SUPPORTED_ICS:
        print(
            f"setup.py only supports IC(s) {sorted(SUPPORTED_ICS)}; got ic{args.ic}",
            file=sys.stderr,
        )
        return 1

    csv_dir: Path = args.csv_dir
    db_path: Path = args.db or (DEFAULT_DB_DIR / f"ic{args.ic}.db")

    if not csv_dir.is_dir():
        print(f"CSV dir not found: {csv_dir}", file=sys.stderr)
        return 1

    if db_path.exists() and not args.force:
        print(f"  cached: {db_path} (pass --force to rebuild)", file=sys.stderr)
        return 0

    if db_path.exists() and args.force:
        db_path.unlink()
    db_path.parent.mkdir(parents=True, exist_ok=True)

    print(f"  building {db_path} from {csv_dir}", file=sys.stderr)
    t0 = time.perf_counter()

    # Build the single CREATE statement in memory. Each pattern is
    # one line in the buffer. We end up with ~600K patterns total and
    # ~50-100 MB string. That's the same scale as auksys's own
    # pokec_small_import.cypher (132K lines) — they ship that and
    # their CI runs it via execute_oc_query, so it's the supported path.
    print("    building Cypher import statement...", file=sys.stderr)
    t_build = time.perf_counter()
    buf: list[str] = ["CREATE"]

    # ---- Persons ----
    n_persons = 0
    for r in read_csv_dict(csv_dir / "person_0_0.csv"):
        var = f"p_{r['id']}"
        emit_node(buf, var, "Person", {
            "id": int(r["id"]),
            "firstName": r["firstName"],
            "lastName": r["lastName"],
        })
        n_persons += 1
    print(f"    persons: {n_persons} patterns staged", file=sys.stderr)

    # ---- Comments ----
    n_comments = 0
    for r in read_csv_dict(csv_dir / "comment_0_0.csv"):
        var = f"c_{r['id']}"
        emit_node(buf, var, "Comment", {
            "id": int(r["id"]),
            "creationDate": int(r["creationDate"]),
            "content": r.get("content", ""),
        })
        n_comments += 1
    print(f"    comments: {n_comments} patterns staged", file=sys.stderr)

    # ---- Posts ----
    n_posts = 0
    for r in read_csv_dict(csv_dir / "post_0_0.csv"):
        var = f"po_{r['id']}"
        emit_node(buf, var, "Post", {
            "id": int(r["id"]),
            "creationDate": int(r["creationDate"]),
            "content": r.get("content", ""),
        })
        n_posts += 1
    print(f"    posts: {n_posts} patterns staged", file=sys.stderr)

    # ---- knows edges (both directions) ----
    n_knows = 0
    for r in read_csv_rows(csv_dir / "person_knows_person_0_0.csv"):
        s, d = r[0], r[1]
        emit_edge(buf, f"p_{s}", "knows", f"p_{d}")
        emit_edge(buf, f"p_{d}", "knows", f"p_{s}")
        n_knows += 2
    print(f"    knows edges: {n_knows} patterns staged (both directions)",
          file=sys.stderr)

    # ---- hasCreator (Comment → Person) ----
    n_cc = 0
    for r in read_csv_rows(csv_dir / "comment_hasCreator_person_0_0.csv"):
        emit_edge(buf, f"c_{r[0]}", "hasCreator", f"p_{r[1]}")
        n_cc += 1
    print(f"    comment hasCreator edges: {n_cc} patterns staged",
          file=sys.stderr)

    # ---- hasCreator (Post → Person) ----
    n_pc = 0
    for r in read_csv_rows(csv_dir / "post_hasCreator_person_0_0.csv"):
        emit_edge(buf, f"po_{r[0]}", "hasCreator", f"p_{r[1]}")
        n_pc += 1
    print(f"    post hasCreator edges: {n_pc} patterns staged",
          file=sys.stderr)

    # Join with `,\n` between patterns. The first element is the bare
    # `CREATE` keyword (no comma after); patterns 1..N each get a
    # leading two-space indent already.
    query = buf[0] + "\n" + ",\n".join(buf[1:])
    n_total_patterns = len(buf) - 1
    qsize_mb = len(query) / (1024 * 1024)
    elapsed_build = time.perf_counter() - t_build
    print(
        f"    staged {n_total_patterns} patterns in {qsize_mb:.1f} MB query "
        f"({elapsed_build:.1f}s)",
        file=sys.stderr,
    )

    # Execute as one big query (auksys/gqlite's canonical bulk-load idiom).
    print("    executing single CREATE statement...", file=sys.stderr)
    t_exec = time.perf_counter()
    conn = gqlite.connect(str(db_path))
    conn.execute_oc_query(query)
    elapsed_exec = time.perf_counter() - t_exec
    print(f"    execute_oc_query: {elapsed_exec:.1f}s", file=sys.stderr)

    elapsed = time.perf_counter() - t0
    print(f"  done in {elapsed:.1f}s. db at {db_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
