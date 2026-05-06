#!/usr/bin/env python3
"""Load the FULL LDBC SNB SF0.1 dataset into a Kuzu database.

This is the cross-system bench's "set up once per system" loader.
After this finishes, `bench/data/cross-system/kuzu/ldbc-sf01.db/`
holds every node and edge type LDBC SF0.1 has, ready to query for
any IC translation we might add (`ic2.cypher`, `ic3.cypher`, ...).
The IC-specific work is in the runner; setup is IC-agnostic.

Loaded entities:
  Static  nodes: Organisation, Place, Tag, TagClass
  Dynamic nodes: Comment, Forum, Person, Post
  Static  edges: Organisation→Place, Place→Place, Tag→TagClass,
                 TagClass→TagClass
  Dynamic edges: knows, hasCreator, hasTag, hasMember, hasModerator,
                 containerOf, replyOf, isLocatedIn, hasInterest,
                 studyAt, workAt, likes
  Multi-Valued Attributes (MVAs) on Person: `email` and `language`
    are LDBC's two MVAs; their CSVs (`person_email_emailaddress_0_0.csv`,
    `person_speaks_language_0_0.csv`) have one row per (Person, value)
    pair. We pre-aggregate them to a `{person_id: [value, ...]}` dict
    and inject into the Person table as `email STRING[]` and
    `language STRING[]` columns via an UNWIND-based UPDATE pass after
    the initial COPY. Same data model as gqlite's LDBC loader (which
    surfaces these as `Value::List` properties on Person).

Throughput on this engine: COPY FROM is ~10-100K rows/sec depending
on column count. Full SF0.1 load (~327K nodes, ~1.5M edges) lands in
single-digit seconds.

Usage:
  python setup.py [--force] [--csv-dir <path>] [--db <path>]
"""

from __future__ import annotations

import argparse
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

DEFAULT_CSV_DIR = REPO_ROOT / "bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter"
DEFAULT_DB_DIR = REPO_ROOT / "bench/data/cross-system/kuzu"
DEFAULT_DB_NAME = "ldbc-sf01.db"


# ---- Schema ----------------------------------------------------------

# All node tables. Column order matches the LDBC CSV header order so
# COPY FROM consumes the CSV positionally without column-mapping. PK is
# `id` everywhere — Kuzu auto-indexes it for fast point lookups, which
# is what every IC's start-node MATCH (`{id: $personId}`) needs.
#
# `type` collides with no Kuzu reserved word in 0.11.x (verified by
# probe), so we use the LDBC column name verbatim.
NODE_SCHEMA = [
    # `email STRING[]` and `language STRING[]` are MVAs on Person.
    # The CSV person_0_0.csv doesn't have those columns; Kuzu's COPY
    # FROM requires every column to be present in the CSV (no DEFAULT
    # fill on COPY, verified by probe). So setup pre-aggregates the MVA
    # files into `{person_id: [...]}` dicts and writes an augmented
    # Person CSV (under bench/data/cross-system/kuzu/_kuzu_work/) with
    # the lists formatted as Kuzu LIST literals (`[a,b,c]`). Same data
    # model as gqlite's LDBC loader (Value::List on Person).
    ("Person",
     "id INT64, firstName STRING, lastName STRING, gender STRING, "
     "birthday INT64, creationDate INT64, locationIP STRING, "
     "browserUsed STRING, email STRING[], language STRING[]"),
    ("Comment",
     "id INT64, creationDate INT64, locationIP STRING, "
     "browserUsed STRING, content STRING, length INT64"),
    ("Post",
     "id INT64, imageFile STRING, creationDate INT64, locationIP STRING, "
     "browserUsed STRING, language STRING, content STRING, length INT64"),
    ("Forum",
     "id INT64, title STRING, creationDate INT64"),
    ("Tag",
     "id INT64, name STRING, url STRING"),
    ("TagClass",
     "id INT64, name STRING, url STRING"),
    ("Place",
     "id INT64, name STRING, url STRING, type STRING"),
    ("Organisation",
     "id INT64, type STRING, name STRING, url STRING"),
]


