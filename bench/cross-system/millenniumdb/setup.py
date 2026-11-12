#!/usr/bin/env python3
"""Load the FULL LDBC SNB SF0.1 dataset into a MillenniumDB database.

The cross-system bench's "set up once per system" loader for MillenniumDB
(github.com/MillenniumDB/MillenniumDB, GQL model). Two steps:

  1. Convert the LDBC CsvBasic files into MillenniumDB's GQL import format
     (`bench/data/cross-system/millenniumdb/ldbc-sf01.gql`).
  2. `mdb import ldbc-sf01.gql ldbc-sf01.mdb` builds the on-disk database.

Step 1 mirrors froGQL's own LDBC loader (`load_from_ldbc_csv_dir` in
`src/model/csv_loader.rs`) rule for rule, so both engines see the same
graph and the row-hash oracle compares like with like:

  - Every `*.csv` under `static/` and `dynamic/`, in sorted path order.
  - Node files: label = capitalised file stem (`tagclass` -> `TagClass`).
    A `type` column (Place, Organisation) becomes a second label
    (`:Place :City`, `:Organisation :Company`) and is NOT kept as a
    property, exactly as froGQL does.
  - Every non-empty cell is a property, typed by the same
    int -> float -> bool -> string cascade (`parse_ldbc_value`). Empty
    cells are absent, so `COALESCE` / `IS NULL` behave the same.
  - `id` is kept as a property too.
  - Multi-valued attribute files (`person_email_emailaddress`,
    `person_speaks_language`) become list properties `email` / `speaks`
    on the Person, in file order.
  - Edge label = the middle file stem (`hasCreator`). `knows` is stored as
    ONE undirected edge per CSV row (`a~b`), which is how froGQL stores it
    too; queries use `~[:knows]~`.
  - CSV splitting mirrors froGQL's `parse_csv_line` (`"` toggles a quoted
    run, `""` inside it is a literal quote, each field is trimmed).

Expected totals (same as froGQL on SF0.1): 327 588 nodes, 1 477 965 edges.

Usage:
  python setup.py [--force] [--csv-dir <path>] [--db <path>]
  MDB_BIN=/path/to/mdb python setup.py        # if mdb is not on PATH
"""

from __future__ import annotations

import argparse
import math
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from mdb_common import DB_DIR, DB_NAME, DEFAULT_CSV_DIR, GQL_DUMP_NAME, find_mdb  # noqa: E402

EXPECTED_NODES = 327_588
EXPECTED_EDGES = 1_477_965

_INT_RE = re.compile(r"^[+-]?[0-9]+$")
_FLOAT_RE = re.compile(r"^[+-]?([0-9]+\.?[0-9]*|\.[0-9]+)([eE][+-]?[0-9]+)?$")
_I64_MIN, _I64_MAX = -(2**63), 2**63 - 1


# ---- byte-for-byte mirrors of src/model/csv_loader.rs -----------------


def parse_csv_line(line: str, delim: str = "|") -> list[str]:
    """Mirror of `parse_csv_line`: `"` toggles a quoted run, `""` inside
    one is a literal quote, every field is trimmed."""
    out: list[str] = []
    cur: list[str] = []
    in_quotes = False
    i, n = 0, len(line)
    while i < n:
        c = line[i]
        if in_quotes:
            if c == '"':
                if i + 1 < n and line[i + 1] == '"':
                    cur.append('"')
                    i += 1
                else:
                    in_quotes = False
            else:
                cur.append(c)
        elif c == '"':
            in_quotes = True
        elif c == delim:
            out.append("".join(cur).strip())
            cur = []
        else:
            cur.append(c)
        i += 1
    out.append("".join(cur).strip())
    return out


def read_lines(path: Path) -> list[str]:
    """Rust `BufRead::lines`: split on `\\n` only, drop one trailing `\\r`.
    (`str.splitlines` would also split on U+2028 and friends.)"""
    text = path.read_text(encoding="utf-8")
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return [ln[:-1] if ln.endswith("\r") else ln for ln in lines]


