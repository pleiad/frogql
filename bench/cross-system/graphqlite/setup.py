#!/usr/bin/env python3
"""Load the FULL LDBC SNB SF0.1 dataset into a graphqlite SQLite DB.

This is the cross-system bench's "set up once per system" loader.
After this finishes, `bench/data/cross-system/graphqlite/ldbc-sf01.db`
holds every node and edge type LDBC SF0.1 has, ready to query for any
IC translation we add (`ic2.cypher`, `ic3.cypher`, ...). The
IC-specific work is in the runner; setup is IC-agnostic.

Loaded entities:
  Static  nodes: Organisation, Place, Tag, TagClass
  Dynamic nodes: Comment, Forum, Person, Post
  Static  edges: Organisation→Place (isLocatedIn), Place→Place (isPartOf),
                 Tag→TagClass (hasType), TagClass→TagClass (isSubclassOf)
  Dynamic edges: knows, hasCreator, hasTag, hasMember, hasModerator,
                 containerOf, replyOf, isLocatedIn, hasInterest,
                 studyAt, workAt, likes
  Multi-Valued Attributes (MVAs) on Person: `email` and `language`.
    LDBC ships these as one row per (Person, value); we aggregate them
    and pass into `insert_nodes_bulk` as list-valued properties on
    Person. graphqlite stores properties as JSON in SQLite under the
    hood, so list values round-trip transparently.

API: graphqlite's `Graph.insert_nodes_bulk(nodes)` and
`insert_edges_bulk(edges, id_map)` are the bulk paths. `nodes` is
a list of `(external_id_str, props_dict, label_str)`. `edges` is
`(src_external_id_str, dst_external_id_str, props_dict, rel_type)`.
The combined id_map across all node types is what makes
`insert_edges_bulk` skip per-edge MATCH lookups — this is the
graphqlite-equivalent of an indexed start-node lookup, supplied
externally by the user rather than via Cypher CREATE INDEX.

External-id namespacing: LDBC IDs are unique within a label only
(Person id 933 ≠ Place id 933 conceptually, but they share the
integer "933"). `insert_edges_bulk` takes ONE flat external_id →
rowid map, so loading multiple label types with overlapping integer
IDs would silently corrupt the map. We prefix every external id by
its label — `"Person:933"`, `"Place:0"`, `"Tag:5"` — so the id_map
key space is per-label. Edge loaders know each CSV's source and
target label types from the filename, so they can construct the
right prefixed keys for lookup.

Why properties carry `ldbcId` instead of `id`: graphqlite reserves
the `.id` Cypher accessor for the loader-supplied external_id (the
first tuple element of `insert_nodes_bulk`). Setting `props["id"]`
in the dict is silently overwritten — `MATCH (p) RETURN p.id` always
returns the prefixed string `"Person:933"`, never the int we tried
to store. To return the raw LDBC int from queries, we store it
under a non-conflicting prop name `ldbcId`. IC translation files
(`ic2.cypher` etc.) use `friend.ldbcId` accordingly. This is a
graphqlite-specific divergence; gqlite and Kuzu use plain `.id`.
Documented in `DIVERGENCES.md`.

Throughput: graphqlite's bulk APIs do ~10K nodes/sec; full SF0.1
load (~327K nodes + ~1.5M edges) lands in roughly 1-3 minutes.

Usage:
  python setup.py [--force] [--csv-dir <path>] [--db <path>]
"""

from __future__ import annotations

import argparse
import csv
import os
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

DEFAULT_CSV_DIR = REPO_ROOT / "bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter"
DEFAULT_DB_DIR = REPO_ROOT / "bench/data/cross-system/graphqlite"
DEFAULT_DB_NAME = "ldbc-sf01.db"


# ---- MVA aggregation -----------------------------------------------