# Single-typed REL TABLEs: one FROM-TO pair, optional edge attributes.
# (rel_name, FROM, TO, [attribute_decl_string])
REL_SCHEMA_SINGLE = [
    # knows is undirected per LDBC. Kuzu has no undirected REL TABLE;
    # we materialize both directions in load_edges_knows.
    ("knows",        "Person",   "Person",       "creationDate INT64"),
    ("hasModerator", "Forum",    "Person",       None),
    ("hasMember",    "Forum",    "Person",       "joinDate INT64"),
    ("containerOf",  "Forum",    "Post",         None),
    ("hasInterest",  "Person",   "Tag",          None),
    ("studyAt",      "Person",   "Organisation", "classYear INT64"),
    ("workAt",       "Person",   "Organisation", "workFrom INT64"),
    ("hasType",      "Tag",      "TagClass",     None),
    ("isSubclassOf", "TagClass", "TagClass",     None),
    ("isPartOf",     "Place",    "Place",        None),
]


# Multi-typed REL TABLEs: list of FROM-TO pairs sharing one rel name.
# Kuzu accepts multiple FROM-TO clauses in one REL TABLE declaration.
# COPY FROM into multi-typed REL TABLEs requires explicit FROM/TO
# hints per-CSV.
REL_SCHEMA_MULTI = [
    # (rel_name, [(FROM, TO), ...], optional_attribute_decl)
    ("hasCreator",
     [("Comment", "Person"), ("Post", "Person")],
     None),
    ("hasTag",
     [("Comment", "Tag"), ("Post", "Tag"), ("Forum", "Tag")],
     None),
    ("replyOf",
     [("Comment", "Comment"), ("Comment", "Post")],
     None),
    ("isLocatedIn",
     [("Comment", "Place"), ("Post", "Place"),
      ("Person", "Place"), ("Organisation", "Place")],
     None),
    ("likes",
     [("Person", "Comment"), ("Person", "Post")],
     "creationDate INT64"),
]


# ---- CSV → table mapping ---------------------------------------------

# Each per-system COPY FROM call is one (csv_path, target_table,
# optional_from_to_hint) tuple. We build the list at runtime since
# CSV directory is configurable.

def node_csvs(csv_dir: Path) -> list[tuple[Path, str]]:
    """All node CSVs to load EXCEPT Person, which needs MVA aggregation
    (handled separately by build_person_with_mvas)."""
    return [
        (csv_dir / "static"  / "organisation_0_0.csv", "Organisation"),
        (csv_dir / "static"  / "place_0_0.csv",        "Place"),
        (csv_dir / "static"  / "tag_0_0.csv",          "Tag"),
        (csv_dir / "static"  / "tagclass_0_0.csv",     "TagClass"),
        # Person is loaded via build_person_with_mvas, not from this list.
        (csv_dir / "dynamic" / "comment_0_0.csv",      "Comment"),
        (csv_dir / "dynamic" / "post_0_0.csv",         "Post"),
        (csv_dir / "dynamic" / "forum_0_0.csv",        "Forum"),
    ]


def aggregate_mva(path: Path) -> dict[str, list[str]]:
    """Read an LDBC MVA file (`Person.id|<value>` with one row per
    (person, value) pair) and return `{person_id_str: [value, ...]}`.
    Keep ids as strings — they're 64-bit and we don't need int math
    here, just pass-through to the Person CSV build.
    """
    out: dict[str, list[str]] = {}
    with path.open(encoding="utf-8") as f:
        next(f, None)  # header
        for line in f:
            parts = line.rstrip("\n\r").split("|")
            if len(parts) < 2:
                continue
            pid, value = parts[0], parts[1]
            out.setdefault(pid, []).append(value)
    return out


