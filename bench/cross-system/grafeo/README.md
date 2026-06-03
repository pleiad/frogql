# Grafeo — cross-system bench adapter

[Grafeo](https://grafeo.dev/) (GrafeoDB/grafeo on PyPI) is an embeddable,
GQL-native Rust graph database. This adapter plugs it into the
cross-system bench the same way `kuzu/` and `graphqlite/` do.

- **Version:** `grafeo==0.5.34` (pinned in `requirements.txt`).
- **Backend label:** `grafeo-gql` (the CSV `backend` column).
- **Interface measured:** the Python wheel — `db.execute(query, params)`,
  the documented user-facing API. Grafeo keeps an internal plan cache,
  so repeated calls with the same query string don't re-parse.

## Prereqs

```bash
pip install -r bench/cross-system/grafeo/requirements.txt
```

`import_df` (the bulk loader) needs pandas; `requirements.txt` pulls it in.

## Files

| File | Role |
|---|---|
| `setup.py` | Loads the full LDBC SF0.1 into `bench/data/cross-system/grafeo/ldbc-sf01.grafeo` via `import_df`. IC-agnostic. |
| `run.py` | Runs one IC for every param row; emits the per-iter CSV + the row-content hash oracle. Mirrors `graphqlite/run.py`. |
| `ic<n>.gql` | Per-IC GQL translation of `bench/ldbc-queries/ic<n>.toml`. |
| `DIVERGENCES.md` | Dialect/loader divergences from the canonical toml. |

## How the loader works

Grafeo's bulk path is `import_df(df, mode='nodes'|'edges')`. Node imports
assign sequential internal ids in DataFrame row order, continuing the
global counter, so `setup.py` records `{ldbc_id: grafeo_id}` per label as
`dict(zip(df.id, range(base, base+n)))` (`base = db.node_count` before the
import). Edge CSVs are typed `(srcLabel.id, dstLabel.id)`, so each edge
file maps its two id columns through the right per-label maps, then
`import_df(mode='edges')`.

Loaded entity set mirrors `kuzu/setup.py`. The loader reproduces gqlite's
node/edge totals exactly: **327 588 nodes, 1 477 965 edges**.

## Status

All six currently-implemented ICs are translated and **row-equivalence
verified** against gqlite (every param row byte-identical):

| IC | Translation | Row-equivalence vs gqlite |
|---|---|---|
| IC2 | `ic2.gql` | ✅ 15/15 param rows byte-identical |
| IC5 | `ic5.gql` | ✅ 15/15 |
| IC6 | `ic6.gql` | ✅ 15/15 |
| IC8 | `ic8.gql` | ✅ 15/15 |
| IC9 | `ic9.gql` | ✅ 15/15 |
| IC11 | `ic11.gql` | ✅ 15/15 |

Dialect map (probed against 0.5.34, full detail in `DIVERGENCES.md`):

- Variable-length `~[:knows]~{1,2}` **is supported** (also `*1..2`).
- `OPTIONAL MATCH` is supported; a `WHERE` between MATCH and OPTIONAL
  MATCH is not (move it after the OPTIONAL MATCH).
- `GROUP BY` **is supported** — group by the property expression
  (`GROUP BY forum.id`), not the RETURN alias.
- A second top-level `MATCH` is rejected → fold into one comma-joined
  MATCH (IC5, IC6).
- Pattern label alternation `(:Comment|Post)` is **not** supported →
  bind the node and use `WHERE x:Comment OR x:Post` (IC2, IC8, IC9).
- The loader does not synthesize `:Company` / `:Country` sub-labels;
  filter on `o.type = 'company'` / `pl.type = 'country'` (IC11).

## Run

```bash
# via the orchestrator (sets up once, then benches)
bench/cross-system/run_all.sh --only gqlite,grafeo --ics 2

# or directly
python bench/cross-system/grafeo/setup.py --force
python bench/cross-system/grafeo/run.py /tmp/grafeo.ic2.csv --ic 2 --iters 10 --warmup 2
```
