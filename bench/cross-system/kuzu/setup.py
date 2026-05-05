#!/usr/bin/env python3
"""Load the IC2-relevant subset of LDBC SNB SF0.1 CSVs into a Kuzu
database (kuzudb on PyPI: `pip install kuzu==0.11.3`).

IC2 references:
  - Person (id, firstName, lastName) — Person table holds all 8 LDBC
    columns; only the three are queried.
  - Comment (id, creationDate, content) — table holds all 6 columns.
  - Post (id, creationDate, content) — table holds all 8 columns.
  - knows (Person—Person, undirected per LDBC; Kuzu doesn't have
    undirected REL TABLEs so we materialize both directions).
  - hasCreator (Comment→Person, Post→Person) — a multi-typed REL
    TABLE in Kuzu, declared with two FROM/TO pairs.

Output: `bench/data/cross-system/kuzu/ic2.db/` (Kuzu writes a
multi-file directory, not a single file).
Idempotent: skips if the directory already exists. Pass --force to
rebuild.

Loading idiom — taken from Kuzu's own LDBC examples and docs:
  1. CREATE NODE TABLE / CREATE REL TABLE (schema-first; PK is
     auto-indexed).
  2. COPY <Table> FROM '<csv>' (DELIM='|', HEADER=true) — native
     bulk loader. Multi-typed REL TABLEs need (FROM='X', TO='Y') hints
     so Kuzu knows which sub-table the rows belong to.

Throughput on this engine: ~10K nodes/sec via COPY FROM in our
microbench; SF0.1 (~289K nodes + ~315K edges) lands in ~30-60 sec
including the both-directions knows materialization. One-time cost.

Usage:
  python setup.py [--ic 2] [--force] [--csv-dir <path>] [--db <path>]
"""

from __future__ import annotations

import argparse
import os
import shutil
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

DEFAULT_CSV_DIR = REPO_ROOT / "bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter/dynamic"
DEFAULT_DB_DIR = REPO_ROOT / "bench/data/cross-system/kuzu"

SUPPORTED_ICS = {2}


def schema_statements() -> list[str]:
    """All schema DDL up-front. PK declarations get auto-indexed by
    Kuzu, so post-load `MATCH (p:Person {id: X})` is a btree-style
    point lookup — no full scan, no expression-index DDL needed.

    Kuzu requires every CSV column to map to a typed schema column,
    so even fields IC2 doesn't query (gender, browserUsed, etc.) are
    declared here so COPY FROM doesn't reject the rows. The runtime
    cost of carrying unused columns is trivial vs the alternative
    (pre-processing CSVs).
    """
    return [
        # All 8 Person columns from person_0_0.csv
        """CREATE NODE TABLE Person(
            id INT64,
            firstName STRING,
            lastName STRING,
            gender STRING,
            birthday INT64,
            creationDate INT64,
            locationIP STRING,
            browserUsed STRING,
            PRIMARY KEY(id)
        )""",
        # All 6 Comment columns from comment_0_0.csv
        """CREATE NODE TABLE Comment(
            id INT64,
            creationDate INT64,
            locationIP STRING,
            browserUsed STRING,
            content STRING,
            length INT64,
            PRIMARY KEY(id)
        )""",
        # All 8 Post columns from post_0_0.csv
        """CREATE NODE TABLE Post(
            id INT64,
            imageFile STRING,
            creationDate INT64,
            locationIP STRING,
            browserUsed STRING,
            language STRING,
            content STRING,
            length INT64,
            PRIMARY KEY(id)
        )""",
        # knows is undirected per LDBC; Kuzu has no undirected REL
        # TABLE primitive so we materialize both directions in the
        # loader (consistent with the convention used by every other
        # system in the cross-system bench). The CSV has 3 columns
        # (Person.id|Person.id|creationDate); we declare creationDate
        # on the edge to match.
        "CREATE REL TABLE knows(FROM Person TO Person, creationDate INT64)",
        # hasCreator is multi-typed (Comment→Person, Post→Person).
        # Kuzu supports this natively in one REL TABLE declaration.
        # Each COPY FROM into this table needs FROM/TO hints to
        # disambiguate.
        "CREATE REL TABLE hasCreator(FROM Comment TO Person, FROM Post TO Person)",
    ]