def format_kuzu_list(values: list[str]) -> str:
    """Encode a list of strings as a Kuzu CSV LIST literal: `[a,b,c]`.
    Empty list is `[]`. We don't quote individual values because the
    LDBC email and language values don't contain commas, brackets, or
    pipes (the outer delimiter). If the dataset ever introduces such
    values this needs Kuzu's CSV escape handling — TODO.
    """
    return "[" + ",".join(values) + "]"


def build_person_with_mvas(csv_dir: Path, work_dir: Path) -> Path:
    """Pre-aggregate the Person MVAs (email, language) and write an
    augmented person.csv with `email` and `language` columns appended.
    Returns the path of the new CSV. Lives under work_dir so it's
    regenerated each setup but never gitignored-tracked.
    """
    person_csv = csv_dir / "dynamic" / "person_0_0.csv"
    email_csv = csv_dir / "dynamic" / "person_email_emailaddress_0_0.csv"
    speaks_csv = csv_dir / "dynamic" / "person_speaks_language_0_0.csv"

    emails = aggregate_mva(email_csv) if email_csv.exists() else {}
    speaks = aggregate_mva(speaks_csv) if speaks_csv.exists() else {}

    work_dir.mkdir(parents=True, exist_ok=True)
    out = work_dir / "person_with_mvas.csv"
    with person_csv.open(encoding="utf-8") as f_in, \
         out.open("w", encoding="utf-8", newline="") as f_out:
        header = f_in.readline().rstrip("\n\r")
        f_out.write(header + "|email|language\n")
        for line in f_in:
            line = line.rstrip("\n\r")
            if not line:
                continue
            pid = line.split("|", 1)[0]
            f_out.write(
                f"{line}|{format_kuzu_list(emails.get(pid, []))}"
                f"|{format_kuzu_list(speaks.get(pid, []))}\n"
            )
    return out


# Edge CSVs. For multi-typed REL TABLEs we pass FROM/TO hints so Kuzu
# knows which sub-table to insert into. Format:
#   (csv_path, rel_table_name, from_label_or_None, to_label_or_None)
# from_label is None for single-typed REL TABLEs (no hint needed).
def edge_csvs(csv_dir: Path) -> list[tuple[Path, str, str | None, str | None]]:
    static = csv_dir / "static"
    dyn = csv_dir / "dynamic"
    return [
        # Static edges — all single-typed
        (static / "organisation_isLocatedIn_place_0_0.csv",
         "isLocatedIn", "Organisation", "Place"),
        (static / "place_isPartOf_place_0_0.csv",
         "isPartOf",    None,           None),
        (static / "tag_hasType_tagclass_0_0.csv",
         "hasType",     None,           None),
        (static / "tagclass_isSubclassOf_tagclass_0_0.csv",
         "isSubclassOf", None,          None),
        # Dynamic single-typed
        (dyn / "forum_containerOf_post_0_0.csv",
         "containerOf",  None,          None),
        (dyn / "forum_hasMember_person_0_0.csv",
         "hasMember",    None,          None),
        (dyn / "forum_hasModerator_person_0_0.csv",
         "hasModerator", None,          None),
        (dyn / "person_hasInterest_tag_0_0.csv",
         "hasInterest",  None,          None),
        (dyn / "person_studyAt_organisation_0_0.csv",
         "studyAt",      None,          None),
        (dyn / "person_workAt_organisation_0_0.csv",
         "workAt",       None,          None),
        # Dynamic multi-typed (need FROM/TO hints)
        (dyn / "comment_hasCreator_person_0_0.csv",
         "hasCreator",   "Comment",     "Person"),
        (dyn / "post_hasCreator_person_0_0.csv",
         "hasCreator",   "Post",        "Person"),
        (dyn / "comment_hasTag_tag_0_0.csv",
         "hasTag",       "Comment",     "Tag"),
        (dyn / "post_hasTag_tag_0_0.csv",
         "hasTag",       "Post",        "Tag"),
        (dyn / "forum_hasTag_tag_0_0.csv",
         "hasTag",       "Forum",       "Tag"),
        (dyn / "comment_replyOf_comment_0_0.csv",
         "replyOf",      "Comment",     "Comment"),
        (dyn / "comment_replyOf_post_0_0.csv",
         "replyOf",      "Comment",     "Post"),
        (dyn / "comment_isLocatedIn_place_0_0.csv",
         "isLocatedIn",  "Comment",     "Place"),
        (dyn / "post_isLocatedIn_place_0_0.csv",
         "isLocatedIn",  "Post",        "Place"),
        (dyn / "person_isLocatedIn_place_0_0.csv",
         "isLocatedIn",  "Person",      "Place"),
        (dyn / "person_likes_comment_0_0.csv",
         "likes",        "Person",      "Comment"),
        (dyn / "person_likes_post_0_0.csv",
         "likes",        "Person",      "Post"),
        # MVAs — person_email_emailaddress, person_speaks_language —
        # not loaded. See module docstring.
    ]