def aggregate_mva(path: Path) -> dict[str, list[str]]:
    """Read an LDBC MVA file (`Person.id|<value>` with one row per
    (person, value) pair) and return `{person_id_str: [value, ...]}`.
    Same idiom as the kuzu setup's MVA pre-aggregator.
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


# ---- Per-entity loaders --------------------------------------------

def _csv_dict_rows(path: Path):
    with path.open(encoding="utf-8") as f:
        yield from csv.DictReader(f, delimiter="|")


def _csv_pos_rows(path: Path):
    """Edge CSVs sometimes have repeated header columns
    (`Person.id|Person.id|...`) which DictReader collapses; use
    positional reading."""
    with path.open(encoding="utf-8") as f:
        reader = csv.reader(f, delimiter="|")
        next(reader, None)  # skip header
        for row in reader:
            if row:
                yield row


def _bulk_insert_nodes(g: Graph, label: str, nodes: list, id_map: dict) -> None:
    n_before = len(id_map)
    sub_map = g.insert_nodes_bulk(nodes)
    id_map.update(sub_map)
    print(
        f"      {label}: {len(nodes)} loaded ({len(id_map) - n_before} new ids)",
        file=sys.stderr,
    )


def _key(label: str, ext_id: str) -> str:
    """Prefixed external id for the id_map. See module docstring."""
    return f"{label}:{ext_id}"


def load_persons(g: Graph, csv_dir: Path, id_map: dict) -> None:
    """Person + MVAs (email, language). Aggregate MVAs first, then
    inject into the Person props dict as list values."""
    emails = aggregate_mva(csv_dir / "dynamic" / "person_email_emailaddress_0_0.csv")
    speaks = aggregate_mva(csv_dir / "dynamic" / "person_speaks_language_0_0.csv")
    nodes = []
    for r in _csv_dict_rows(csv_dir / "dynamic" / "person_0_0.csv"):
        pid = r["id"]
        nodes.append((_key("Person", pid), {
            "ldbcId": int(pid),
            "firstName": r["firstName"],
            "lastName": r["lastName"],
            "gender": r["gender"],
            "birthday": int(r["birthday"]),
            "creationDate": int(r["creationDate"]),
            "locationIP": r["locationIP"],
            "browserUsed": r["browserUsed"],
            "email": emails.get(pid, []),
            "language": speaks.get(pid, []),
        }, "Person"))
    _bulk_insert_nodes(g, "Person", nodes, id_map)


def load_comments(g: Graph, csv_dir: Path, id_map: dict) -> None:
    nodes = []
    for r in _csv_dict_rows(csv_dir / "dynamic" / "comment_0_0.csv"):
        nodes.append((_key("Comment", r["id"]), {
            "ldbcId": int(r["id"]),
            "creationDate": int(r["creationDate"]),
            "locationIP": r["locationIP"],
            "browserUsed": r["browserUsed"],
            "content": r.get("content", ""),
            "length": int(r["length"]),
        }, "Comment"))
    _bulk_insert_nodes(g, "Comment", nodes, id_map)


def load_posts(g: Graph, csv_dir: Path, id_map: dict) -> None:
    nodes = []
    for r in _csv_dict_rows(csv_dir / "dynamic" / "post_0_0.csv"):
        nodes.append((_key("Post", r["id"]), {
            "ldbcId": int(r["id"]),
            "imageFile": r.get("imageFile", ""),
            "creationDate": int(r["creationDate"]),
            "locationIP": r["locationIP"],
            "browserUsed": r["browserUsed"],
            "language": r.get("language", ""),
            "content": r.get("content", ""),
            "length": int(r["length"]),
        }, "Post"))
    _bulk_insert_nodes(g, "Post", nodes, id_map)


def load_forums(g: Graph, csv_dir: Path, id_map: dict) -> None:
    nodes = []
    for r in _csv_dict_rows(csv_dir / "dynamic" / "forum_0_0.csv"):
        nodes.append((_key("Forum", r["id"]), {
            "ldbcId": int(r["id"]),
            "title": r["title"],
            "creationDate": int(r["creationDate"]),
        }, "Forum"))
    _bulk_insert_nodes(g, "Forum", nodes, id_map)


def load_organisations(g: Graph, csv_dir: Path, id_map: dict) -> None:
    nodes = []
    for r in _csv_dict_rows(csv_dir / "static" / "organisation_0_0.csv"):
        nodes.append((_key("Organisation", r["id"]), {
            "ldbcId": int(r["id"]),
            "type": r["type"],
            "name": r["name"],
            "url": r["url"],
        }, "Organisation"))
    _bulk_insert_nodes(g, "Organisation", nodes, id_map)


def load_places(g: Graph, csv_dir: Path, id_map: dict) -> None:
    nodes = []
    for r in _csv_dict_rows(csv_dir / "static" / "place_0_0.csv"):
        nodes.append((_key("Place", r["id"]), {
            "ldbcId": int(r["id"]),
            "name": r["name"],
            "url": r["url"],
            "type": r["type"],
        }, "Place"))
    _bulk_insert_nodes(g, "Place", nodes, id_map)


def load_tags(g: Graph, csv_dir: Path, id_map: dict) -> None:
    nodes = []
    for r in _csv_dict_rows(csv_dir / "static" / "tag_0_0.csv"):
        nodes.append((_key("Tag", r["id"]), {
            "ldbcId": int(r["id"]),
            "name": r["name"],
            "url": r["url"],
        }, "Tag"))
    _bulk_insert_nodes(g, "Tag", nodes, id_map)


def load_tagclasses(g: Graph, csv_dir: Path, id_map: dict) -> None:
    nodes = []
    for r in _csv_dict_rows(csv_dir / "static" / "tagclass_0_0.csv"):
        nodes.append((_key("TagClass", r["id"]), {
            "ldbcId": int(r["id"]),
            "name": r["name"],
            "url": r["url"],
        }, "TagClass"))
    _bulk_insert_nodes(g, "TagClass", nodes, id_map)


# ---- Edge loaders --------------------------------------------------

def load_edges(
    g: Graph,
    path: Path,
    rel_type: str,
    src_label: str,
    dst_label: str,
    id_map: dict,
    edge_props_columns: list[str] | None = None,
) -> None:
    """Generic edge loader. `src_label`/`dst_label` are the LDBC
    label names of the endpoints (known from the CSV filename, e.g.
    `comment_hasCreator_person_0_0.csv` → src=Comment, dst=Person);
    they're used to prefix external ids when looking up in id_map.

    `edge_props_columns`, if provided, gives the list of property
    names from CSV columns 2..N (after src/dst). For LDBC the
    edge-attr columns are all numeric (creationDate, joinDate,
    classYear, workFrom).
    """
    edges = []
    rows = _csv_pos_rows(path)
    if edge_props_columns:
        for row in rows:
            props = {}
            for i, col in enumerate(edge_props_columns):
                v = row[2 + i]
                try:
                    props[col] = int(v)
                except ValueError:
                    props[col] = v
            edges.append((
                _key(src_label, row[0]),
                _key(dst_label, row[1]),
                props,
                rel_type,
            ))
    else:
        for row in rows:
            edges.append((
                _key(src_label, row[0]),
                _key(dst_label, row[1]),
                {},
                rel_type,
            ))
    n = g.insert_edges_bulk(edges, id_map)
    print(f"      {rel_type} ({src_label}→{dst_label}): {n} loaded",
          file=sys.stderr)


def load_knows(g: Graph, csv_dir: Path, id_map: dict) -> None:
    """LDBC `person_knows_person_0_0.csv` stores each (Person, friend)
    pair once; `:knows` is semantically undirected. We store ONLY the
    forward direction. Cypher's `-[:knows]-` (any-direction match)
    finds each stored edge in either direction → 1 row per pair, which
    is what the spec query "messages by your friends" asks for.

    An earlier version of this loader inserted both directions (`(a,b)`
    AND `(b,a)`) — combined with `-[:knows]-` matching the stored edge
    in either direction, that yielded 2× rows per pair. The
    cross-system row-content hash oracle caught it (gqlite uses an
    undirected primitive `~[:knows]~` on single-direction storage,
    which is the spec-faithful behaviour). Same fix lives in
    bench/cross-system/kuzu/setup.py."""
    edges = []
    for row in _csv_pos_rows(csv_dir / "dynamic" / "person_knows_person_0_0.csv"):
        cdate = int(row[2]) if len(row) > 2 else 0
        a, b = _key("Person", row[0]), _key("Person", row[1])
        edges.append((a, b, {"creationDate": cdate}, "knows"))
    n = g.insert_edges_bulk(edges, id_map)
    print(f"      knows: {n} edges (forward only; -[:knows]- matches both ways)",
          file=sys.stderr)


# ---- Driver --------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--force", action="store_true",
                    help="rebuild even if the cached db exists")
    ap.add_argument("--csv-dir", type=Path, default=DEFAULT_CSV_DIR,
                    help="LDBC dataset root (with static/ and dynamic/)")
    ap.add_argument("--db", type=Path, default=None,
                    help="output db path; defaults to ldbc-sf01.db "
                         "under bench/data/cross-system/graphqlite/")
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
        db_path.unlink()
    db_path.parent.mkdir(parents=True, exist_ok=True)

    print(f"  building {db_path} from {csv_dir}", file=sys.stderr)
    t0 = time.perf_counter()

    g = Graph(str(db_path))

    # Combined external_id → internal_rowid map across all node types.
    # `insert_edges_bulk` uses this to skip per-edge node lookups.
    id_map: dict[str, int] = {}

    print("    loading nodes...", file=sys.stderr)
    load_organisations(g, csv_dir, id_map)
    load_places(g, csv_dir, id_map)
    load_tags(g, csv_dir, id_map)
    load_tagclasses(g, csv_dir, id_map)
    load_persons(g, csv_dir, id_map)
    load_comments(g, csv_dir, id_map)
    load_posts(g, csv_dir, id_map)
    load_forums(g, csv_dir, id_map)

    print("    loading edges...", file=sys.stderr)
    # Static edges (no edge attrs)
    load_edges(g, csv_dir / "static" / "organisation_isLocatedIn_place_0_0.csv",
               "isLocatedIn", "Organisation", "Place", id_map)
    load_edges(g, csv_dir / "static" / "place_isPartOf_place_0_0.csv",
               "isPartOf", "Place", "Place", id_map)
    load_edges(g, csv_dir / "static" / "tag_hasType_tagclass_0_0.csv",
               "hasType", "Tag", "TagClass", id_map)
    load_edges(g, csv_dir / "static" / "tagclass_isSubclassOf_tagclass_0_0.csv",
               "isSubclassOf", "TagClass", "TagClass", id_map)

    # Dynamic edges (some have edge attrs)
    load_knows(g, csv_dir, id_map)
    load_edges(g, csv_dir / "dynamic" / "comment_hasCreator_person_0_0.csv",
               "hasCreator", "Comment", "Person", id_map)
    load_edges(g, csv_dir / "dynamic" / "post_hasCreator_person_0_0.csv",
               "hasCreator", "Post", "Person", id_map)
    load_edges(g, csv_dir / "dynamic" / "comment_hasTag_tag_0_0.csv",
               "hasTag", "Comment", "Tag", id_map)
    load_edges(g, csv_dir / "dynamic" / "post_hasTag_tag_0_0.csv",
               "hasTag", "Post", "Tag", id_map)
    load_edges(g, csv_dir / "dynamic" / "forum_hasTag_tag_0_0.csv",
               "hasTag", "Forum", "Tag", id_map)
    load_edges(g, csv_dir / "dynamic" / "comment_replyOf_comment_0_0.csv",
               "replyOf", "Comment", "Comment", id_map)
    load_edges(g, csv_dir / "dynamic" / "comment_replyOf_post_0_0.csv",
               "replyOf", "Comment", "Post", id_map)
    load_edges(g, csv_dir / "dynamic" / "comment_isLocatedIn_place_0_0.csv",
               "isLocatedIn", "Comment", "Place", id_map)
    load_edges(g, csv_dir / "dynamic" / "post_isLocatedIn_place_0_0.csv",
               "isLocatedIn", "Post", "Place", id_map)
    load_edges(g, csv_dir / "dynamic" / "person_isLocatedIn_place_0_0.csv",
               "isLocatedIn", "Person", "Place", id_map)
    load_edges(g, csv_dir / "dynamic" / "forum_containerOf_post_0_0.csv",
               "containerOf", "Forum", "Post", id_map)
    load_edges(g, csv_dir / "dynamic" / "forum_hasMember_person_0_0.csv",
               "hasMember", "Forum", "Person", id_map,
               edge_props_columns=["joinDate"])
    load_edges(g, csv_dir / "dynamic" / "forum_hasModerator_person_0_0.csv",
               "hasModerator", "Forum", "Person", id_map)
    load_edges(g, csv_dir / "dynamic" / "person_hasInterest_tag_0_0.csv",
               "hasInterest", "Person", "Tag", id_map)
    load_edges(g, csv_dir / "dynamic" / "person_studyAt_organisation_0_0.csv",
               "studyAt", "Person", "Organisation", id_map,
               edge_props_columns=["classYear"])
    load_edges(g, csv_dir / "dynamic" / "person_workAt_organisation_0_0.csv",
               "workAt", "Person", "Organisation", id_map,
               edge_props_columns=["workFrom"])
    load_edges(g, csv_dir / "dynamic" / "person_likes_comment_0_0.csv",
               "likes", "Person", "Comment", id_map,
               edge_props_columns=["creationDate"])
    load_edges(g, csv_dir / "dynamic" / "person_likes_post_0_0.csv",
               "likes", "Person", "Post", id_map,
               edge_props_columns=["creationDate"])

    # Bulk APIs commit per call (transactional internally), but a final
    # commit + close flushes the SQLite WAL and releases the file
    # handle cleanly.
    g.connection.commit()
    g.close()

    elapsed = time.perf_counter() - t0
    print(f"  done in {elapsed:.1f}s. db at {db_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