def load_nodes(conn, table: str, csv_path: Path) -> None:
    csv_str = str(csv_path).replace("\\", "/")
    t = time.perf_counter()
    conn.execute(
        f"COPY {table} FROM '{csv_str}' (DELIM='|', HEADER=true)"
    )
    elapsed = time.perf_counter() - t
    n = conn.execute(f"MATCH (n:{table}) RETURN count(n)").get_next()[0]
    print(f"    {table.lower()}s: {n} loaded in {elapsed:.2f}s", file=sys.stderr)


def load_knows_both_directions(conn, csv_path: Path, work_dir: Path) -> None:
    """LDBC's person_knows_person CSV stores each pair once. We need
    both directions for the IC2 undirected `(p)-[:knows]-(f)` pattern,
    so we generate a second CSV with src/dst swapped and COPY both.
    The reversed CSV lives under work_dir (gitignored bench/data/
    subtree) so it's regenerated each setup but never tracked.
    """
    t = time.perf_counter()
    csv_str = str(csv_path).replace("\\", "/")
    conn.execute(
        f"COPY knows FROM '{csv_str}' (DELIM='|', HEADER=true)"
    )
    elapsed_fwd = time.perf_counter() - t

    # Reversed CSV
    reversed_csv = work_dir / "person_knows_person_reversed.csv"
    work_dir.mkdir(parents=True, exist_ok=True)
    t = time.perf_counter()
    with csv_path.open(encoding="utf-8") as f_in, reversed_csv.open(
        "w", encoding="utf-8", newline=""
    ) as f_out:
        header = f_in.readline()
        f_out.write(header)
        for line in f_in:
            parts = line.rstrip("\n\r").split("|")
            if len(parts) < 2:
                continue
            # Swap src and dst, keep creationDate column unchanged.
            swapped = [parts[1], parts[0]] + parts[2:]
            f_out.write("|".join(swapped) + "\n")
    rev_str = str(reversed_csv).replace("\\", "/")
    conn.execute(
        f"COPY knows FROM '{rev_str}' (DELIM='|', HEADER=true)"
    )
    elapsed_rev = time.perf_counter() - t
    n = conn.execute("MATCH ()-[k:knows]->() RETURN count(k)").get_next()[0]
    print(
        f"    knows edges: {n} loaded "
        f"({elapsed_fwd:.2f}s fwd + {elapsed_rev:.2f}s reversed)",
        file=sys.stderr,
    )


def load_has_creator(conn, csv_path: Path, src_label: str) -> None:
    csv_str = str(csv_path).replace("\\", "/")
    t = time.perf_counter()
    conn.execute(
        f"COPY hasCreator FROM '{csv_str}' "
        f"(DELIM='|', HEADER=true, FROM='{src_label}', TO='Person')"
    )
    elapsed = time.perf_counter() - t
    print(
        f"    {src_label.lower()} hasCreator: loaded in {elapsed:.2f}s",
        file=sys.stderr,
    )


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
        # Kuzu writes a multi-file directory, not a single file.
        if db_path.is_dir():
            shutil.rmtree(db_path)
        else:
            db_path.unlink()
    db_path.parent.mkdir(parents=True, exist_ok=True)

    print(f"  building {db_path} from {csv_dir}", file=sys.stderr)
    t0 = time.perf_counter()

    db = kuzu.Database(str(db_path))
    conn = kuzu.Connection(db)

    print("    creating schema...", file=sys.stderr)
    for stmt in schema_statements():
        conn.execute(stmt)

    load_nodes(conn, "Person", csv_dir / "person_0_0.csv")
    load_nodes(conn, "Comment", csv_dir / "comment_0_0.csv")
    load_nodes(conn, "Post", csv_dir / "post_0_0.csv")

    work_dir = db_path.parent / "_kuzu_work"
    load_knows_both_directions(
        conn, csv_dir / "person_knows_person_0_0.csv", work_dir
    )
    load_has_creator(
        conn, csv_dir / "comment_hasCreator_person_0_0.csv", src_label="Comment"
    )
    load_has_creator(
        conn, csv_dir / "post_hasCreator_person_0_0.csv", src_label="Post"
    )

    elapsed = time.perf_counter() - t0
    print(f"  done in {elapsed:.1f}s. db at {db_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