# ---- Loaders ---------------------------------------------------------

def create_schema(conn) -> None:
    """All NODE TABLE + REL TABLE statements up-front, before any COPY.
    Multi-typed REL TABLE syntax is `CREATE REL TABLE <name>(FROM A TO B,
    FROM C TO D, [attrs])` — we synthesize that from REL_SCHEMA_MULTI.
    """
    for name, cols in NODE_SCHEMA:
        conn.execute(
            f"CREATE NODE TABLE {name}({cols}, PRIMARY KEY(id))"
        )
    for name, frm, to, attrs in REL_SCHEMA_SINGLE:
        attr_clause = f", {attrs}" if attrs else ""
        conn.execute(
            f"CREATE REL TABLE {name}(FROM {frm} TO {to}{attr_clause})"
        )
    for name, pairs, attrs in REL_SCHEMA_MULTI:
        pair_clauses = ", ".join(f"FROM {f} TO {t}" for f, t in pairs)
        attr_clause = f", {attrs}" if attrs else ""
        conn.execute(
            f"CREATE REL TABLE {name}({pair_clauses}{attr_clause})"
        )


def copy_node_csv(conn, csv_path: Path, table: str) -> int:
    """COPY FROM one node CSV. Returns the row count inserted."""
    csv_str = str(csv_path).replace("\\", "/")
    conn.execute(
        f"COPY {table} FROM '{csv_str}' (DELIM='|', HEADER=true)"
    )
    n = conn.execute(f"MATCH (n:{table}) RETURN count(n)").get_next()[0]
    return n


def copy_edge_csv(
    conn,
    csv_path: Path,
    rel: str,
    from_label: str | None,
    to_label: str | None,
) -> None:
    """COPY FROM one edge CSV. For multi-typed REL TABLEs, pass FROM/TO
    labels so Kuzu disambiguates which sub-table the rows belong to.
    """
    csv_str = str(csv_path).replace("\\", "/")
    if from_label is not None and to_label is not None:
        conn.execute(
            f"COPY {rel} FROM '{csv_str}' "
            f"(DELIM='|', HEADER=true, FROM='{from_label}', TO='{to_label}')"
        )
    else:
        conn.execute(
            f"COPY {rel} FROM '{csv_str}' (DELIM='|', HEADER=true)"
        )


def load_knows(conn, csv_dir: Path, work_dir: Path) -> None:
    """LDBC stores each `knows` pair once; the relationship is
    semantically undirected. We COPY only the forward CSV — Cypher's
    `(p)-[:knows]-(f)` (any-direction match) finds each stored edge
    in either direction → 1 row per pair, matching the spec.

    An earlier version of this loader generated a reversed CSV and
    COPYed both files. Combined with `-[:knows]-` matching each
    stored edge in either direction, that yielded 2× rows per pair.
    The cross-system row-content hash oracle caught the doubling
    against gqlite (which uses an undirected primitive `~[:knows]~`
    on single-direction storage). Same fix lives in
    bench/cross-system/graphqlite/setup.py.
    """
    _ = work_dir  # no longer needed; kept for callsite compatibility
    src = csv_dir / "dynamic" / "person_knows_person_0_0.csv"
    src_str = str(src).replace("\\", "/")
    conn.execute(f"COPY knows FROM '{src_str}' (DELIM='|', HEADER=true)")


