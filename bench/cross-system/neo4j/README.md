# Neo4j (community, via Docker) — cross-system bench runner

Neo4j 5 community is the production-system reference point in the
cross-system bench ("the cost of portability"): a mature client-server
graph database with a daemon, page cache, and a network protocol,
benched on the same LDBC SNB SF0.1 ICs as the embedded systems.

## Components

- `docker.sh` — container lifecycle helper
- `setup.py` — loads the FULL LDBC SF0.1 over bolt (IC-agnostic)
- `run.py` — runs one IC against the loaded server (cross-system CSV +
  ROW hash contract)
- `ic<N>.cypher` — per-IC Cypher translations of `bench/ldbc-queries/ic<N>.toml`
- `DIVERGENCES.md` — every semantic carve-out vs the canonical GQL toml

## Quick start

```bash
# 1. Start the server (pulls neo4j:5 on first use, waits for bolt)
bash bench/cross-system/neo4j/docker.sh up

# 2. Load LDBC SF0.1 (~30 s; pass --force to wipe + reload)
bench/cross-system/.venv/bin/python bench/cross-system/neo4j/setup.py

# 3. Run an IC
bench/cross-system/.venv/bin/python bench/cross-system/neo4j/run.py \
    /tmp/neo_ic2.csv --ic 2 --iters 10 --warmup 2

# Status / teardown
bash bench/cross-system/neo4j/docker.sh status
bash bench/cross-system/neo4j/docker.sh down
```

## Container

`docker.sh up` runs a container named **`frogql-bench-neo4j`** from
the **`neo4j:5`** (community) image:

- bolt on `localhost:7687`, HTTP browser on `localhost:7474`
- auth `neo4j` / `benchbench` (override at query time with
  `NEO4J_URI` / `NEO4J_USER` / `NEO4J_PASSWORD` env vars, honored by
  both `setup.py` and `run.py`)
- heap 4G (initial == max), page cache 2G

No volume is mounted: data lives inside the container, and `setup.py`
reloads from the LDBC CSVs in well under a minute, so
`docker.sh down && docker.sh up` is the cheapest full wipe.
`setup.py --force` is the in-place alternative (batched
`CALL { ... } IN TRANSACTIONS` DETACH DELETE, then reload).

`docker.sh up` waits until `cypher-shell` inside the container answers
`RETURN 1` — the TCP port opens well before the database accepts
queries, so port-polling is not a sufficient readiness probe.

## Loader (setup.py)

Same logical data model as `kuzu/setup.py` — see `DIVERGENCES.md` §1
for the full mapping. Highlights:

- node labels Person / Comment / Post / Forum / Organisation / Place /
  Tag / TagClass; LDBC sub-types stay flat as the `type` property
- relationship types lowercase (`knows`, `hasCreator`, ...), including
  the multi-source/target rels (`hasCreator` from Comment AND Post,
  `likes` to Comment AND Post, `replyOf` to Comment AND Post,
  `isLocatedIn` from four labels, `hasTag` from three)
- Person `email` / `language` MVAs pre-aggregated into list properties
- `knows` loaded single-direction; queries match it undirected
- dates kept as epoch-millis ints (no Neo4j temporal types)
- empty CSV fields are NOT stored (absent == null), so
  `coalesce(content, imageFile)` agrees with gqlite's loader
- uniqueness constraints (`id` per label) created BEFORE loading: the
  edge-phase `MATCH ... {id}` joins and the ICs' start-node lookups
  are index-backed, the analog of Kuzu's `PRIMARY KEY(id)`
- transport: batched `UNWIND $rows` (5000 rows/tx) over bolt

The final line prints total wall time and node/edge counts; the counts
must equal gqlite's import (327 588 nodes / 1 477 965 edges).
Reference load time on an M-series laptop: **~30 s**.

## Runner (run.py)

Same CLI/output contract as `kuzu/run.py`:

```
python run.py <out_csv> --ic N --iters N --warmup N
```

- output CSV schema `query;backend;params;row;iter;result_count;elapsed_ns`
  with backend **`neo4j-cypher`**
- one `ROW row=... count=... shape=... hash=...` stderr line per param
  row + a `<out>.rows.jsonl` envelope, via `_lib/row_hash.py` — the
  cross-system row-equivalence oracle
- latency is wall time around `session.run` + draining the cursor: the
  complete per-query round-trip including bolt serialization. One
  driver + session is reused across all params/iters (mirrors how the
  embedded runners reuse their connection). This measures Neo4j via
  its primary user-facing interface; the bolt/Docker hop is part of
  what a Neo4j user pays per query and is NOT subtracted — quote the
  standard measurement caveat from `bench/cross-system/README.md`.
- structured result cells (lists/maps) are re-encoded into froGQL's
  `Value` Debug format before hashing so the oracle compares logical
  values, not driver reprs — see `DIVERGENCES.md` §3.

## Verification status (SF0.1, 15 param rows per IC, vs gqlite)

All 12 implemented ICs row-hash-verified against gqlite: IC1, IC2,
IC3, IC4, IC5, IC6, IC7, IC8, IC9, IC11, IC12, IC13 — 15/15 rows each
(see DIVERGENCES.md §3 for how list/record columns are compared).