def read_csv(path: Path) -> tuple[list[str], list[list[str]]]:
    lines = read_lines(path)
    if not lines:
        raise RuntimeError(f"empty CSV: {path}")
    header = parse_csv_line(lines[0])
    rows = [parse_csv_line(ln) for ln in lines[1:] if ln.strip()]
    return header, rows


def parse_ldbc_value(s: str):
    """Mirror of `parse_ldbc_value`: i64, then finite f64, then bool, else str."""
    if _INT_RE.match(s):
        v = int(s)
        if _I64_MIN <= v <= _I64_MAX:
            return v
    if _FLOAT_RE.match(s):
        f = float(s)
        if math.isfinite(f):
            return f
    if s in ("true", "True", "TRUE"):
        return True
    if s in ("false", "False", "FALSE"):
        return False
    return s


def capitalize_first(s: str) -> str:
    return s[:1].upper() + s[1:] if s else s


def ldbc_node_label(stem: str) -> str:
    return "TagClass" if stem.lower() == "tagclass" else capitalize_first(stem)


def filename_stems(path: Path) -> list[str]:
    parts = path.stem.split("_")
    if len(parts) < 3 or not (parts[-2].isdigit() and parts[-1].isdigit()):
        raise RuntimeError(f"unexpected LDBC filename: {path.name}")
    return parts[:-2]


def is_dotted_id(col: str) -> bool:
    dot = col.find(".")
    return dot > 0 and col[dot:].lower() == ".id"


def classify(path: Path, header: list[str]):
    stems = filename_stems(path)
    plain = sum(1 for c in header if c.lower() == "id")
    dotted = sum(1 for c in header if is_dotted_id(c))
    if plain == 1 and len(stems) == 1:
        return ("node", ldbc_node_label(stems[0]))
    if dotted >= 2 and len(stems) == 3:
        return ("edge", stems[1])
    if dotted == 1 and len(stems) == 3:
        return ("mva", stems[1])
    raise RuntimeError(f"cannot classify {path} (header {header})")


# ---- MillenniumDB GQL import format ------------------------------------


def node_ident(label: str, ldbc_id: str) -> str:
    # Import identifiers are [a-zA-Z][a-zA-Z0-9_]*. LDBC ids repeat across
    # entity types (Place 0, Organisation 0), so the label is part of it.
    return f"{label}_{ldbc_id}"


def fmt_value(v) -> str:
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return repr(v)
    if isinstance(v, list):
        return "[" + ", ".join(fmt_value(x) for x in v) + "]"
    s = str(v).replace("\\", "\\\\").replace('"', '\\"')
    return f'"{s}"'


def fmt_props(props: dict) -> str:
    return "".join(f" {k}:{fmt_value(v)}" for k, v in props.items())


