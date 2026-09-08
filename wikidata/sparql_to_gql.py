#!/usr/bin/env python3
"""Translate an IMGpedia basic graph pattern into a froGQL query.

The input dialect is the compact triple form the MillenniumDB similarity
benchmark is written in: whitespace-separated `subject predicate object`
triples joined by ` . `, where

  ?vNN            a variable
  NNN             a constant image id
  NN or PNN       an edge label; the bare number is shorthand for `PNN`
  kN              the similarity predicate: the object is among the N
                  nearest images to the subject

so

  ?v10 69 ?v00 . ?v10 6 100980834 . ?v01 926 ?v21 . ?v01 69 ?v11 . ?v00 k50 ?v11

reads as "images v11, reachable through P69 from something with a P926
edge, that are among the 50 nearest to v00, where v00 is reached through
P69 from something whose P6 points at image 100980834".

The `kN` triple is what makes this a similarity *join* rather than a
similarity *search*: the query vector is v00's, and v00 is bound by the
pattern, so there is one ranking per v00 rather than one for the query.
froGQL spells that with a pattern variable inside the query-vector
expression:

  NEAREST 50 v11.hog TO VECTOR(v00, 'hog') AS dist

Usage
-----
  sparql_to_gql.py '?v10 69 ?v00 . ?v00 k50 ?v11'
  echo '...' | sparql_to_gql.py
  sparql_to_gql.py --file q.txt --attr hog --limit 100
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass

VAR = re.compile(r"^\?([A-Za-z_][A-Za-z0-9_]*)$")
CONST = re.compile(r"^\d+$")
PRED = re.compile(r"^P?(\d+)$")
KNN = re.compile(r"^k(\d+)$", re.IGNORECASE)


class TranslationError(Exception):
    pass


@dataclass(frozen=True)
class Edge:
    """One ordinary triple: `src -[:label]-> tgt`.

    `label` is `None` when the source wrote a **variable** in predicate
    position (`?v20 ?v10 142434602`). SPARQL binds that variable to the
    property IRI; GQL has no label variable, so it renders as an
    unlabelled edge — "some edge, any label". That is exact as long as
    the variable is not used anywhere else, which `parse` enforces: a
    predicate variable that is also joined on would need label equality,
    and dropping it would silently widen the query.
    """

    src: str | None  # variable name, or None when the subject is a constant
    src_const: int | None
    label: str | None
    tgt: str | None
    tgt_const: int | None


@dataclass
class Knn:
    """One `kN` triple: `target` is among the `k` nearest to `anchor`."""

    anchor: str
    k: int
    target: str


def split_triples(text: str) -> list[list[str]]:
    """Split on ` . ` and newlines, tolerating a trailing dot.

    A dot is only a separator when it stands alone as a token, so a
    decimal inside a constant is never mistaken for one.
    """
    tokens = text.replace("\n", " ").split()
    triples: list[list[str]] = []
    current: list[str] = []
    for tok in tokens:
        if tok == ".":
            if current:
                triples.append(current)
                current = []
            continue
        current.append(tok)
    if current:
        triples.append(current)
    for t in triples:
        if len(t) != 3:
            raise TranslationError(f"expected three terms per triple, got {' '.join(t)!r}")
    return triples


def term(tok: str) -> tuple[str | None, int | None]:
    """Classify a subject/object term as (variable, constant)."""
    m = VAR.match(tok)
    if m:
        return m.group(1), None
    if CONST.match(tok):
        return None, int(tok)
    raise TranslationError(
        f"term {tok!r} is neither a ?variable nor a numeric image id"
    )


def parse(text: str) -> tuple[list[Edge], list[Knn]]:
    edges: list[Edge] = []
    knns: list[Knn] = []
    # A SPARQL basic graph pattern is a *set* of triple patterns, so a
    # repeated triple contributes nothing. A comma-join is not idempotent
    # under ISO bag semantics — repeating an operand multiplies the rows
    # once parallel edges exist — so the duplicate has to go here rather
    # than be left for RETURN DISTINCT to hide.
    seen_triples: set[tuple[str, str, str]] = set()
    # Where each predicate variable was used, so reuse can be caught.
    pred_vars: dict[str, int] = {}
    other_vars: set[str] = set()

    for s_tok, p_tok, o_tok in split_triples(text):
        if (s_tok, p_tok, o_tok) in seen_triples:
            continue
        seen_triples.add((s_tok, p_tok, o_tok))
        s_var, s_const = term(s_tok)
        o_var, o_const = term(o_tok)
        for v in (s_var, o_var):
            if v is not None:
                other_vars.add(v)

        m = KNN.match(p_tok)
        if m:
            if s_var is None or o_var is None:
                raise TranslationError(
                    "the similarity predicate needs variables on both sides, "
                    f"got {s_tok} {p_tok} {o_tok}"
                )
            knns.append(Knn(anchor=s_var, k=int(m.group(1)), target=o_var))
            continue

        m = VAR.match(p_tok)
        if m:
            # A variable property: "some edge, whichever". Recorded so the
            # reuse check below can run once every triple is seen.
            pred_vars[m.group(1)] = pred_vars.get(m.group(1), 0) + 1
            edges.append(
                Edge(
                    src=s_var,
                    src_const=s_const,
                    label=None,
                    tgt=o_var,
                    tgt_const=o_const,
                )
            )
            continue

        m = PRED.match(p_tok)
        if not m:
            raise TranslationError(
                f"predicate {p_tok!r} is neither a property number, a "
                f"?variable, nor a kN"
            )
        edges.append(
            Edge(
                src=s_var,
                src_const=s_const,
                label=f"P{m.group(1)}",
                tgt=o_var,
                tgt_const=o_const,
            )
        )

    for name, uses in pred_vars.items():
        if uses > 1 or name in other_vars:
            raise TranslationError(
                f"?{name} is used in predicate position and joined on "
                "elsewhere; GQL has no label variable, so this pattern "
                "cannot be translated without changing what it asks"
            )
    if len(knns) > 1:
        raise TranslationError(
            "froGQL allows one NEAREST clause per query; this pattern has "
            f"{len(knns)}"
        )
    return edges, knns


def node(var: str | None, const: int | None, label: str, id_prop: str) -> str:
    """Render one endpoint.

    A constant becomes an anonymous node carrying a value filter, which
    froGQL's elaboration lowers to a WHERE on the same element — writing
    the WHERE by hand would say the same thing less legibly.
    """
    if var is not None:
        return f"({var}:{label})"
    return f"(:{label} {{{id_prop}: {const}}})"


def declared_once(edges: list[Edge], label: str, id_prop: str) -> dict[str, str]:
    """Give each variable its label on first mention only.

    Repeating `(v10:img)` in every operand is legal but noisy, and it
    makes a reader check that the labels agree. Later mentions render as
    a bare `(v10)`.
    """
    seen: set[str] = set()
    rendering: dict[str, str] = {}
    for e in edges:
        for v in (e.src, e.tgt):
            if v is None or v in seen:
                continue
            seen.add(v)
            rendering[v] = f"({v}:{label})"
    return rendering


def translate(
    text: str,
    *,
    attr: str,
    label: str,
    id_prop: str,
    dist: str,
    limit: int | None,
    returns: str | None,
    distinct: bool,
) -> str:
    edges, knns = parse(text)
    if not edges:
        raise TranslationError("the pattern has no ordinary triples to match")

    first = declared_once(edges, label, id_prop)
    printed: set[str] = set()

    def endpoint(var: str | None, const: int | None) -> str:
        if var is None:
            return node(None, const, label, id_prop)
        if var in printed:
            return f"({var})"
        printed.add(var)
        return first[var]

    operands = []
    for e in edges:
        src = endpoint(e.src, e.src_const)
        tgt = endpoint(e.tgt, e.tgt_const)
        arrow = "-[]->" if e.label is None else f"-[:{e.label}]->"
        operands.append(f"{src}{arrow}{tgt}")

    lines = ["MATCH " + ",\n      ".join(operands)]

    if knns:
        knn = knns[0]
        for who, role in ((knn.anchor, "anchor"), (knn.target, "target")):
            if who not in printed:
                raise TranslationError(
                    f"the similarity {role} ?{who} is not bound by any triple"
                )
        lines.append(
            f"NEAREST {knn.k} {knn.target}.{attr} "
            f"TO VECTOR({knn.anchor}, '{attr}') AS {dist}"
        )

    if returns is not None:
        projection = returns
    else:
        cols = [f"{v}.{id_prop}" for v in sorted(printed)]
        if knns:
            cols.append(dist)
        projection = ", ".join(cols)
    lines.append(("RETURN DISTINCT " if distinct else "RETURN ") + projection)

    if knns:
        lines.append(f"ORDER BY {knns[0].anchor}.{id_prop}, {dist}")
    if limit is not None:
        lines.append(f"LIMIT {limit}")

    return "\n".join(lines) + ";"


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Translate an IMGpedia basic graph pattern into froGQL."
    )
    ap.add_argument("pattern", nargs="?", help="the pattern; omit to read stdin")
    ap.add_argument("--file", help="read the pattern from this file instead")
    ap.add_argument(
        "--batch",
        action="store_true",
        help="treat every non-empty line of the input as its own query "
        "(what a .tsv of benchmark patterns is). Without it the whole "
        "input is one pattern, and a file of 100 patterns would merge "
        "into one with 100 similarity predicates.",
    )
    ap.add_argument(
        "--one-line",
        action="store_true",
        help="emit each query on a single line — the froGQL REPL reads a "
        "statement per line, so a pasted multi-line query is parsed as "
        "several broken ones",
    )
    ap.add_argument("--attr", default="hog", help="vector attribute name (default: hog)")
    ap.add_argument("--label", default="img", help="node label (default: img)")
    ap.add_argument(
        "--id-prop", default="id", help="property holding the image id (default: id)"
    )
    ap.add_argument(
        "--dist", default="dist", help="name for the distance binding (default: dist)"
    )
    ap.add_argument("--limit", type=int, help="append a LIMIT")
    ap.add_argument("--return", dest="returns", help="override the projection")
    ap.add_argument(
        "--no-distinct",
        action="store_true",
        help="keep ISO bag multiplicity instead of RETURN DISTINCT",
    )
    args = ap.parse_args()

    if args.file:
        text = open(args.file).read()
    elif args.pattern:
        text = args.pattern
    else:
        text = sys.stdin.read()
    if not text.strip():
        ap.error("no pattern given")

    patterns = (
        [ln for ln in text.splitlines() if ln.strip()] if args.batch else [text]
    )

    failed = 0
    for i, pattern in enumerate(patterns, 1):
        try:
            gql = translate(
                pattern,
                attr=args.attr,
                label=args.label,
                id_prop=args.id_prop,
                dist=args.dist,
                limit=args.limit,
                returns=args.returns,
                distinct=not args.no_distinct,
            )
        except TranslationError as e:
            # Keep going: one unsupported pattern in a benchmark file
            # should not cost the other ninety-nine.
            where = f"line {i}: " if args.batch else ""
            print(f"error: {where}{e}", file=sys.stderr)
            failed += 1
            continue
        print(" ".join(gql.split()) if args.one_line else gql)

    return 1 if failed else 0


if __name__ == "__main__":
    # Die quietly when the reader goes away, the way a well-behaved
    # filter does: `sparql_to_gql.py --batch | head` closes the pipe
    # mid-write, and the default Python handler turns that into a
    # BrokenPipeError traceback that looks like the translation failed.
    try:
        import signal

        signal.signal(signal.SIGPIPE, signal.SIG_DFL)
    except (ImportError, AttributeError, ValueError):
        # No SIGPIPE on Windows; there the write simply raises and the
        # except below catches it.
        pass
    try:
        raise SystemExit(main())
    except BrokenPipeError:
        raise SystemExit(0) from None
