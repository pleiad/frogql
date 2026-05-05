# GraphLite-AI/GraphLite — divergences from spec-faithful execution

GraphLite is included as the third system in the cross-system bench,
but we had to make several concessions to get LDBC SF0.1 data through
its loader and IC2 through its query engine. This file documents each
concession plain — readers should know what's in the comparison
numbers and what isn't.

The teacher's read on this is right: bench-time bugginess of an external
system is itself a finding worth reporting. We run the queries that
succeed, and `compare_results.py` surfaces a per-system error count
alongside the latency table so failures are visible, not hidden.

## What's in the comparison

- Same LDBC SF0.1 dataset (`bench/data/ldbc-sf0.1/...`) as the other
  systems consume.
- Same 15 IC2 substitution-param rows, same `personId|maxDate` columns,
  same per-row `RETURN`.
- Latency measured with the same cross-system CSV schema
  (`query;backend;params;row;iter;result_count;elapsed_ns`), so the
  comparison code treats all three systems identically.

## What's divergent

### 1. Non-ASCII string content is dropped at load time

GraphLite 0.0.1's lexer (`graphlite-0.0.1/src/ast/lexer.rs:488`) slices
into a UTF-8 string by byte offset (`&input[..N]`) during keyword
detection without checking `is_char_boundary`. Any non-ASCII byte
that lands inside a multi-byte char triggers a panic before the parser
or planner even see the input.

LDBC SF0.1 includes names like "Amenábar" and content with accented
characters; without intervention the loader panics on the first such
row (~10% of `Person` and a similar fraction of `Comment` / `Post`).

**What we do:** `setup.rs` calls `ascii_only(&str)` on every string
property before formatting the INSERT. Non-ASCII chars are dropped:
"Amenábar" → "Amenbar". The escape function (`quote`) sees ASCII-only
input and the lexer is happy.

**Why this is acceptable for IC2:** IC2 doesn't `WHERE`-filter on
string content — it only RETURNs `friend.firstName`, `friend.lastName`,
`c.content`. Row counts, column types, and join structure all stay
intact. The only thing affected is the *contents* of returned name /
content fields, which our `compare_results.py` doesn't compare on
(it checks counts and column-type signatures, not row values).

**Why this is NOT acceptable in general:** for any IC that filters
on a string property (e.g. `WHERE friend.firstName = 'María'`), the
ASCII-folded data would silently miss matches. We'd need to either fix
the underlying lexer bug or ASCII-fold the query parameters too. IC2
is the only IC currently `status = "implemented"` in our toml, and it's
not a string-filter query, so the issue doesn't bite.

### 1b. String literals: backslash-escape only

