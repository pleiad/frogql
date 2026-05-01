#!/usr/bin/env python3
"""Load the IC2-relevant subset of LDBC SNB SF0.1 CSVs into a
graphqlite SQLite database.

IC2 only references these node/edge types:
  - Person nodes (id, firstName, lastName)
  - Comment nodes (id, creationDate, content)
  - Post nodes (id, creationDate, content)
  - knows edges (Person—Person, undirected per LDBC)
  - hasCreator edges (Comment→Person, Post→Person)

Other LDBC types (Forum, Tag, Place, etc.) are skipped — they're not
referenced by IC2, loading them would just slow down setup. If we
ever extend to other ICs we'd add them here.

The output DB is `bench/data/cross-system/graphqlite/ic2.db`.
Idempotent: skips if the file already exists. Delete it to force
re-load.

Cardinality on SF0.1 (~):
  Person    10k    knows         150k
  Comment  140k    comment→Person 140k
  Post      25k    post→Person    25k

Total: ~175k nodes + ~315k edges. Setup takes a few minutes on a
laptop; it's a one-time cost amortized over every bench run.

Usage:
  python setup.py                      # default paths
  python setup.py --force              # rebuild even if ic2.db exists
  python setup.py --csv-dir <path>     # override CSV source dir
"""

from __future__ import annotations

import argparse
import csv
import os
import sys
import time
from pathlib import Path

# Make sure graphqlite is importable; if not, fail with a clear hint.
try:
    from graphqlite import Graph
except ImportError:
    sys.stderr.write(
        "graphqlite not installed. From this directory:\n"
        "  pip install -r requirements.txt\n"
    )
    sys.exit(1)


# Repo root is three dirs up from this file
# (bench/cross-system/graphqlite/setup.py).
HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent.parent

DEFAULT_CSV_DIR = REPO_ROOT / "bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter/dynamic"
DEFAULT_DB_DIR = REPO_ROOT / "bench/data/cross-system/graphqlite"
DEFAULT_DB_PATH = DEFAULT_DB_DIR / "ic2.db"


# NOTE on API choice: graphqlite has both upsert_*_batch and
# insert_*_bulk. The former is documented as "calls upsert_node for
# each item" — a Python for-loop with one SQL statement per node;
# benchmarked at ~100 ops/s, so a full SF0.1 load would take ~1.5
# hours. The bulk variants are real transactional bulk inserts
# ("bypasses Cypher parsing entirely for maximum performance") and
# additionally return an external_id → internal_rowid map that
# `insert_edges_bulk` uses to skip per-edge node lookups. Different
# semantics: bulk = INSERT (no merge); upsert_*_batch = MERGE. We
# accept INSERT semantics because the LDBC dataset has no duplicate
# ids — each id appears once in its node file.


def load_persons(g: Graph, path: Path) -> dict[str, int]:
    """person_0_0.csv columns:
        id|firstName|lastName|gender|birthday|creationDate|locationIP|browserUsed
    Returns the external_id → internal_rowid map for use in edge insertion.
    """
    nodes: list[tuple[str, dict, str]] = []
    with path.open(encoding="utf-8") as f:
        reader = csv.DictReader(f, delimiter="|")
        for r in reader:
            nodes.append(
                (
                    r["id"],
                    {
                        "id": int(r["id"]),
                        "firstName": r["firstName"],
                        "lastName": r["lastName"],
                    },
                    "Person",
                )
            )
    print(f"    persons: inserting {len(nodes)}...", file=sys.stderr)
    id_map = g.insert_nodes_bulk(nodes)
    print(f"    persons: {len(nodes)} done", file=sys.stderr)
    return id_map


