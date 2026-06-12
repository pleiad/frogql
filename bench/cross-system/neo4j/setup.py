#!/usr/bin/env python3
"""Load the FULL LDBC SNB SF0.1 dataset into a running Neo4j server.

This is the cross-system bench's "set up once per system" loader for
Neo4j (community, neo4j:5 via Docker — see docker.sh). After this
finishes, the server's default `neo4j` database holds every node and
edge type LDBC SF0.1 has, ready to query for any IC translation
(`ic1.cypher` ... `ic13.cypher`). The IC-specific work is in the
runner; setup is IC-agnostic.

Data model — same logical model as kuzu/setup.py:
  Node labels : Person, Comment, Post, Forum, Organisation, Place,
                Tag, TagClass — property names identical to the LDBC
                CSV headers (id, firstName, creationDate, ...).
                Sub-types (Company/University, Country/City/Continent)
                stay flat as the `type` property; queries filter on it.
  Rel types   : knows, hasCreator, hasTag, hasMember, hasModerator,
                containerOf, replyOf, isLocatedIn, isPartOf, hasType,
                isSubclassOf, hasInterest, studyAt, workAt, likes —
                lowercase per loader convention, same as every other
                system in the bench.
  Dates       : LongDateFormatter CSVs carry epoch-millis ints; we
                store them AS ints (no Neo4j temporal types) so the
                int arithmetic in the ICs ($startDate + days*86400000)
                and the row-hash oracle line up across systems.
  Person MVAs : `email` and `language` are pre-aggregated from their
                one-row-per-(person,value) CSVs into list properties
                on Person — same data model as gqlite's loader
                (Value::List) and kuzu's STRING[] columns.
  knows       : loaded single-direction (the CSV direction); queries
                match it undirected (`-[:knows]-`) so each pair counts
                once. Same convention as kuzu (its earlier
                both-directions variant double-counted and was caught
                by the row-hash oracle).
  Empty CSV fields (e.g. Post.imageFile, Post.content) are NOT stored
  — absent property == null, so COALESCE(content, imageFile) behaves
  exactly like gqlite's loader (which omits empty properties).

Load path: batched `UNWIND $rows` over bolt (~5000 rows/tx). Node-id
uniqueness constraints are created per label BEFORE loading so the
edge-phase `MATCH (a:Label {id: ...})` joins are index lookups, and
so the IC start-node lookups (`{id: $personId}`) are fair vs the
other systems' PK indexes.

--force semantics: data lives inside the container (no volume), and
`MATCH (n) DETACH DELETE n` is too slow for 1.8M elements in one tx.
We wipe with Neo4j 5's batched form instead:
    MATCH (n) CALL { WITH n DETACH DELETE n } IN TRANSACTIONS OF 10000 ROWS
run with an implicit (auto-commit) transaction. Equivalent and faster:
`bash docker.sh down && bash docker.sh up` (fresh container).

Usage:
  python setup.py [--force] [--csv-dir <path>]
Env:
  NEO4J_URI       (default bolt://localhost:7687)
  NEO4J_USER      (default neo4j)
  NEO4J_PASSWORD  (default benchbench)
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

DEFAULT_CSV_DIR = REPO_ROOT / "bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter"

NEO4J_URI = os.environ.get("NEO4J_URI", "bolt://localhost:7687")
NEO4J_USER = os.environ.get("NEO4J_USER", "neo4j")
NEO4J_PASSWORD = os.environ.get("NEO4J_PASSWORD", "benchbench")

BATCH = 5000

# Expected totals for SF0.1 (must match gqlite's import: 327 588
# nodes / 1 477 965 edges). Printed at the end for eyeballing.
EXPECTED_NODES = 327_588
EXPECTED_EDGES = 1_477_965


# ---- Schema ----------------------------------------------------------

NODE_LABELS = [
    "Person", "Comment", "Post", "Forum",
    "Organisation", "Place", "Tag", "TagClass",
]

# Per-label integer columns (everything else stays a string). Headers
# are taken from the CSVs themselves; this table only decides which
# get int-coerced. Dates are epoch-millis ints (LongDateFormatter).
INT_COLUMNS = {
    "Person": {"id", "birthday", "creationDate"},
    "Comment": {"id", "creationDate", "length"},
    "Post": {"id", "creationDate", "length"},
    "Forum": {"id", "creationDate"},
    "Tag": {"id"},
    "TagClass": {"id"},
    "Place": {"id"},
    "Organisation": {"id"},
}

# (csv_relpath, label) — node files. Person is handled separately
# (MVA aggregation).
NODE_CSVS = [
    ("static/organisation_0_0.csv", "Organisation"),
    ("static/place_0_0.csv", "Place"),
    ("static/tag_0_0.csv", "Tag"),
    ("static/tagclass_0_0.csv", "TagClass"),
    ("dynamic/comment_0_0.csv", "Comment"),
    ("dynamic/post_0_0.csv", "Post"),
    ("dynamic/forum_0_0.csv", "Forum"),
]

# (csv_relpath, rel_type, src_label, dst_label, [edge_prop or None])
# Mirrors kuzu/setup.py's edge_csvs incl. the multi-src/dst rels
# (hasCreator from Comment AND Post, likes to Comment AND Post, ...).
# knows is appended separately (single direction, see module doc).
EDGE_CSVS = [
    # Static
    ("static/organisation_isLocatedIn_place_0_0.csv",
     "isLocatedIn", "Organisation", "Place", None),
    ("static/place_isPartOf_place_0_0.csv",
     "isPartOf", "Place", "Place", None),
    ("static/tag_hasType_tagclass_0_0.csv",
     "hasType", "Tag", "TagClass", None),
    ("static/tagclass_isSubclassOf_tagclass_0_0.csv",
     "isSubclassOf", "TagClass", "TagClass", None),
    # Dynamic single-pair
    ("dynamic/forum_containerOf_post_0_0.csv",
     "containerOf", "Forum", "Post", None),
    ("dynamic/forum_hasMember_person_0_0.csv",
     "hasMember", "Forum", "Person", "joinDate"),
    ("dynamic/forum_hasModerator_person_0_0.csv",
     "hasModerator", "Forum", "Person", None),
    ("dynamic/person_hasInterest_tag_0_0.csv",
     "hasInterest", "Person", "Tag", None),
    ("dynamic/person_studyAt_organisation_0_0.csv",
     "studyAt", "Person", "Organisation", "classYear"),
    ("dynamic/person_workAt_organisation_0_0.csv",
     "workAt", "Person", "Organisation", "workFrom"),
    # Dynamic multi-pair (several CSVs share one rel type)
    ("dynamic/comment_hasCreator_person_0_0.csv",
     "hasCreator", "Comment", "Person", None),
    ("dynamic/post_hasCreator_person_0_0.csv",
     "hasCreator", "Post", "Person", None),
    ("dynamic/comment_hasTag_tag_0_0.csv",
     "hasTag", "Comment", "Tag", None),
    ("dynamic/post_hasTag_tag_0_0.csv",
     "hasTag", "Post", "Tag", None),
    ("dynamic/forum_hasTag_tag_0_0.csv",
     "hasTag", "Forum", "Tag", None),
    ("dynamic/comment_replyOf_comment_0_0.csv",
     "replyOf", "Comment", "Comment", None),
    ("dynamic/comment_replyOf_post_0_0.csv",
     "replyOf", "Comment", "Post", None),
    ("dynamic/comment_isLocatedIn_place_0_0.csv",
     "isLocatedIn", "Comment", "Place", None),
    ("dynamic/post_isLocatedIn_place_0_0.csv",
     "isLocatedIn", "Post", "Place", None),
    ("dynamic/person_isLocatedIn_place_0_0.csv",
     "isLocatedIn", "Person", "Place", None),
    ("dynamic/person_likes_comment_0_0.csv",
     "likes", "Person", "Comment", "creationDate"),
    ("dynamic/person_likes_post_0_0.csv",
     "likes", "Person", "Post", "creationDate"),
    # knows handled in main() — single direction, creationDate prop
]


# ---- CSV helpers ------------------------------------------------------

def read_csv_rows(path: Path):
    """Yield (header, row_values) pairs from a pipe-delimited LDBC CSV.
    LDBC values never contain the pipe delimiter, so a plain split is
    safe (same assumption as every other loader in this bench).
    """
    with path.open(encoding="utf-8") as f:
        header = f.readline().rstrip("\n\r").split("|")
        for line in f:
            line = line.rstrip("\n\r")
            if not line:
                continue
            yield header, line.split("|")


def node_props(header: list[str], values: list[str], label: str) -> dict:
    """CSV row → property dict. Int-coerce the per-label int columns;
    DROP empty fields entirely (absent property == null, matching
    gqlite's loader so COALESCE(content, imageFile) agrees).
    """
    ints = INT_COLUMNS[label]
    props = {}
    for col, val in zip(header, values):
        if val == "":
            continue
        props[col] = int(val) if col in ints else val
    return props


def aggregate_mva(path: Path) -> dict[int, list[str]]:
    """LDBC MVA file (`Person.id|<value>`, one row per pair) →
    `{person_id: [value, ...]}`. Values keep CSV order, same as
    gqlite's loader (list order is CSV order in both)."""
    out: dict[int, list[str]] = {}
    with path.open(encoding="utf-8") as f:
        next(f, None)  # header
        for line in f:
            parts = line.rstrip("\n\r").split("|")
            if len(parts) < 2:
                continue
            out.setdefault(int(parts[0]), []).append(parts[1])
    return out


# ---- Loaders ----------------------------------------------------------

def run_batched(session, cypher: str, rows: list[dict], what: str) -> int:
    """Send `rows` through `cypher` (an UNWIND $rows statement) in
    BATCH-sized write transactions. Returns total rows sent."""
    total = 0
    for i in range(0, len(rows), BATCH):
        chunk = rows[i:i + BATCH]
        session.execute_write(
            lambda tx, c=chunk: tx.run(cypher, rows=c).consume()
        )
        total += len(chunk)
    sys.stderr.write(f"      {what}: {total} rows\n")
    return total


def create_constraints(session) -> None:
    """Uniqueness constraint on `id` per node label, BEFORE loading.
    Gives the edge-phase MATCH joins (and the ICs' start-node lookups)
    a real index — the analog of Kuzu's PRIMARY KEY(id)."""
    for label in NODE_LABELS:
        session.run(
            f"CREATE CONSTRAINT {label.lower()}_id IF NOT EXISTS "
            f"FOR (n:{label}) REQUIRE n.id IS UNIQUE"
        ).consume()


def load_node_csv(session, csv_path: Path, label: str) -> int:
    rows = [node_props(h, v, label) for h, v in read_csv_rows(csv_path)]
    cypher = f"UNWIND $rows AS row CREATE (n:{label}) SET n = row"
    return run_batched(session, cypher, rows, label)


def load_person(session, csv_dir: Path) -> int:
    """Person with the two MVAs pre-aggregated into list properties."""
    emails = aggregate_mva(csv_dir / "dynamic" / "person_email_emailaddress_0_0.csv")
    speaks = aggregate_mva(csv_dir / "dynamic" / "person_speaks_language_0_0.csv")
    rows = []
    for h, v in read_csv_rows(csv_dir / "dynamic" / "person_0_0.csv"):
        props = node_props(h, v, "Person")
        pid = props["id"]
        if pid in emails:
            props["email"] = emails[pid]
        if pid in speaks:
            props["language"] = speaks[pid]
        rows.append(props)
    cypher = "UNWIND $rows AS row CREATE (n:Person) SET n = row"
    return run_batched(session, cypher, rows, "Person (with email/language MVAs)")


def load_edge_csv(session, csv_path: Path, rel: str,
                  src_label: str, dst_label: str,
                  prop: str | None) -> int:
    rows = []
    for _h, v in read_csv_rows(csv_path):
        row = {"src": int(v[0]), "dst": int(v[1])}
        if prop is not None:
            row["p"] = int(v[2])
        rows.append(row)
    set_clause = f" SET r.{prop} = row.p" if prop is not None else ""
    cypher = (
        f"UNWIND $rows AS row "
        f"MATCH (a:{src_label} {{id: row.src}}) "
        f"MATCH (b:{dst_label} {{id: row.dst}}) "
        f"CREATE (a)-[r:{rel}]->(b)" + set_clause
    )
    return run_batched(
        session, cypher, rows, f"{rel} ({src_label}->{dst_label})"
    )


def wipe(session) -> None:
    """Batched wipe — Neo4j 5's CALL { ... } IN TRANSACTIONS form,
    which must run in an implicit (auto-commit) transaction. A plain
    `MATCH (n) DETACH DELETE n` blows the single-tx memory budget at
    1.8M elements. (`docker.sh down && up` is the even simpler wipe.)
    """
    session.run(
        "MATCH (n) "
        "CALL { WITH n DETACH DELETE n } IN TRANSACTIONS OF 10000 ROWS"
    ).consume()
    # Constraints are idempotent (IF NOT EXISTS), no need to drop.


# ---- Driver -----------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--force", action="store_true",
                    help="wipe the database and reload")
    ap.add_argument("--csv-dir", type=Path, default=DEFAULT_CSV_DIR,
                    help="LDBC dataset root (with static/ and dynamic/)")
    args = ap.parse_args()

    csv_dir: Path = args.csv_dir
    if not (csv_dir / "static").is_dir() or not (csv_dir / "dynamic").is_dir():
        print(
            f"CSV dir missing static/ or dynamic/ subdir: {csv_dir}\n"
            f"  Run ./target/release/bench_setup from the repo root first.",
            file=sys.stderr,
        )
        return 1

    driver = GraphDatabase.driver(NEO4J_URI, auth=(NEO4J_USER, NEO4J_PASSWORD))
    try:
        driver.verify_connectivity()
    except Exception as e:
        print(
            f"cannot reach Neo4j at {NEO4J_URI}: {e}\n"
            f"  start it with: bash {HERE / 'docker.sh'} up",
            file=sys.stderr,
        )
        return 1

    with driver.session(database="neo4j") as session:
        existing = session.run("MATCH (n) RETURN count(n) AS c").single()["c"]
        if existing > 0 and not args.force:
            print(
                f"  cached: {existing} nodes already loaded at {NEO4J_URI} "
                f"(pass --force to wipe + reload)",
                file=sys.stderr,
            )
            if existing == EXPECTED_NODES:
                marker = REPO_ROOT / "bench/data/cross-system/neo4j/.loaded"
                marker.parent.mkdir(parents=True, exist_ok=True)
                marker.write_text(f"{existing} nodes @ {NEO4J_URI}\n")
            return 0
        if existing > 0 and args.force:
            print(f"  wiping {existing} nodes (batched DETACH DELETE)...",
                  file=sys.stderr)
            t = time.perf_counter()
            wipe(session)
            print(f"    wiped in {time.perf_counter() - t:.1f}s",
                  file=sys.stderr)

        print(f"  loading {csv_dir} -> {NEO4J_URI}", file=sys.stderr)
        t0 = time.perf_counter()

        print("    creating uniqueness constraints (id per label)...",
              file=sys.stderr)
        create_constraints(session)

        print("    loading nodes...", file=sys.stderr)
        t = time.perf_counter()
        load_person(session, csv_dir)
        for rel_path, label in NODE_CSVS:
            p = csv_dir / rel_path
            if not p.exists():
                print(f"      WARN: missing {p} (skipping)", file=sys.stderr)
                continue
            load_node_csv(session, p, label)
        print(f"    nodes loaded in {time.perf_counter() - t:.1f}s",
              file=sys.stderr)

        print("    loading edges...", file=sys.stderr)
        t = time.perf_counter()
        for rel_path, rel, src, dst, prop in EDGE_CSVS:
            p = csv_dir / rel_path
            if not p.exists():
                print(f"      WARN: missing {p} (skipping)", file=sys.stderr)
                continue
            load_edge_csv(session, p, rel, src, dst, prop)
        # knows: forward direction only; ICs match it undirected.
        load_edge_csv(
            session,
            csv_dir / "dynamic" / "person_knows_person_0_0.csv",
            "knows", "Person", "Person", "creationDate",
        )
        print(f"    edges loaded in {time.perf_counter() - t:.1f}s",
              file=sys.stderr)

        n_nodes = session.run("MATCH (n) RETURN count(n) AS c").single()["c"]
        n_edges = session.run(
            "MATCH ()-[r]->() RETURN count(r) AS c"
        ).single()["c"]
        elapsed = time.perf_counter() - t0
        ok = "" if (n_nodes, n_edges) == (EXPECTED_NODES, EXPECTED_EDGES) \
            else f" (EXPECTED {EXPECTED_NODES}/{EXPECTED_EDGES} — MISMATCH)"
        print(
            f"  done in {elapsed:.1f}s. {n_nodes} nodes, {n_edges} edges{ok}.",
            file=sys.stderr,
        )

        # Sentinel for run_all.sh's SETUP_MARKER check: the data lives
        # inside the docker container, so the orchestrator needs an
        # on-disk witness that a load completed. Written only on a
        # count-verified load.
        if (n_nodes, n_edges) == (EXPECTED_NODES, EXPECTED_EDGES):
            marker = REPO_ROOT / "bench/data/cross-system/neo4j/.loaded"
            marker.parent.mkdir(parents=True, exist_ok=True)
            marker.write_text(
                f"{n_nodes} nodes / {n_edges} edges @ {NEO4J_URI}\n"
            )

    driver.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
