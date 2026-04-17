#!/usr/bin/env python3
"""
Convert bench/data/text2gql/dataset/dataset/{dev,test,train}/ datasets
into examples/*.gdb databases + *_queries.json files.

- dev/ datasets use TuGraph format (import_config.json with schema[] + columns as lists).
  These need conversion to spanner format + CSV header renaming (_id -> vid).
- test/ and train/ datasets already have GQL/Spanner_Instance/ with spanner_import_config.json.
  These can be imported directly with gqlite --import-csv.

Query files (*_gql.json or *_cypher.json) are extracted where available.
Train datasets have no query files.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BASE_DIR = REPO / "bench" / "data" / "text2gql" / "dataset" / "dataset"
EXAMPLES_DIR = REPO / "examples"
GQLITE = REPO / "target" / "release" / "gqlite"


# ---------------------------------------------------------------------------
# Helpers: find directories and query files
# ---------------------------------------------------------------------------

def find_tugraph_dir(dataset_dir: Path) -> Path | None:
    """Find the *tugraph* subdirectory under Cypher/."""
    cypher_dir = dataset_dir / "Cypher"
    if not cypher_dir.exists():
        return None
    for d in cypher_dir.iterdir():
        if d.is_dir() and "tugraph" in d.name.lower():
            if (d / "import_config.json").exists():
                return d
    return None


def find_spanner_dir(dataset_dir: Path) -> Path | None:
    """Find a Spanner_Instance directory (under GQL/ or gql/)."""
    for subdir_name in ("GQL", "gql"):
        candidate = dataset_dir / subdir_name / "Spanner_Instance"
        if candidate.exists() and (candidate / "spanner_import_config.json").exists():
            return candidate
    return None


def find_query_json(dataset_dir: Path) -> Path | None:
    """Find *_gql.json (preferred) or *_cypher.json query file."""
    # Try GQL/ first for *_gql.json
    for subdir_name in ("GQL", "gql"):
        gql_dir = dataset_dir / subdir_name
        if gql_dir.exists():
            for f in gql_dir.iterdir():
                if f.name.endswith("_gql.json") and f.is_file():
                    return f
    # Fall back to Cypher/ for *_cypher.json
    cypher_dir = dataset_dir / "Cypher"
    if cypher_dir.exists():
        for f in cypher_dir.iterdir():
            if f.name.endswith("_cypher.json") and f.is_file():
                return f
    return None


def db_name_from_folder(name: str) -> str:
    """Convert folder name to lowercase db name for examples/."""
    return name.lower().replace("(", "").replace(")", "").replace(" ", "_")


def is_dropped_column(name: str) -> bool:
    """Columns we strip before importing: anything whose name looks like an embedding.
    Dense float vectors (768/1024-dim) exceed gqlite's 4KB page-cell limit and aren't
    useful for the kind of path queries these datasets benchmark."""
    return "embedding" in name.lower()


# ---------------------------------------------------------------------------
# TuGraph -> Spanner conversion (for dev/ datasets)
# ---------------------------------------------------------------------------

def build_type_map(schema: list) -> dict:
    """Build {label: {prop_name: prop_type}} from TuGraph schema."""
    type_map = {}
    for entry in schema:
        label = entry["label"]
        props = {}
        for p in entry.get("properties", []):
            props[p["name"]] = p.get("type", "STRING")
        type_map[label] = props
    return type_map


def convert_tugraph_type(tugraph_type: str) -> str:
    """Map TuGraph types to spanner config types."""
    t = tugraph_type.upper()
    if t in ("INT64", "INT32", "INT16", "INTEGER"):
        return "INT64"
    if t in ("DOUBLE", "FLOAT", "REAL"):
        return "FLOAT64"
    if t in ("BOOL", "BOOLEAN"):
        return "BOOL"
    return "STRING"


def generate_spanner_config(tugraph_config: dict) -> dict:
    """Convert TuGraph import_config.json to spanner_import_config.json format."""
    type_map = build_type_map(tugraph_config["schema"])
    spanner_files = []

    for file_entry in tugraph_config["files"]:
        is_edge = "SRC_ID" in file_entry
        label = file_entry["label"]
        columns_list = file_entry.get("columns", [])
        schema_types = type_map.get(label, {})

        columns_dict = {}
        for col in columns_list:
            if col == "_id":
                if not is_edge:
                    columns_dict["vid"] = "STRING"
                continue
            if col in ("SRC_ID", "DST_ID"):
                columns_dict[col] = "STRING"
                continue
            if is_dropped_column(col):
                continue
            col_type = schema_types.get(col, "STRING")
            columns_dict[col] = convert_tugraph_type(col_type)

        spanner_entry = {
            "path": file_entry["path"],
            "label": label,
            "format": "CSV",
            "header": 1,
            "columns": columns_dict,
        }
        spanner_files.append(spanner_entry)

    return {"files": spanner_files}


def transform_node_csv(src_path: Path, dst_path: Path):
    """Copy a node CSV, renaming _id → vid in the header and dropping embedding columns."""
    import csv as csv_mod
    with open(src_path, "r", encoding="utf-8", errors="replace") as fin:
        header_line = fin.readline().rstrip("\n").rstrip("\r")
        cols = [c.strip() for c in header_line.split(",")]
        keep_mask = [not is_dropped_column(c) for c in cols]
        new_cols = [("vid" if c == "_id" else c) for c, keep in zip(cols, keep_mask) if keep]
        with open(dst_path, "w", encoding="utf-8") as fout:
            fout.write(",".join(new_cols))
            fout.write("\n")
            reader = csv_mod.reader(fin)
            writer = csv_mod.writer(fout)
            for row in reader:
                writer.writerow([v for v, keep in zip(row, keep_mask) if keep])


# ---------------------------------------------------------------------------
# Import functions
# ---------------------------------------------------------------------------

def import_tugraph(dataset_dir: Path, db_name: str, verbose: bool) -> bool:
    """Convert TuGraph dataset and import via gqlite."""
    tugraph_dir = find_tugraph_dir(dataset_dir)
    if tugraph_dir is None:
        return False

    with open(tugraph_dir / "import_config.json") as f:
        tugraph_config = json.load(f)

    spanner_config = generate_spanner_config(tugraph_config)

    node_files = []
    edge_files = []
    for file_entry in tugraph_config["files"]:
        if "SRC_ID" in file_entry:
            edge_files.append(file_entry["path"])
        else:
            node_files.append(file_entry["path"])

    with tempfile.TemporaryDirectory(prefix=f"gqlite_{db_name}_") as staging:
        staging_path = Path(staging)

        with open(staging_path / "spanner_import_config.json", "w") as f:
            json.dump(spanner_config, f, indent=2)

        for csv_name in node_files:
            src = tugraph_dir / csv_name
            dst = staging_path / csv_name
            if src.exists():
                transform_node_csv(src, dst)
            else:
                print(f"    WARNING: {csv_name} not found")

        for csv_name in edge_files:
            src = tugraph_dir / csv_name
            dst = staging_path / csv_name
            if src.exists():
                # Reuse the same header-aware filter as the spanner path so edge
                # CSVs also drop embedding columns if any are declared.
                with open(src, "r", encoding="utf-8", errors="replace") as fin:
                    header = fin.readline().rstrip("\n").rstrip("\r")
                    cols = [c.strip() for c in header.split(",")]
                drop_cols = [c for c in cols if is_dropped_column(c)]
                filter_csv_columns(src, dst, drop_cols)
            else:
                print(f"    WARNING: {csv_name} not found")

        return run_gqlite_import(staging_path, db_name, verbose)


def import_spanner(dataset_dir: Path, db_name: str, verbose: bool) -> bool:
    """Import a Spanner_Instance dataset directly via gqlite.
    Stages the config and CSVs through a temp dir, dropping any columns whose
    names match `is_dropped_column` (e.g. dense embedding vectors that exceed
    the 4KB page-cell limit).
    """
    spanner_dir = find_spanner_dir(dataset_dir)
    if spanner_dir is None:
        return False

    with open(spanner_dir / "spanner_import_config.json") as f:
        config = json.load(f)

    with tempfile.TemporaryDirectory(prefix=f"gqlite_{db_name}_") as staging:
        staging_path = Path(staging)
        new_files = []
        for file_entry in config.get("files", []):
            csv_name = file_entry["path"]
            src_csv = spanner_dir / csv_name
            dst_csv = staging_path / csv_name
            cols = file_entry.get("columns", {})
            drop_cols = [c for c in cols if is_dropped_column(c)]
            if drop_cols and verbose:
                print(f"    dropping columns from {csv_name}: {drop_cols}")
            new_cols = {c: t for c, t in cols.items() if c not in drop_cols}
            new_entry = dict(file_entry)
            new_entry["columns"] = new_cols
            new_files.append(new_entry)
            if src_csv.exists():
                filter_csv_columns(src_csv, dst_csv, drop_cols)
            else:
                print(f"    WARNING: {csv_name} not found")

        with open(staging_path / "spanner_import_config.json", "w") as f:
            json.dump({"files": new_files}, f, indent=2)

        return run_gqlite_import(staging_path, db_name, verbose)


def filter_csv_columns(src: Path, dst: Path, drop_cols: list):
    """Copy a CSV while dropping the listed columns (case-insensitive match on header)."""
    if not drop_cols:
        shutil.copy2(src, dst)
        return
    drop_lower = {c.lower() for c in drop_cols}
    with open(src, "r", encoding="utf-8", errors="replace") as fin:
        header = fin.readline().rstrip("\n").rstrip("\r")
        cols = [c.strip() for c in header.split(",")]
        keep_mask = [c.lower() not in drop_lower for c in cols]
        with open(dst, "w", encoding="utf-8") as fout:
            fout.write(",".join(c for c, keep in zip(cols, keep_mask) if keep))
            fout.write("\n")
            import csv as csv_mod
            reader = csv_mod.reader(fin)
            writer = csv_mod.writer(fout)
            for row in reader:
                writer.writerow([v for v, keep in zip(row, keep_mask) if keep])


def run_gqlite_import(csv_dir: Path, db_name: str, verbose: bool) -> bool:
    """Run gqlite --import-csv to produce examples/<db_name>.gdb."""
    db_path = EXAMPLES_DIR / f"{db_name}.gdb"

    if db_path.exists():
        os.remove(db_path)

    result = subprocess.run(
        [str(GQLITE), str(db_path), "--import-csv", str(csv_dir)],
        capture_output=True,
        text=True,
    )

    if result.returncode != 0:
        print(f"    ERROR running gqlite: {result.stderr}")
        return False

    if verbose:
        for line in result.stderr.strip().split("\n"):
            if line.strip():
                print(f"    {line}")

    return True


def extract_queries(dataset_dir: Path, db_name: str, verbose: bool):
    """Extract initial_gql queries from *_gql.json or *_cypher.json."""
    query_file = find_query_json(dataset_dir)
    if query_file is None:
        if verbose:
            print(f"    (no query file)")
        return

    with open(query_file) as f:
        queries_raw = json.load(f)

    queries = []
    for entry in queries_raw:
        gql = entry.get("initial_gql", "").strip()
        if not gql:
            continue
        queries.append({
            "question": entry.get("initial_question", ""),
            "gql": gql,
            "difficulty": entry.get("difficulty", ""),
        })

    if queries:
        queries_path = EXAMPLES_DIR / f"{db_name}_queries.json"
        with open(queries_path, "w") as f:
            json.dump(queries, f, indent=2)
        if verbose:
            print(f"    {len(queries)} queries -> {queries_path.name}")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def convert_dataset(split: str, name: str, verbose: bool = True) -> bool:
    """Convert a single dataset to .gdb + queries."""
    dataset_dir = BASE_DIR / split / name
    db_name = db_name_from_folder(name)

    if verbose:
        print(f"  [{split}] {name} -> {db_name}.gdb")

    # Try Spanner first (test/, train/, and some dev/ datasets)
    ok = import_spanner(dataset_dir, db_name, verbose)

    # Fall back to TuGraph (most dev/ datasets)
    if not ok:
        ok = import_tugraph(dataset_dir, db_name, verbose)

    if not ok:
        print(f"    SKIP: no importable data found")
        return False

    extract_queries(dataset_dir, db_name, verbose)
    return True


def main():
    if not GQLITE.exists():
        print(f"ERROR: gqlite not found at {GQLITE}")
        print("Run: cd gqlrust && cargo build --release")
        sys.exit(1)

    EXAMPLES_DIR.mkdir(exist_ok=True)

    # Discover all datasets per split
    splits = {}
    for split in ("train", "dev", "test"):
        split_dir = BASE_DIR / split
        if split_dir.exists():
            datasets = sorted([
                d.name for d in split_dir.iterdir()
                if d.is_dir() and not d.name.startswith(".")
            ])
            splits[split] = datasets

    total = sum(len(ds) for ds in splits.values())
    print(f"Converting {total} datasets from train/dev/test to examples/")
    print()

    success = 0
    for split, datasets in splits.items():
        for name in datasets:
            if convert_dataset(split, name):
                success += 1
        print()

    print(f"Done: {success}/{total} datasets converted")


if __name__ == "__main__":
    main()