# ---- Driver ----------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--force", action="store_true",
                    help="rebuild even if the cached db dir exists")
    ap.add_argument("--csv-dir", type=Path, default=DEFAULT_CSV_DIR,
                    help="LDBC dataset root (with static/ and dynamic/)")
    ap.add_argument("--db", type=Path, default=None,
                    help="output db directory; defaults to ldbc-sf01.db "
                         "under bench/data/cross-system/kuzu/")
    args = ap.parse_args()

    csv_dir: Path = args.csv_dir
    db_path: Path = args.db or (DEFAULT_DB_DIR / DEFAULT_DB_NAME)

    if not csv_dir.is_dir():
        print(f"CSV dir not found: {csv_dir}", file=sys.stderr)
        return 1
    if not (csv_dir / "static").is_dir() or not (csv_dir / "dynamic").is_dir():
        print(
            f"CSV dir missing static/ or dynamic/ subdir: {csv_dir}\n"
            f"  Expected layout: {csv_dir}/{{static,dynamic}}/*.csv\n"
            f"  Run ./target/release/bench_setup from the repo root first.",
            file=sys.stderr,
        )
        return 1

    if db_path.exists() and not args.force:
        print(f"  cached: {db_path} (pass --force to rebuild)", file=sys.stderr)
        return 0

    if db_path.exists() and args.force:
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
    create_schema(conn)

    work_dir = db_path.parent / "_kuzu_work"
    work_dir.mkdir(parents=True, exist_ok=True)

    print("    pre-aggregating Person MVAs (email, language)...",
          file=sys.stderr)
    t = time.perf_counter()
    person_aug = build_person_with_mvas(csv_dir, work_dir)
    print(
        f"      person_with_mvas.csv built in {time.perf_counter() - t:.2f}s",
        file=sys.stderr,
    )

    print("    loading nodes...", file=sys.stderr)
    # Person first (the augmented CSV with MVA list columns)
    t = time.perf_counter()
    n = copy_node_csv(conn, person_aug, "Person")
    print(
        f"      Person: {n} rows in {time.perf_counter() - t:.2f}s",
        file=sys.stderr,
    )
    for csv_path, table in node_csvs(csv_dir):
        if not csv_path.exists():
            print(f"      WARN: missing {csv_path} (skipping)", file=sys.stderr)
            continue
        t = time.perf_counter()
        n = copy_node_csv(conn, csv_path, table)
        print(
            f"      {table}: {n} rows in {time.perf_counter() - t:.2f}s",
            file=sys.stderr,
        )

    print("    loading edges...", file=sys.stderr)
    for csv_path, rel, frm, to in edge_csvs(csv_dir):
        if not csv_path.exists():
            print(f"      WARN: missing {csv_path} (skipping)", file=sys.stderr)
            continue
        hint = f" ({frm}→{to})" if frm else ""
        t = time.perf_counter()
        copy_edge_csv(conn, csv_path, rel, frm, to)
        print(
            f"      {rel}{hint}: loaded in {time.perf_counter() - t:.2f}s",
            file=sys.stderr,
        )

    # Forward-only :knows; `-[:knows]-` (any-direction match) finds
    # each stored edge in either direction → 1 row per pair.
    print("    loading knows (forward only)...", file=sys.stderr)
    t = time.perf_counter()
    load_knows(conn, csv_dir, work_dir)
    print(f"      knows: {time.perf_counter() - t:.2f}s", file=sys.stderr)

    # Final stats
    n_nodes = conn.execute("MATCH (n) RETURN count(n)").get_next()[0]
    n_edges = conn.execute("MATCH ()-[e]->() RETURN count(e)").get_next()[0]
    elapsed = time.perf_counter() - t0
    print(
        f"  done in {elapsed:.1f}s. {n_nodes} nodes, {n_edges} edges. "
        f"db at {db_path}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