GraphLite's lexer (`graphlite-0.0.1/src/ast/lexer.rs::escaped_string_content`)
recognises `\'` as the only way to embed a single quote in a `'...'`-quoted
string. SQL-style `''` doubling is NOT recognised — the lexer terminates
the string at the first inner `'` and the rest of the statement becomes
garbage tokens. Symptom on bulk-insert: every row whose content contains
an apostrophe (e.g. LDBC's `BBC's` or names like `O'Brien`) fails the
NEXT statement's parse with `Parse error: UnexpectedToken(Insert)`,
because the lexer's earlier confusion bleeds into the next read.

**What we do:** `setup.rs::quote()` writes `\'` (and escapes any literal
backslashes as `\\`) for every embedded apostrophe in property values.
Keeps load throughput intact — only ~5 rows in the SF0.1 comments file
were affected, but in a larger dataset (SF1+) we'd lose thousands.

This is documented for completeness; it doesn't compromise bench
fidelity since query content goes through the same engine that decoded
it on insert.

### 2. `USE SCHEMA` / `USE GRAPH` are rejected by the parser

Documented quick-start usage:
```rust
session.execute("USE SCHEMA myschema")?;
session.execute("USE GRAPH social")?;
```

Parser rejects `USE`:
```
Parse error: UnexpectedToken(Identifier("USE"))
```

The runtime error message recommends `SESSION SET SCHEMA <name>` /
`SESSION SET GRAPH <name>`, which the parser does accept. We use the
working form. Surface area: just `setup.rs` and `run.rs` — both have a
comment noting the doc divergence.

### 2c. Why the load hung — root cause from the source (post-mortem)

Initial diagnosis was "Sled WAL contention." Wrong. After a deep
review of the upstream source, the actual reason is at the executor
level, not the storage layer:

- `graphlite/src/exec/write_engine/operations/match_insert.rs:506` —
  `MATCH (a {id:X}) ... INSERT (a)-[:rel]->(b)` resolves variable `a`
  by calling `graph.get_all_nodes()` and `.filter()` in Rust. **A
  full O(N) linear filter over the in-memory node `HashMap`, not a
  hash lookup.** Per matched variable. Per edge insert.
- `graphlite/src/storage/graph_cache.rs:21` — `nodes: HashMap<String, Node>`
  is keyed by GraphLite-internal id, not by user `id` property. There
  is no secondary property index ever, despite what `Architecture.md`
  claims. The "Property Index" string in the docs is aspirational.
- `graphlite/src/storage/persistent/sled.rs:97` — `batch_insert` is
  declared but the body is `for (k,v) in entries { self.insert(k,v)?; }`.
  **No `sled::Batch`**. Pretending to batch.
- `graphlite/src/storage/data_adapter.rs:415-418` — every commit
  flushes all four trees (`nodes_tree`, `edges_tree`, `metadata_tree`,
  `catalog_tree`). Per-row commits = four fsyncs per row.

So with persons (1.5K) we got ~30s — fine, the in-memory HashMap is
small, the per-edge filter is cheap. With **comments + their edges**
the in-memory node HashMap grows past 150K and **every edge insert
runs a linear filter over that growing set**. That's quadratic in
total entities.

The "RSS dropped to 700kB, no disk writes for 90 min" detail we saw
matches this exactly: the executor is stuck in a tight CPU filter-loop
over the in-memory node set, Sled is idle (nothing to write yet
because nothing committed), Windows pages out the cold parts of the
process.

### 2d. Bulk load is on the upstream roadmap, not shipped

Source comment in `graphlite/src/storage/indexes/traits.rs:61` literally
reads: *"ROADMAP v0.4.0 - Batch index operations for bulk data loading"*
and `schema/validator.rs:216` mentions index schema validation as a
future item. The maintainers know the system can't bulk-load at scale
in v0.0.1. The `block_cache_size: 64MB` field on the storage trait
(`storage/persistent/traits.rs:174`) is silently ignored — `sled.rs:117`
calls `sled::open(path)` flat with library defaults, no `sled::Config`,
no `Mode::HighThroughput`, no `flush_every_ms`. The knob exists; it's
not wired up.

### 2e. Designed-for scale, by their own tests/examples/benches

We checked the entire repository for any bulk-load idiom or richer SDK
method. Findings:

- CLI (`gql-cli/src/cli/commands.rs`) has 4 commands: `Version`,
  `Query`, `Gql` (REPL), `Install`. **No `import`, no `\copy`, no CSV
  loader, no `-f script.gql`.**
- C-FFI (`graphlite-ffi/src/lib.rs`) has 7 functions, the same surface
  as the SDK: `open / create_session / query / close_session /
  free_string / close / version`. No batch entrypoint.
- All bindings (Python, Java, Rust SDKs and their bindings/) expose
  only `execute / query / transaction`. No bulk methods. We checked.
- Largest example anywhere in `examples/` is `drug_discovery` with
  ~25 INSERT patterns. No LDBC, no bulk, no large-data example.
- Their own benches (`benches/session_throughput.rs` and
  `benches/catalog_cache_throughput.rs`) exercise 1000 sessions and
  5 schemas / 15 graphs respectively. Both run on near-empty graphs.

**Project's own performance-testing scale: ~10³ entities.** LDBC SF0.1
(~604K node+edge entities) is **two-to-three orders of magnitude past
what GraphLite v0.0.1 was designed and tested for.** Document it as
out-of-scale and move on.

### 2b. Setup hangs partway through bulk load (observed empirically)

In the run we recorded for the bench, `graphlite-setup` (Rust loader,
auto-commit per row, batch=40 nodes / batch=15 edges to stay under the
1000-iteration lexer cap, after the apostrophe-escape fix) completed
the persons phase (1528 nodes in ~30s) but then ran for 90+ minutes
on the comments phase without emitting a single batch-success or
skip line and without growing the on-disk DB beyond ~37 MB. The
process became effectively idle — RSS dropped to ~700 kB and the
last write to `ic2.db/db` was minutes after persons completed. We
killed it and proceeded with a partial-load DB rather than wait
indefinitely.

The auksys/gqlite loader (different system, also Rust-core-based,
also auto-commit) processed all 287K nodes via UNWIND in a few
seconds on the same machine and dataset, so this isn't a
system-load issue — it's specific to GraphLite's storage stack
locking up under sustained per-statement INSERTs against a Sled
backend.

For the bench, this means we either:
- Run with the partial DB (persons-only) — IC2 returns 0 rows
  for every param row because the join requires Comments / Posts
  / edges, none of which are loaded.
- Run with the empty `expected_shape = "empty"` and the runner's
  sentinel-row mechanism flagging every iter as errored.

Either way the GraphLite column in the comparison table is going
to be mostly empty / sentinels. That's the honest finding.

### 3. Per-row INSERT pipeline (no bulk-insert API)

The published SDK has no batch / parameterised / bulk-insert path —
only `tx.execute(&str)` and `session.query(&str)`. So the loader
formats and executes individual GQL statements through the full
lex→parse→plan→execute pipeline (1.5K Persons + 151K Comments + 136K
Posts + 28K knows-edges-both-directions + 287K hasCreator-edges).
This makes one-time setup considerably slower than gqlite's CSV
importer (which does it in a single linear pass over the files).

We do batch multiple graph patterns into one INSERT statement
(`INSERT (:L {...}), (:L {...}), ...`) where the parser allows it,
but **the lexer hard-caps tokenization at 1000 iterations**
(`graphlite-0.0.1/src/ast/lexer.rs:326`, with the comment "Infinite
loop protection") so node batches above ~50 patterns and edge batches
above ~25 pairs hit `Parse error: LexerError("Infinite loop detected
in lexer")` even on perfectly valid syntax. We use NODE_BATCH=40 and
EDGE_BATCH=15 to stay safely under the cap.

This is reported as setup-time cost; query-time cost (what the latency
table actually measures) is unaffected.

### 4. Numbers are `f64` everywhere

`graphlite_sdk::Value::Number(f64)` is the only numeric variant — no
separate Int / Float discrimination. LDBC IDs are conceptually i64 but
this engine transports them as f64. Our shape-of-value mapping in
`run.rs` calls a `Number(n)` "i" when `n.fract() == 0.0` so the column
type signature still matches `expected_shape = "i,s,s,i,n/s,i"`. For
SF0.1 the ID range fits within f64 mantissa (53-bit) so precision
holds; at SF1+ scale this would start dropping bits.

### 5. No parameter-binding API

`session.query` only takes a `&str`. The runner does string
substitution of `{{personId}}` / `{{maxDate}}` / `{{messageLabel}}`
into the template per param row before calling `query`. Substitution
values are LDBC-supplied integers, so injection-shaped concerns
don't apply, but it's a syntax-fragile pattern.

### 6. No `:A | :B` union-label syntax

IC2's `(c:Comment | Post)` doesn't compile in this dialect. The
runner runs the template once per label (`Comment`, then `Post`) and
concatenates the rows before passing to the shape/count logic. Cost:
two queries per param row instead of one. Reported latency is
wall-clock for both queries summed.

### 7. Errors are caught, not fatal

Per-query failures (panics or `Err` returns from the SDK) are caught
in `run.rs` via `catch_unwind`. The runner emits a sentinel CSV row
with `result_count = -1` and continues. `compare_results.py` reads
sentinel rows as "errored" and reports a per-system error tally
alongside the latency table.

This is necessary because GraphLite 0.0.1 is buggy enough that any
single query is a non-trivial chance of bringing the whole runner
down (lexer panics on input we didn't ASCII-fold, SDK Result errors
on parser surface mismatches, etc.). Aborting the bench on first
error would lose all the other rows' measurements.

## Project activity signals (still relevant)

- `graphlite-rust-sdk` v0.0.1 published 2025-11-23 — only version.
- `graphlite` core crate v0.0.1 published 2025-11-21 — only version.
- Upstream repo's most recent commit at the time of bench integration
  is ~4 months old: `fix(GQL): ORDER BY clause not sorting results
  correctly` — followed by no further activity.

So we're benching against an early, slow-moving project, and reporting
both its latency (where it can answer) and its error rate (where it
can't). Both numbers are findings.

## What would change this story

If upstream:
1. Fixes the UTF-8 lexer panic — we drop the ASCII-fold concession.
2. Reconciles `USE` keyword surface — minor cleanup, no behavior change.
3. Ships a bulk-insert API — setup time drops to comparable scale.
4. Stops `Value::Number(f64)` collapsing Int and Float — we get a real
   `i` vs `f` shape signature without our heuristic.
5. Adds `:A | :B` union-label syntax — we drop the runner-side
   per-label loop.

…then the bench numbers become more directly comparable. Until then
the divergences above are the contract: read the table knowing what's
been compromised, and the error count tells you how often "compromised"
becomes "couldn't answer."