def load_messages(g: Graph, path: Path, label: str) -> dict[str, int]:
    """comment_0_0.csv / post_0_0.csv share most columns. Comment has:
        id|creationDate|locationIP|browserUsed|content|length
    Post has:
        id|imageFile|creationDate|locationIP|browserUsed|language|content|length
    Only IC2-relevant fields are id, creationDate, content. Both files have those.
    """
    nodes: list[tuple[str, dict, str]] = []
    with path.open(encoding="utf-8") as f:
        reader = csv.DictReader(f, delimiter="|")
        for r in reader:
            nodes.append(
                (
                    r["id"],
                    {
                        "id": int(r["id"]),
                        "creationDate": int(r["creationDate"]),
                        "content": r.get("content", ""),
                    },
                    label,
                )
            )
    print(f"    {label.lower()}s: inserting {len(nodes)}...", file=sys.stderr)
    id_map = g.insert_nodes_bulk(nodes)
    print(f"    {label.lower()}s: {len(nodes)} done", file=sys.stderr)
    return id_map


def load_edges(g: Graph, path: Path, rel_type: str, id_map: dict[str, int]) -> int:
    """Edge CSVs are pipe-delimited with source id in column 0 and target
    id in column 1. Some have repeated headers (`Person.id|Person.id|...`)
    so we read positionally with `csv.reader` instead of DictReader.

    The combined `id_map` (across all node types loaded so far) lets
    `insert_edges_bulk` resolve endpoints to internal rowids without a
    database lookup per edge.
    """
    edges: list[tuple[str, str, dict, str]] = []
    with path.open(encoding="utf-8") as f:
        reader = csv.reader(f, delimiter="|")
        next(reader, None)  # skip header
        for row in reader:
            if row:
                edges.append((row[0], row[1], {}, rel_type))
    print(f"    {rel_type} edges: inserting {len(edges)}...", file=sys.stderr)
    n = g.insert_edges_bulk(edges, id_map)
    print(f"    {rel_type} edges: {n} done", file=sys.stderr)
    return n


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--force", action="store_true",
                    help="rebuild even if the cached ic2.db exists")
    ap.add_argument("--csv-dir", type=Path, default=DEFAULT_CSV_DIR)
    ap.add_argument("--db", type=Path, default=DEFAULT_DB_PATH)
    args = ap.parse_args()

    csv_dir: Path = args.csv_dir
    db_path: Path = args.db

    if not csv_dir.is_dir():
        print(
            f"CSV dir not found: {csv_dir}\n"
            f"Run ./target/release/bench_setup from the repo root first.",
            file=sys.stderr,
        )
        return 1

    if db_path.exists() and not args.force:
        print(f"  cached: {db_path} (pass --force to rebuild)", file=sys.stderr)
        return 0

    if db_path.exists() and args.force:
        db_path.unlink()
    db_path.parent.mkdir(parents=True, exist_ok=True)

    print(f"  building {db_path} from {csv_dir}", file=sys.stderr)
    t0 = time.perf_counter()
    g = Graph(str(db_path))

    # Nodes first; merge their id_maps so edge insertion can resolve
    # both endpoints (Comment.id → Person.id needs both maps).
    id_map: dict[str, int] = {}
    id_map.update(load_persons(g, csv_dir / "person_0_0.csv"))
    id_map.update(load_messages(g, csv_dir / "comment_0_0.csv", label="Comment"))
    id_map.update(load_messages(g, csv_dir / "post_0_0.csv", label="Post"))

    # All three edge files have source.id in column 0 and target.id
    # in column 1, so the loader is positional.
    load_edges(g, csv_dir / "person_knows_person_0_0.csv",
               rel_type="knows", id_map=id_map)
    load_edges(g, csv_dir / "comment_hasCreator_person_0_0.csv",
               rel_type="hasCreator", id_map=id_map)
    load_edges(g, csv_dir / "post_hasCreator_person_0_0.csv",
               rel_type="hasCreator", id_map=id_map)

    # Bulk APIs commit per call (they're transactional internally),
    # but a final commit + close is still good hygiene to flush any
    # SQLite WAL state and release the file handle cleanly.
    g.connection.commit()
    g.close()

    elapsed = time.perf_counter() - t0
    print(f"  done in {elapsed:.1f}s. db at {db_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
