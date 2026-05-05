# Kuzu — cross-system bench integration

[Kuzu](https://kuzudb.com) is an embedded graph database with a
vectorized columnar engine and an openCypher subset. Originated as a
research project at the University of Waterloo (CIDR 2023 paper);
the company sponsoring the project archived the GitHub repo on
2025-10-10. The PyPI wheel still installs and the engine still works;
we pin to **kuzu 0.11.3** (the final release) so the bench numbers
are reproducible. See `DIVERGENCES.md` for the full story.

## Prerequisites

```bash
# From this directory, or anywhere with the repo's Python on PATH:
pip install -r requirements.txt
```

That installs `kuzu==0.11.3` from PyPI. No other system dependencies
(no Docker, no JVM, no Redis). Wheel is available for Python 3.8+ on
Linux, macOS, and Windows.

You also need the LDBC SF0.1 dataset materialized (`bench_setup` from
the repo root); see `bench/cross-system/README.md` for that step —
it's shared across all per-system loaders.

## Running just this system

The cross-system orchestrator drives this subdir as part of
`bench/cross-system/run_all.sh`. To iterate on the Kuzu integration
in isolation:

```bash
# One-time: load FULL LDBC SF0.1 (every entity type, MVAs included)
# into Kuzu's native format. Outputs to
# bench/data/cross-system/kuzu/ldbc-sf01.db/ (Kuzu writes a multi-
# file directory). Idempotent; --force to rebuild.
python bench/cross-system/kuzu/setup.py

# Bench an IC. Writes per-iter CSV to <out_csv>.
python bench/cross-system/kuzu/run.py /tmp/kuzu.csv \
    --ic 2 --iters 10 --warmup 2
```

The runner does NOT auto-invoke setup if the DB is missing — the
cross-system orchestrator (`run_all.sh`) is responsible for
ordering setup-then-run per system. If you bypass run_all.sh,
remember to run setup.py first.

Setup is **IC-agnostic**: one `ldbc-sf01.db` covers any IC we
implement. Adding a new IC translation (`ic<n>.cypher`) doesn't
require any setup.py changes.

## Files

| File | Purpose |
|---|---|
| `requirements.txt` | Single line: `kuzu==0.11.3` |
| `setup.py` | LDBC CSV → Kuzu native DB. Loads the FULL LDBC SF0.1 dataset (every entity, every edge, MVAs as `STRING[]` on Person). Schema-first (CREATE NODE/REL TABLE), then COPY FROM with FROM/TO hints for the multi-typed REL TABLEs (`hasCreator`, `replyOf`, `isLocatedIn`, `hasTag`, `likes`). Materializes `knows` in both directions. Pre-aggregates Person MVAs (email, language) into a generated CSV before COPY. |
| `run.py` | Per-iter CSV emitter. Reads `ic<n>.toml` for query metadata + params; reads `ic<n>.cypher` for the translation; iterates the result set via `QueryResult.has_next()` / `.get_next()`. Uses the documented `Connection.execute(query_str, params)` API (Kuzu's internal plan cache handles repeated queries; the `PreparedStatement` API is officially deprecated). |
| `ic2.cypher` | openCypher translation of `bench/ldbc-queries/ic2.toml`. Uses unlabeled `(c)` because Kuzu's multi-typed `hasCreator` REL TABLE constrains it to Comment-or-Post automatically — no UNION ALL or label predicate needed. See `DIVERGENCES.md`. |
| `DIVERGENCES.md` | Documented divergences from the spec / the other systems' translations, plus the archival-status framing. |

## Adding new ICs

When `bench/ldbc-queries/ic<n>.toml` flips to `status = "implemented"`,
add `bench/cross-system/kuzu/ic<n>.cypher` here. **No setup.py
changes needed** — the DB is loaded once with the full LDBC dataset
and shared across all ICs. `run.py` derives all paths from `--ic <n>`.
