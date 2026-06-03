#!/usr/bin/env python3
"""Load the FULL LDBC SNB SF0.1 dataset into a Grafeo database.

The cross-system bench's "set up once per system" loader for Grafeo
(GrafeoDB/grafeo on PyPI, GQL-native). After this finishes,
`bench/data/cross-system/grafeo/ldbc-sf01.grafeo` holds every node and
edge type LDBC SF0.1 has, ready to query for any IC translation. The
IC-specific work is in the runner; setup is IC-agnostic.

Loading strategy — Grafeo's bulk path is `import_df` (pandas
DataFrame). `import_df(mode='nodes', label=L)` assigns sequential
internal node ids in DataFrame row order, continuing the global
counter across calls. We exploit that: for each node CSV we record
`{ldbc_id: grafeo_id}` as `dict(zip(df.id, range(base, base+n)))`
where `base = db.node_count` before the import. Edge CSVs reference
endpoints by (label, ldbc_id), so each edge file maps its two id
columns through the appropriate per-label map, then
`import_df(mode='edges', edge_type=T, source=..., target=...)`.

Divergences from the gqlite loader are documented in DIVERGENCES.md:
  - `knows` is stored forward-only (1 row per pair), matching gqlite's
    undirected `~[:knows]~` over single-direction storage. The runner's
    GQL uses any-direction `-[:knows]-`.
  - Person MVAs (email, language) are NOT loaded. None of the currently
    implemented ICs (2,5,6,8,9,11) reference them. See DIVERGENCES.md.
  - Empty string cells in `content` / `imageFile` are stored as NULL so
    `COALESCE(content, imageFile)` (IC2) picks the populated one, as in
    gqlite.

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
    import grafeo
except ImportError:
    sys.stderr.write(
        "grafeo not installed. From this directory:\n"
        "  pip install -r requirements.txt\n"
    )
    sys.exit(1)

try:
    import pandas as pd
except ImportError:
    sys.stderr.write(
        "pandas not installed (required by grafeo.import_df). From this directory:\n"
        "  pip install -r requirements.txt\n"
    )
    sys.exit(1)


HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent.parent

DEFAULT_CSV_DIR = REPO_ROOT / "bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter"
DEFAULT_DB_DIR = REPO_ROOT / "bench/data/cross-system/grafeo"
DEFAULT_DB_NAME = "ldbc-sf01.grafeo"

# Columns that are integers (epoch-millis dates, ids, counts) across the
# LDBC CSVs. Everything else loads as a string. Keeping these typed lets
# `message.creationDate <= $maxDate` compare as ints, matching gqlite.
INT_COLUMNS = {
    "id", "birthday", "creationDate", "length", "joinDate",
    "classYear", "workFrom",
}
# String columns whose empty cells must become NULL (not "") so COALESCE
# and `IS NULL` behave like gqlite's absent-property semantics.
NULLABLE_STR_COLUMNS = {"content", "imageFile", "language", "imageFile"}


# ---- node + edge schema (mirrors kuzu/setup.py's entity set) ---------

# (label, node_csv_relpath)
NODE_FILES = [
    ("Organisation", "static/organisation_0_0.csv"),
    ("Place",        "static/place_0_0.csv"),
    ("Tag",          "static/tag_0_0.csv"),
    ("TagClass",     "static/tagclass_0_0.csv"),
    ("Person",       "dynamic/person_0_0.csv"),
    ("Comment",      "dynamic/comment_0_0.csv"),
    ("Post",         "dynamic/post_0_0.csv"),
    ("Forum",        "dynamic/forum_0_0.csv"),
]

# (edge_type, csv_relpath, src_label, tgt_label)
# `knows` is loaded separately (forward-only). MVA files are skipped.
EDGE_FILES = [
    # static
    ("isLocatedIn",  "static/organisation_isLocatedIn_place_0_0.csv", "Organisation", "Place"),
    ("isPartOf",     "static/place_isPartOf_place_0_0.csv",           "Place",        "Place"),
    ("hasType",      "static/tag_hasType_tagclass_0_0.csv",           "Tag",          "TagClass"),
    ("isSubclassOf", "static/tagclass_isSubclassOf_tagclass_0_0.csv", "TagClass",     "TagClass"),
    # dynamic
    ("containerOf",  "dynamic/forum_containerOf_post_0_0.csv",        "Forum",        "Post"),
    ("hasMember",    "dynamic/forum_hasMember_person_0_0.csv",        "Forum",        "Person"),
    ("hasModerator", "dynamic/forum_hasModerator_person_0_0.csv",     "Forum",        "Person"),
    ("hasInterest",  "dynamic/person_hasInterest_tag_0_0.csv",        "Person",       "Tag"),
    ("studyAt",      "dynamic/person_studyAt_organisation_0_0.csv",   "Person",       "Organisation"),
    ("workAt",       "dynamic/person_workAt_organisation_0_0.csv",    "Person",       "Organisation"),
    ("hasCreator",   "dynamic/comment_hasCreator_person_0_0.csv",     "Comment",      "Person"),
    ("hasCreator",   "dynamic/post_hasCreator_person_0_0.csv",        "Post",         "Person"),
    ("hasTag",       "dynamic/comment_hasTag_tag_0_0.csv",            "Comment",      "Tag"),
    ("hasTag",       "dynamic/post_hasTag_tag_0_0.csv",               "Post",         "Tag"),
    ("hasTag",       "dynamic/forum_hasTag_tag_0_0.csv",              "Forum",        "Tag"),
    ("replyOf",      "dynamic/comment_replyOf_comment_0_0.csv",       "Comment",      "Comment"),
    ("replyOf",      "dynamic/comment_replyOf_post_0_0.csv",          "Comment",      "Post"),
    ("isLocatedIn",  "dynamic/comment_isLocatedIn_place_0_0.csv",     "Comment",      "Place"),
    ("isLocatedIn",  "dynamic/post_isLocatedIn_place_0_0.csv",        "Post",         "Place"),
    ("isLocatedIn",  "dynamic/person_isLocatedIn_place_0_0.csv",      "Person",       "Place"),
    ("likes",        "dynamic/person_likes_comment_0_0.csv",          "Person",       "Comment"),
    ("likes",        "dynamic/person_likes_post_0_0.csv",             "Person",       "Post"),
]


def read_csv(path: Path) -> "pd.DataFrame":
    """Read a pipe-delimited LDBC CSV. All cells start as strings; int
    columns are cast; empty cells in nullable string columns become
    None (NULL in Grafeo) so COALESCE/IS-NULL match gqlite."""
    df = pd.read_csv(path, sep="|", dtype=str, keep_default_na=False)
    for col in df.columns:
        if col in INT_COLUMNS:
            df[col] = df[col].astype("int64")
        elif col in NULLABLE_STR_COLUMNS:
            df[col] = df[col].map(lambda s: None if s == "" else s)
    return df


def load_nodes(db, csv_dir: Path) -> dict[str, dict[int, int]]:
    """Import every node CSV; return {label: {ldbc_id: grafeo_id}}."""
    id_maps: dict[str, dict[int, int]] = {}
    for label, rel in NODE_FILES:
        path = csv_dir / rel
        if not path.exists():
            sys.stderr.write(f"      WARN: missing {path} (skipping)\n")
            continue
        df = read_csv(path)
        base = db.node_count
        t = time.perf_counter()
        db.import_df(df, mode="nodes", label=label)
        # import_df assigns grafeo ids [base, base+len(df)) in df row order.
        id_maps[label] = dict(zip(df["id"].tolist(),
                                  range(base, base + len(df))))
        sys.stderr.write(
            f"      {label}: {len(df)} rows in {time.perf_counter() - t:.2f}s\n"
        )
    return id_maps


def load_edge_file(db, path: Path, edge_type: str,
                   src_map: dict[int, int], tgt_map: dict[int, int]) -> int:
    """Map a typed edge CSV's two id columns to grafeo ids and bulk
    import. The CSV has exactly two id columns (src, tgt) plus optional
    attribute columns."""
    df = read_csv(path)
    cols = list(df.columns)
    src_col, tgt_col = cols[0], cols[1]
    # Edge id columns are named "Person.id" / "Comment.id" etc. (not the
    # bare "id" INT_COLUMNS catches), so cast them here before mapping
    # LDBC ids -> grafeo internal ids.
    out = pd.DataFrame({
        "source": df[src_col].astype("int64").map(src_map),
        "target": df[tgt_col].astype("int64").map(tgt_map),
    })
    for attr in cols[2:]:
        out[attr] = df[attr]
    # Drop any row whose endpoint wasn't found (shouldn't happen on a
    # consistent dataset; guards against partial CSVs).
    before = len(out)
    out = out.dropna(subset=["source", "target"])
    if len(out) != before:
        sys.stderr.write(
            f"      WARN: {before - len(out)} {edge_type} rows dropped "
            f"(unmapped endpoint)\n"
        )
    out["source"] = out["source"].astype("int64")
    out["target"] = out["target"].astype("int64")
    db.import_df(out, mode="edges", edge_type=edge_type)
    return len(out)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--force", action="store_true",
                    help="rebuild even if the cached db exists")
    ap.add_argument("--csv-dir", type=Path, default=DEFAULT_CSV_DIR)
    ap.add_argument("--db", type=Path, default=None)
    args = ap.parse_args()

    csv_dir: Path = args.csv_dir
    db_path: Path = args.db or (DEFAULT_DB_DIR / DEFAULT_DB_NAME)

    if not csv_dir.is_dir():
        print(f"CSV dir not found: {csv_dir}", file=sys.stderr)
        return 1
    if not (csv_dir / "static").is_dir() or not (csv_dir / "dynamic").is_dir():
        print(
            f"CSV dir missing static/ or dynamic/: {csv_dir}\n"
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
    db = grafeo.GrafeoDB.open(str(db_path))

    print("    loading nodes...", file=sys.stderr)
    id_maps = load_nodes(db, csv_dir)

    print("    loading edges...", file=sys.stderr)
    for edge_type, rel, src_label, tgt_label in EDGE_FILES:
        path = csv_dir / rel
        if not path.exists():
            print(f"      WARN: missing {path} (skipping)", file=sys.stderr)
            continue
        src_map = id_maps.get(src_label, {})
        tgt_map = id_maps.get(tgt_label, {})
        t = time.perf_counter()
        n = load_edge_file(db, path, edge_type, src_map, tgt_map)
        print(
            f"      {edge_type} ({src_label}->{tgt_label}): {n} in "
            f"{time.perf_counter() - t:.2f}s",
            file=sys.stderr,
        )

    # knows — forward only (1 row per pair). The runner's GQL uses
    # any-direction -[:knows]- so each stored edge matches in either
    # direction, matching gqlite's undirected ~[:knows]~.
    print("    loading knows (forward only)...", file=sys.stderr)
    pmap = id_maps.get("Person", {})
    t = time.perf_counter()
    n = load_edge_file(
        db, csv_dir / "dynamic/person_knows_person_0_0.csv",
        "knows", pmap, pmap,
    )
    print(f"      knows: {n} in {time.perf_counter() - t:.2f}s", file=sys.stderr)

    # `open(path)` is already persistent — import_df writes land in the
    # opened DB. `save(path)` to the same path errors (file exists), so
    # flush via the WAL checkpoint instead when available.
    try:
        db.wal_checkpoint()
    except Exception:
        pass

    n_nodes = db.node_count
    n_edges = db.edge_count
    elapsed = time.perf_counter() - t0
    print(
        f"  done in {elapsed:.1f}s. {n_nodes} nodes, {n_edges} edges. "
        f"db at {db_path}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
