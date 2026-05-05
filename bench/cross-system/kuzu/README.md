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
# One-time: load LDBC SF0.1 IC2 subset into Kuzu's native format.
# Outputs to bench/data/cross-system/kuzu/ic2.db/ (Kuzu writes a
# multi-file directory). Idempotent; --force to rebuild.
python bench/cross-system/kuzu/setup.py --ic 2

# Bench. Writes per-iter CSV in cross-system schema to <out_csv>.
python bench/cross-system/kuzu/run.py /tmp/kuzu.csv \
    --ic 2 --iters 10 --warmup 2
```

`run.py` will auto-invoke `setup.py` if the DB doesn't exist.

## Files

| File | Purpose |
|---|---|
| `requirements.txt` | Single line: `kuzu==0.11.3` |
| `setup.py` | LDBC CSV → Kuzu native DB. Schema-first (CREATE NODE/REL TABLE), then COPY FROM with FROM/TO hints for the multi-typed `hasCreator` REL TABLE. Materializes `knows` in both directions. |
| `run.py` | Per-iter CSV emitter. Reads `ic<n>.toml` for query metadata + params; reads `ic<n>.cypher` for the translation; iterates the result set via `QueryResult.has_next()` / `.get_next()`. |
| `ic2.cypher` | openCypher translation of `bench/ldbc-queries/ic2.toml`. Uses `UNION ALL` of two MATCH clauses (one for `:Comment`, one for `:Post`) because Kuzu doesn't accept `(c:Comment\|Post)` or `WHERE c:Comment OR c:Post`. See `DIVERGENCES.md`. |
| `DIVERGENCES.md` | Documented divergences from the spec / the other systems' translations. The UNION ALL form is the major one. |

## Adding new ICs

When `bench/ldbc-queries/ic<n>.toml` flips to `status = "implemented"`,
add `bench/cross-system/kuzu/ic<n>.cypher` here. `setup.py`'s
`SUPPORTED_ICS` may need updating if the new IC requires LDBC nodes/
edges we don't currently load (Forum, Tag, Place, etc.). `run.py`
needs no changes — it derives all paths from `--ic <n>`.
