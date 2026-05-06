"""Row-content canonicalization + sha256 for the cross-system
row-equivalence oracle. Byte-mirror of `canonicalize_cell` /
`canonicalize_row` / `canonicalize_and_hash` in
`src/bin/ldbc_bench.rs`; both implementations must produce
identical hashes for the same logical row set.

Cell encoding:
    None  → "\\x00"
    bool  → "true" / "false"
    int   → decimal
    float → repr (matches Rust's `{}` for finite f64)
    str   → rstripped (loaders disagree on trailing whitespace; the
            oracle compares semantic equivalence, not formatting)
    other → repr() fallback

Cells joined with "\\x1f", rows with "\\n", sha256 over the bytes
(lowercase hex). LDBC strings don't contain the separator bytes, so
the join is unambiguous.
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
