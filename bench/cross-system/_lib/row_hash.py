"""Row-content canonicalization + sha256 for the cross-system
row-equivalence oracle.

Mirrors `canonicalize_cell` / `canonicalize_row` / `canonicalize_and_hash`
in `src/bin/ldbc_bench.rs` exactly. With ORDER BY in every IC's toml
the three runners' iter-0 results are deterministic, so byte-equal
blobs across systems → identical sha256 hashes → row-content
equivalence. compare_results.py reads HASH lines from each runner's
stderr and flags any (IC, params_row) where systems disagree.

Cell encoding:
    None         → "\\x00"
    True         → "true"
    False        → "false"
    int          → decimal repr
    float        → repr (matches Rust's default Display for finite f64)
    str          → trailing whitespace trimmed (rstrip), no other escaping
    other        → repr() fallback

Cells joined with "\\x1f" (US, unit separator). Rows joined with "\\n".
sha256 of the resulting bytes, lowercase hex.

Why rstrip on strings: a formatting-level difference between the
loaders — gqlite strips trailing whitespace from CSV string columns,
Kuzu preserves it. Neither is "wrong"; both are reasonable choices.
The row-content oracle wants to compare semantic equivalence, so we
normalize away the formatting drift at canonicalization time without
touching the data each engine actually loaded. Mirrored in
`canonicalize_cell` in `src/bin/ldbc_bench.rs`.

LDBC SF0.1 doesn't ship strings containing the separator bytes, so the
join-without-escape is unambiguous.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path


def canonicalize_cell(v) -> str:
    if v is None:
        return "\x00"
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return repr(v)
    if isinstance(v, str):
        # Trailing-whitespace rstrip — see module docstring.
        return v.rstrip()
    return repr(v)


def canonicalize_row(row) -> str:
    """`row` may be a list (Kuzu) or a dict keyed by alias (graphqlite).
    For dicts we iterate the column ORDER provided by the caller — the
    canonical form is positional.
    """
    if isinstance(row, dict):
        # Caller is responsible for passing the column ORDER via an
        # ordered iterable; if a plain dict slips through, fall back
        # to insertion order (Python 3.7+ guarantees this).
        cells = list(row.values())
    else:
        cells = list(row)
    return "\x1f".join(canonicalize_cell(v) for v in cells)


def canonicalize_and_hash(rows, columns: list[str] | None = None) -> tuple[str, str]:
    """Returns `(canonical_blob, sha256_hex)`. `columns` is used for
    dict-shaped rows (graphqlite returns one); for list-shaped rows
    (Kuzu's `result.get_next()`) it's ignored.
    """
    canon_rows = []
    for row in rows:
        if isinstance(row, dict) and columns is not None:
            cells = [row.get(c) for c in columns]
            canon_rows.append("\x1f".join(canonicalize_cell(v) for v in cells))
        else:
            canon_rows.append(canonicalize_row(row))
    blob = "\n".join(canon_rows)
    h = hashlib.sha256(blob.encode("utf-8")).hexdigest()
    return blob, h


def append_rows_jsonl(
    path: Path,
    ic: int,
    params: str,
    row_idx: int,
    count: int,
    rows_blob: str,
    row_hash: str,
) -> None:
    """Append one envelope to `path` (created if absent). Mirrors
    `append_rows_jsonl` in `src/bin/ldbc_bench.rs` byte-for-byte —
    same field names, same ordering — so compare_results.py reads
    one format regardless of producer.
    """
    envelope = {
        "ic": ic,
        "params": params,
        "row": row_idx,
        "count": count,
        "hash": row_hash,
        "rows": rows_blob,
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as f:
        f.write(json.dumps(envelope, ensure_ascii=False) + "\n")