def write_dump(csv_dir: Path, out_path: Path) -> tuple[int, int]:
    files = []
    for sub in ("static", "dynamic"):
        d = csv_dir / sub
        if d.is_dir():
            files.extend(p for p in d.iterdir() if p.suffix == ".csv")
    files.sort()
    if not files:
        raise RuntimeError(f"no .csv under {csv_dir}/static or /dynamic")

    classified = []
    for p in files:
        header, rows = read_csv(p)
        classified.append((p, classify(p, header), header, rows))

    # Pass 1: nodes, keyed by (label, id) like froGQL's node_name_to_id.
    nodes: dict[tuple[str, str], tuple[list[str], dict]] = {}
    for p, (kind, label), header, rows in classified:
        if kind != "node":
            continue
        id_idx = next(i for i, c in enumerate(header) if c.lower() == "id")
        type_idx = next((i for i, c in enumerate(header) if c.lower() == "type"), None)
        prop_cols = [(i, c) for i, c in enumerate(header) if i != id_idx and i != type_idx]
        for row in rows:
            id_str = row[id_idx] if id_idx < len(row) else ""
            if not id_str or (label, id_str) in nodes:
                continue
            labels = [label]
            if type_idx is not None:
                raw = row[type_idx] if type_idx < len(row) else ""
                if raw:
                    labels.append(capitalize_first(raw))
            props: dict = {}
            for i, name in prop_cols:
                if i < len(row) and row[i]:
                    props[name] = parse_ldbc_value(row[i])
            props["id"] = parse_ldbc_value(id_str)
            nodes[(label, id_str)] = (labels, props)

    # Pass 2: multi-valued attributes -> list property on the owner.
    for p, (kind, attr), header, rows in classified:
        if kind != "mva":
            continue
        id_idx = next(i for i, c in enumerate(header) if is_dotted_id(c))
        val_idx = next(i for i, _ in enumerate(header) if i != id_idx)
        entity = header[id_idx].split(".")[0]
        for row in rows:
            id_str = row[id_idx] if id_idx < len(row) else ""
            val = row[val_idx] if val_idx < len(row) else ""
            if not id_str or not val:
                continue
            owner = nodes.get((entity, id_str))
            if owner is None:
                continue
            owner[1].setdefault(attr, []).append(parse_ldbc_value(val))

    n_edges = 0
    with out_path.open("w", encoding="utf-8") as out:
        for (label, id_str), (labels, props) in nodes.items():
            lbls = "".join(f" :{lb}" for lb in labels)
            out.write(f"{node_ident(label, id_str)}{lbls}{fmt_props(props)}\n")

        # Pass 3: edges.
        for p, (kind, label), header, rows in classified:
            if kind != "edge":
                continue
            ids = [i for i, c in enumerate(header) if is_dotted_id(c)]
            src_idx, tgt_idx = ids[0], ids[1]
            src_ent = header[src_idx].split(".")[0]
            tgt_ent = header[tgt_idx].split(".")[0]
            prop_cols = [(i, c) for i, c in enumerate(header) if i not in (src_idx, tgt_idx)]
            arrow = "~" if label.lower() == "knows" else "->"
            for row in rows:
                s = row[src_idx] if src_idx < len(row) else ""
                t = row[tgt_idx] if tgt_idx < len(row) else ""
                if (src_ent, s) not in nodes or (tgt_ent, t) not in nodes:
                    continue
                props = {}
                for i, name in prop_cols:
                    if i < len(row) and row[i]:
                        props[name] = parse_ldbc_value(row[i])
                out.write(
                    f"{node_ident(src_ent, s)}{arrow}{node_ident(tgt_ent, t)}"
                    f" :{label}{fmt_props(props)}\n"
                )
                n_edges += 1
    return len(nodes), n_edges


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--force", action="store_true", help="rebuild even if the db exists")
    ap.add_argument("--csv-dir", type=Path, default=DEFAULT_CSV_DIR)
    ap.add_argument("--db", type=Path, default=None)
    args = ap.parse_args()

    csv_dir: Path = args.csv_dir
    db_path: Path = args.db or (DB_DIR / DB_NAME)
    dump_path = db_path.parent / GQL_DUMP_NAME

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
    if db_path.exists():
        shutil.rmtree(db_path)
    db_path.parent.mkdir(parents=True, exist_ok=True)

    mdb = find_mdb()
    t0 = time.perf_counter()
    print(f"  writing {dump_path} from {csv_dir}", file=sys.stderr)
    n_nodes, n_edges = write_dump(csv_dir, dump_path)
    t_dump = time.perf_counter() - t0
    print(
        f"    {n_nodes} nodes, {n_edges} edges in {t_dump:.1f}s",
        file=sys.stderr,
    )
    if (n_nodes, n_edges) != (EXPECTED_NODES, EXPECTED_EDGES):
        print(
            f"  WARN: froGQL loads {EXPECTED_NODES} nodes / {EXPECTED_EDGES} edges "
            f"from SF0.1; this dump differs (another scale factor?)",
            file=sys.stderr,
        )

    print(f"  {mdb} import {dump_path.name} {db_path.name}", file=sys.stderr)
    t1 = time.perf_counter()
    proc = subprocess.run(
        [str(mdb), "import", str(dump_path), str(db_path)],
        capture_output=True, text=True,
    )
    sys.stderr.write(proc.stdout[-3000:])
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr[-3000:])
        print(f"  mdb import failed (exit {proc.returncode})", file=sys.stderr)
        if db_path.exists():
            shutil.rmtree(db_path)
        return 1
    t_import = time.perf_counter() - t1
    print(
        f"  done in {time.perf_counter() - t0:.1f}s "
        f"(dump {t_dump:.1f}s + mdb import {t_import:.1f}s). db at {db_path}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
