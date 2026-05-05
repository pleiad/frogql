# auksys/gqlite (gqlite.org) — divergences and integration notes

auksys/gqlite is included as the third external system in the cross-system
bench, after the teacher pointed at [gqlite.org](https://gqlite.org/).
It's distinct from the dead `webbery/gqlite` listed in the original plan
and from our own `gqlite/` — three different systems share the name.

This file documents:
1. The integration choices and divergences from spec-faithful execution
   that affect the bench numbers.
2. **The integration journey itself**, including approaches that didn't
   work and why. This is here on purpose: the teacher's framing applies
   to `auksys/gqlite` as much as to GraphLite — bench-time friction is
   itself a finding worth reporting, and the trail of "what we tried"
   matters as much as the final code.

## What it is

- Rust core, OpenCypher subset, with C / C++ / Python / Ruby / Crystal / Rune bindings.
- Multiple backends: **SQLite** (default), Redb, PostgreSQL.
- Active project; latest changelog entry is v0.9 (2026-04-09) for the
  engine and v1.5 for the Python distribution. Author: Cyrille Berger
  / auKsys org.
- PyPI distribution name: `gqlitedb`. Python module name: `gqlite`.
  (There's a totally unrelated `gqlite` package on PyPI — a GraphQL
  HTTP client — that name-squats the obvious slot. The wrong one
  installs cleanly but exposes no `Connection` class. Smoke check:
  `hasattr(gqlite, "execute_oc_query")`.)

## What's in the comparison

- Same LDBC SF0.1 dataset as every other system.
- Same 15 IC2 substitution-param rows.
- Same cross-system CSV schema, dispatched the same way as graphqlite
  (Python runner, no shell wrapper).
- Native parameter binding via `Connection.execute_oc_query(query, bindings)`
  with `$param` syntax in queries and `{"$param": value}` dicts.

## Documented divergences from spec-faithful execution

### 1. IC2 uses `WHERE c:Comment OR c:Post`, not `(c:Comment|Post)`

auksys/gqlite supports the cleaner `(c:Comment|Post)` Cypher 5+ union-
label syntax in standalone patterns, but in **multi-hop patterns
containing it** the planner errors with `CompileTime: UnknownFunction:
get_source` (gqlitedb 1.5.1). Reproducer: the IC2 query exact shape

```cypher
MATCH (a)-[:knows]-(b)<-[:hasCreator]-(c:Comment|Post)
```

triggers it, while replacing the terminal node with `(c)` and adding
`WHERE c:Comment OR c:Post` works fine. The OR-form is what graphqlite
uses for the same logical query (graphqlite's dialect rejects `:A|B`
entirely), so this matches the apples-to-apples shape across systems.

### 2. No `CREATE INDEX` in the parser

The Cypher parser does not accept any of the standard index-creation
syntaxes:

```cypher
CREATE INDEX FOR (n:Person) ON (n.id)            -- rejected
CREATE INDEX ON :Person(id)                      -- rejected
CREATE INDEX person_id IF NOT EXISTS FOR (n:Person) ON (n.id)  -- rejected
CREATE INDEX FOR (n:Person) ON n.id              -- rejected
```

Each fails with the same `expected node_pattern or edge_pattern` error,
because the parser handles `CREATE` only as the node/edge-creation form.
There is no DDL surface for indexes in v1.5.1.

This is a real divergence from "Cypher" the spec — but it doesn't break
the bench because the bench's load idiom (see below) avoids needing one.

### 3. Properties stored as JSON in a TEXT column → no per-property index

Inspecting the underlying SQLite schema (from a freshly-loaded DB):

```
gqlite_default_nodes:
  id INTEGER PRIMARY KEY      -- internal node rowid
  node_key BLOB               -- 128-bit node key (returned by id(n))
  labels TEXT                 -- '["Person"]'
  properties TEXT             -- '{"id":1,"firstName":"Alice"}'
indexes: only the auto sqlite_autoindex on metadata
```

Property values live JSON-encoded in a single `TEXT` column. Without a
JSON-extract expression index (which Cypher CREATE INDEX would have to
generate), `MATCH (n:Person {id: X})` is a full table scan with JSON
parsing per row. The bench's data-load idiom sidesteps this entirely
by using fresh node-variables in the same statement (see below). At
query time, since IC2 starts with a single `(p:Person {id: $personId})`,
the engine eats one full scan per query — that's reflected in the
latency numbers.

## Setup — what we tried before settling on the canonical idiom

This is documented because the journey took ~2 hours, and the wrong
turns are themselves data points about the system's API ergonomics.

### Attempt 1: per-row UNWIND batches with property-MATCH for edges

```python
conn.execute_oc_query(
    "UNWIND $rows AS r CREATE (:Person {id: r.id, ...})",
    {"$rows": [...]},
)
# ... then for edges:
conn.execute_oc_query(
    "UNWIND $rows AS r "
    "MATCH (a:Person {id: r.s}), (b:Person {id: r.d}) "
    "CREATE (a)-[:knows]->(b)",
    {"$rows": [...]},
)
```

**Outcome:** nodes loaded in ~seconds (288K of them via the UNWIND
CREATE path; that part is fine). **Edges then ground for an hour
without making meaningful progress.** Each MATCH lookup is O(N) due
to the JSON-property storage (above), and the LDBC IC2 hasCreator
phase has 287K edges × 2 lookups each, giving roughly 180 billion
property comparisons.

### Attempt 2: add `CREATE INDEX FOR (n:Person) ON (n.id)` before edges

The textbook fix. **Doesn't work** — see divergence 2. The parser
rejects every index-creation syntax we tried.

### Attempt 3: id_map dance — load nodes, RETURN id(n), use internal IDs

```python
result = conn.execute_oc_query(
    "UNWIND $rows AS r CREATE (p:Person {id: r.id, ...}) RETURN id(p), p.id",
    {"$rows": [...]},
)
# Build Python-side dict: ldbc_id → internal_id (128-bit)
# Then for edges:
conn.execute_oc_query(
    "UNWIND $rows AS r "
    "MATCH (a) WHERE id(a) = r.aid AND id(b) = r.bid "
    "CREATE (a)-[:knows]->(b)",
    {"$rows": [{"aid": ..., "bid": ...}]},
)
```

The same trick the graphqlite Python API does internally (returns
external→rowid maps from its bulk loaders). **Killed before completion**
because we found a better way before the run finished — see Attempt 4.
Whether `MATCH (a) WHERE id(a) = $x` actually hits a fast path in this
engine is unclear; the `node_key` BLOB column has no SQLite index in
the schema we inspected, so it'd likely also be a scan, just without
JSON parsing. Faster than Attempt 1 but probably not by enough to
matter at SF0.1 scale.

### Attempt 4 (the one that works): single big CREATE with shared variables

This is the canonical idiom from auksys's own benchmarks. Their bench
crate (`crates/gqlitedb/benches/common/pokec.rs`) loads the Pokec
social-network dataset by reading the entire file
`pokec_*_import.cypher` into one string and passing it to
`execute_oc_query`. The file is shaped:

```cypher
CREATE
  (user_4826:User {id: 4826, age: 22, ...}),
  (user_3317:User {id: 3317, age: 21, ...}),
  ...
  (user_4826)-[:Friend]->(user_3317),
  (user_3317)-[:Friend]->(user_4502),
  ...
```

**One CREATE statement.** Variables defined when nodes are created
(`user_4826`, etc.) are reused for edges *within the same statement*,
so the engine binds them directly — no MATCH lookup, no property scan.
The pokec_small_import.cypher they ship is 132K lines.

This is what `setup.py` does today. For LDBC SF0.1:
- 1.5K Persons + 151K Comments + 136K Posts = 288K node patterns
- 28K knows × 2 + 287K hasCreator = 315K edge patterns
- ~600K total patterns, ~46 MB query string
- Python build time: ~2 seconds
- gqlite ingest time: TBD on this run, but should match their pokec
  scale expectation since the shape is the same.

### Why this took two hours

Lazy debugging order. We probed the Python API surface, hit the
property-MATCH wall, tried index DDL, designed the id_map workaround,
ran half of it — and only THEN cloned the upstream repo to look at
how *they* benchmark. The upstream answer was sitting in
`crates/gqlitedb/benches/common/pokec.rs` the whole time. It would
have taken five minutes to read first.

The lesson, written here so it sticks: **for any external system
in the cross-system bench, look at how the upstream's own benchmarks
load data before designing your loader.** The integration shape they
ship is almost certainly faster (and certainly more idiomatic) than
whatever you'd reverse-engineer from their public API.

## Why even the canonical idiom doesn't scale to LDBC SF0.1

After Attempt 4 ran for ~10 minutes with the DB growing at ~440 KB/sec
and no end in sight, we did a deep source-and-issues review of the
upstream. Findings (with file:line citations, all paths relative to
`auksys/gqlite` repo at the dev/1 branch):

1. **All "alternative" code paths converge at `execute_oc_query`.**
   - The CLI's `.read FILE` (`crates/gqlitecli/src/main.rs`) reads
     lines and calls `execute_oc_query`. No fast-import path.
   - `gqlitebrowser` (web UI), `gqb` (query builder), `gqls` (ORM)
     all wrap `execute_oc_query`.
   - All five language bindings (Python, Ruby, Crystal, C++, Rune)
     expose only `new` / `execute_oc_query` / `close`.

2. **`Connection::builder().set_option(...)` accepts a fixed key set:**
   `path`, `backend`, `url`, `host`, `user`, `password`. **No PRAGMA,
   `journal_mode`, `synchronous`, cache-size, batch-mode, or
   durability knobs.** (`crates/gqlitedb/src/connection.rs:225-310`)

3. **SQLite is opened with library defaults** —
   `crates/gqlitedb/src/store/sqlite.rs:265` calls
   `rusqlite::Connection::open(&path)`. That gives you rollback
   journal, `synchronous=FULL`, 2 MB cache, no mmap. **Per-row fsync
   per commit.**

4. **The schema has no index on the node key column.** The CREATE
   TABLE template (`crates/gqlitedb/templates/sql/sqlite/graph_create.sql`)
   has only `id INTEGER PRIMARY KEY` and a `node_key` BLOB. **No index
   on `node_key`, none on `properties`, none on `labels`.** Every
   `MATCH (n {id:X})` is a full-table scan with `json_extract` per
   row.

5. **Redb has the same shape.** `crates/gqlitedb/src/store/redb.rs:540-555`:
   when matching by a property (`{id: X}`), it does
   `nodes_table.range::<PersistentKey>(..)` — a full scan, then a
   linear filter. Switching backend doesn't help.

6. **The interpreter doesn't batch even when the storage trait would
   allow it.** `crates/gqlitedb/src/interpreter/evaluators.rs:1275,1286`:
   each CREATE pattern calls `store.create_nodes(&mut tx, &name, vec![&n])`
   — a Vec of ONE. The Store trait accepts iterators but the
   interpreter never gathers them.

7. **The missing pieces are on the published roadmap, not shipped.**
   Open GitLab issues at gitlab.com/auksys/GQLite (none merged):
   - #169 — custom indexes
   - #196 — streaming / pipeline execution
   - #198 — refactor interpreter into a stream pipeline
   - #200 — introduce logical planner
   - #202 — deterministic planner
   The README itself states: *"Development effort has now slowed down."*

## Designed-for scale, by the project's own benchmarks

The Pokec social-network benchmark in `crates/gqlitedb/benches/`:

| File | Nodes | Friend edges | Bytes | Patterns |
|---|---|---|---|---|
| pokec_micro (smallest) | 138 | 138 | 17 KB | ~280 |
| **pokec_tiny (largest in bench enum)** | **4,538** | **12,681** | **870 KB** | **~17K** |
| pokec_small (file exists, not in `PokecSize`) | 10,000 | 121,716 | 5.6 MB | ~132K |
| **LDBC SF0.1 (us)** | **~289K** | **~315K** | **46 MB** | **~604K** |

Their `PokecSize` enum is `{Micro, Tiny}` — `Small` exists as a file
but **isn't even part of their benchmark suite**. Their largest *run*
is 17K patterns. We're throwing **35× their benched scale at the
system, and 5× their largest data file**. There's no realistic
expectation this would work; it's past the cliff.

## Verdict for the bench writeup

`auksys/gqlite` v1.5.1 cannot load LDBC SF0.1 in reasonable time.
This isn't a configuration miss or a missing idiom — the deep review
confirmed via source citations that the architecture lacks property
indexes, lacks batched store calls, lacks SQLite tuning, and the
upstream acknowledges these as roadmap items. LDBC SF0.1 is the
**smallest** scale factor LDBC publishes; this system can't ingest
it. That's the finding.

## Project signals

- Active project. Last engine release v0.9 (2026-04-09); changelog
  entries are recent and meaningful (parser rewrite, postgres
  backend, schema generation). Sub-1.0, but moving.
- README: *"still in its early stage"* and *"Development effort has
  now slowed down."*
- Multiple language bindings, multiple backends, gqlitebrowser web UI.
  Breadth over depth — they shipped lots of surface area before
  shipping the load/index machinery underneath.

## What would change this story

1. **Property indexes (issue #169)** — would let MATCH-by-property
   scale. Today only the shared-variable single-CREATE trick avoids
   needing one, and that trick doesn't scale past their tested
   pokec_tiny.
2. **Batched interpreter (issue #198)** — would let the engine
   amortize parse/plan/execute over many patterns instead of paying
   per-row.
3. **`CREATE INDEX` in the parser** — fixes divergence 2.
4. **Fix the `(c:A|B)` planner bug in multi-hop patterns** — fixes
   divergence 1.

Until those land — none earlier than v0.10 per the roadmap —
auksys/gqlite is not a candidate for LDBC-scale benchmarks.
