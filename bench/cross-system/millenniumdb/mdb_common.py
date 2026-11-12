"""Shared paths and binary lookup for the MillenniumDB runner + loader."""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent.parent

DEFAULT_CSV_DIR = (
    REPO_ROOT
    / "bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter"
)
DB_DIR = REPO_ROOT / "bench/data/cross-system/millenniumdb"
GQL_DUMP_NAME = "ldbc-sf01.gql"
DB_NAME = "ldbc-sf01.mdb"


def find_mdb() -> Path:
    """Locate the `mdb` binary.

    Order: `$MDB_BIN`, then `mdb` on PATH, then a sibling checkout built in
    place (`<interpreter>/MillenniumDB/build/bin/mdb`, the layout of the
    research repo this bench lives in). MillenniumDB has no wheel; the
    server is a native binary built from source.
    """
    env = os.environ.get("MDB_BIN")
    if env:
        p = Path(env).expanduser()
        if p.is_file():
            return p
        sys.stderr.write(f"  MDB_BIN={env} is not a file\n")
        sys.exit(1)
    on_path = shutil.which("mdb")
    if on_path:
        return Path(on_path)
    # setup_millenniumdb.sh builds into build/<type>/; a bare `cmake -B build`
    # builds into build/.
    for rel in ("build/Release/bin/mdb", "build/bin/mdb"):
        sibling = REPO_ROOT.parent / "MillenniumDB" / rel
        if sibling.is_file():
            return sibling
    sys.stderr.write(
        "  mdb binary not found. Build MillenniumDB and point MDB_BIN at it:\n"
        "    git clone https://github.com/MillenniumDB/MillenniumDB\n"
        "    cd MillenniumDB && ./scripts/setup_millenniumdb.sh\n"
        "    export MDB_BIN=$PWD/build/Release/bin/mdb\n"
    )
    sys.exit(1)
