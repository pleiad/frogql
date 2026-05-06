# Cross-system bench: systems considered

Single-page survey of every external graph database we evaluated as
a candidate for the cross-system LDBC SNB IC2 bench, working or
rejected. Use this as the "Systems considered but rejected"
subsection for any writeup; the per-system `DIVERGENCES.md` and
`SKIPPED.md` files are the deep-dive references.

## Outcome at a glance

### Headline numbers (canonical — cite these)

LDBC SNB SF0.1, 15 IC2 param rows, 5 measured iters + 2 warmup,
`--rebuild-setup --ablate`, run captured at
`bench/cross-system/results/20260505_162656/`. All 15 param rows
pass count AND shape verification across every mode (`OK row N:
count=20` × 15 plus per-system 15/15 shape passes in
`comparison.txt`).

Five axes: setup time, on-disk DB size, IC2 latency, peak RSS, and
the gqlite ablation slowdowns. All captured in the same run.

| System / mode | Setup (s) | DB size | IC2 median (ms) | Peak RSS over baseline | × gqlite-baseline |
|---|---|---|---|---|---|
| **gqlite** `lazy-baseline` (all opts on) | **~29†** | 1.41 GiB | **0.23** | 432 MiB | **1×** |
| gqlite `lazy-no-auto-indexes` | (same .gdb) | 1.41 GiB | 182.69 | 315 MiB | 794× slower |
| gqlite `lazy-no-fold` | (same .gdb) | 1.41 GiB | 186.15 | 431 MiB | 810× slower |
| gqlite `disk-baseline` | (same .gdb) | 1.41 GiB | 189.94 | 293 MiB | 826× slower |
| **Kuzu** v0.11.3 | **2.48** | 91 MiB | 6.52 | 108 MiB | 28× slower |
| **graphqlite** v0.4.4 | 28.76 | 333 MiB | 11.18 | **8 MiB** | 49× slower |

† gqlite setup measured separately via the `gqlite` CLI's
`--import-ldbc-csv` mode (which doesn't hit the `bench_setup.exe`
UAC issue described below). Breakdown of the ~29 s: 2.96 s CSV
parse + ~26 s writing the 1.41 GiB `.gdb` to disk. The `.gdb` is
about 4× larger than graphqlite's SQLite (333 MiB) and 16× larger
than Kuzu's `.db` (91 MiB), which explains most of the disk-write
time. Open + TripleIndex warm at *query* time add ~2.2 s; that's
part of the per-bench cost, not setup.

**FFI per-call floor** (measured from a minimal cached-plan query —
the cost of the Python ↔ C/C++ round-trip with essentially no
engine work): graphqlite 0.13 ms, Kuzu 0.59 ms, gqlite 0 ms (Rust
direct, no FFI in the bench path). FFI is **1.2 % of graphqlite's
IC2 latency**, **9.0 % of Kuzu's**, and 0 % of gqlite's — too small
to alter the ranking. A hypothetical gqlite-py with ~0.5 ms FFI
would still be ~13× faster than Kuzu and ~22× faster than
graphqlite. Documented in `README.md` "Measurement basis".

`run_all.sh` itself reports "user-managed" for gqlite because its
own CSV→`.gdb` runner is `bench_setup.exe`, which Windows treats as
an installer (binary name contains "setup") and demands UAC
elevation regardless of shell. The clean fix is upstream — rename
the binary or add a Windows manifest declaring
`requestedExecutionLevel asInvoker` — and is out of scope for this
PR. Linux/macOS hosts don't hit this. The number we report above
(~29 s) is from the unaffected `gqlite --import-ldbc-csv` path.

### Reading the table

- **Setup is the once-per-DB cost** (load LDBC SF0.1 CSVs into the
  system's native format). Kuzu's `COPY ... FROM 'csv'` is ~12×
  faster than graphqlite's per-row insert. Both are dwarfed by query
  latency cumulatively over a benchmark run.
- **gqlite's headline 0.23 ms IC2 median** is bought entirely by
  three architectural choices working together (auto-built `(label,
  prop)` indexes, LTJ index-fold pass, TripleIndex cache). Disable
  any of the first two and gqlite collapses to ~185 ms — *17-30×
  slower than Kuzu and graphqlite.* Full ablation breakdown below.
- **gqlite trades RAM for speed.** 432 MiB peak RSS at lazy-baseline
  vs graphqlite's 8 MiB and Kuzu's 108 MiB — graphqlite leans hard on
  SQLite's mmap (data pages live in the OS page cache, not the
  process), while gqlite holds topology + adjacency CSR + auto-
  indexes + TripleIndex + LRU-cached label/prop pages all resident.
  This is an architectural choice, not a free lunch.
- **disk-baseline** is *not* a "disk I/O is slow" measurement; it's a
  "the secondary-index machinery isn't wired into `DiskGraphStore`"
  measurement. RAM cost drops to 293 MiB (-139 MiB vs lazy-baseline
  — that delta is the LRU page cache lazy keeps resident), but
  latency collapses because the indexed start-node lookup falls
  through to a position scan. See "Ablation results" below.

### One-line per system

| System | Engine type | Outcome | Why |
|---|---|---|---|
| **gqlite** (us) | Rust + custom `.gdb`, ISO GQL | ✅ working | LTJ + auto-indexes + TripleIndex cache → 0.23 ms IC2 median |
| **graphqlite** (colliery-io) | Python wrapper, SQLite extension, Cypher | ✅ working | SQLite rowid as internal node id; `insert_*_bulk` API skips MATCH lookups → 11.2 ms IC2 median |
| **Kuzu** (kuzudb) | Vectorized columnar, openCypher | ✅ working (pinned) | CIDR 2023 paper, real engineering; archived Oct 2025; pinned to v0.11.3 → 6.5 ms IC2 median |
| **GraphLite-AI/GraphLite** | Rust + Sled, ISO GQL | ❌ rejected | Per-edge linear scan over in-memory HashMap; bulk-load on roadmap, unbuilt; load hung after persons phase |
| **auksys/gqlite** (gqlite.org) | Rust core, openCypher subset, SQLite/Redb | ❌ rejected | Properties stored as JSON in `TEXT`; no `CREATE INDEX`; no batched store calls; 5× past their largest tested data file |
| **webbery/gqlite** | C++ + libmdbx, custom JSON-shaped DSL | ❌ rejected | DSL grammar lacks `LIMIT` and label-disjunction; build fails 4 ways on Windows MSVC 2022; last commit 2023-04-08 |

See "[Measurement basis](README.md#measurement-basis-read-this-before-quoting-numbers)"
in the top-level README before quoting these — particularly the FFI
overhead disclosure (gqlite is benched via Rust CLI, the others via
Python wheels) and the ORDER-BY divergence from spec IC2.

---

## Working systems

### gqlite (this repository)

Rust graph database implementing ISO GQL path pattern matching with
custom single-file storage (`.gdb`). Bench uses the `lazy` backend
(`LazyGraphStore` — topology in RAM, labels/props paged from disk
via LRU cache).

**Why it's fast on IC2 specifically** — three architectural choices
working together:

1. **Auto-built secondary indexes**: `LazyGraphStore::build_auto_indexes_bulk()`
   runs at open and creates hash + btree indexes on every
   `(label, prop)` whose values are unique within the label. For
   IC2's start-node lookup `MATCH (p:Person {id: $personId})` this
   is a true hash lookup, O(1).
2. **Leapfrog Triejoin (LTJ)** with **TripleIndex cache**: edges are
   modelled RDF-style as `(src, label, tgt)` triples; the
   TripleIndex maintains six sorted orderings (SPO, SOP, POS, PSO,
   OSP, OPS), warmed at open and reused across queries. LTJ binds
   variables one at a time by intersecting candidate lists with no
   intermediate materialisation, with early termination on `LIMIT`.
3. **Index-driven constant folding**: when a `MATCH (n {id: X})`
   predicate hits a known secondary index, the variable is folded
   to a single NodeId pre-LTJ; the variable is dropped from the VEO
   and pre-bound in the result tuple.

See [CLAUDE.md "Optimizer" / "Secondary indexes" / "Join strategy"](../../CLAUDE.md)
for the full architecture.

### graphqlite (colliery-io)

Python wrapper around a SQLite extension that implements a Cypher-like
query language. Stores graph data in regular SQLite tables; uses
SQLite's btree-on-rowid as the index by construction.

**Why it works at LDBC scale** — graphqlite's `Graph.insert_nodes_bulk(nodes)`
returns an `external_id → SQLite_rowid` dict. The companion
`insert_edges_bulk(edges, id_map)` then takes that dict so per-edge
inserts skip the MATCH lookup entirely (they go straight to the
mapped rowid). This is what every other system needs an index for;
graphqlite gets it via API design.

We pre-aggregate Person MVAs (`email`, `language`) before the
`insert_nodes_bulk` call so they ride along as list-valued properties
on Person — graphqlite stores props as JSON internally so list values
round-trip transparently.

External IDs are prefixed by label (`Person:933`, `Place:0`, etc.)
because LDBC IDs aren't globally unique across labels — without
prefixing, smaller-id node types collide and the id_map silently
corrupts. (Bug we hit and fixed during integration; see
[`graphqlite/setup.py`](graphqlite/setup.py) module docstring.)

Setup time at SF0.1: 30.07 s for full load including MVAs.
IC2 latency: 11.18 ms cross-row median (canonical run).
Peak RSS over baseline: ~8 MiB. SQLite's mmap pushes the data
pages into the OS page cache rather than the process — RSS doesn't
account for the working set, only graphqlite's bookkeeping. This is
why graphqlite is the most memory-frugal of the three working systems.

### Kuzu (kuzudb)

C++ embedded graph database with vectorized columnar execution.
Originated as a research project at the Waterloo DSG (CIDR 2023
paper); commercialized by Kùzu Inc., **archived 2025-10-10** with
v0.11.3 as the final release.

**Why it's still in the bench despite being archived**: it's a
peer-reviewed engineered system with a citable provenance. The
PyPI wheel `kuzu==0.11.3` is frozen — pinning gives the bench
numbers a more stable reference than a moving target would. For a
research-paper-tier comparison, "actively maintained" is not a
strict requirement; "engineered, working, reproducible" is, and
Kuzu satisfies all three. Full archival framing in
[`kuzu/DIVERGENCES.md`](kuzu/DIVERGENCES.md).

**Integration shape**:
- Schema-first DDL with `PRIMARY KEY(id)` on each NODE TABLE → PK
  index by construction → no separate `CREATE INDEX` needed.
- `COPY <Table> FROM 'csv' (DELIM='|', HEADER=true)` is the bulk
  loader; multi-typed REL TABLEs (e.g. `hasCreator(FROM Comment TO
  Person, FROM Post TO Person)`) need `(FROM='X', TO='Y')` hints in
  COPY.
- IC2's label-disjunction is expressed via the `label()` builtin:
  `WHERE label(message) IN ["Comment", "Post"]`. Kuzu's openCypher
  rejects pattern disjunction (`(c:Comment|Post)`) and the
  `node:Label` predicate, so this is the spec-faithful query-level
  form; details and the rejected alternatives (`UNION ALL` LIMIT
  trap, the schema-constrained form) are in
  [`kuzu/DIVERGENCES.md`](kuzu/DIVERGENCES.md).
- Use the documented `Connection.execute(query_str, params)` API,
  not `PreparedStatement` (officially deprecated in 0.11.x).

Setup time at SF0.1: ~2.7 s for full load + MVAs.
IC2 latency: ~520 ms cross-row median on the spec-faithful
`label()` form (Kuzu 0.11.3's optimizer doesn't push `label()`
through joins). IC8 ~14 s/iter for the same reason — an honest
finding, not a number to mask.
Peak RSS over baseline: ~108 MiB.

---

## Rejected systems

### GraphLite-AI/GraphLite

Rust embedded graph database, claims ISO GQL surface, Sled-backed
storage. v0.0.1 on crates.io (`graphlite-rust-sdk`), published
2025-11. Source at github.com/GraphLite-AI/GraphLite.

**Outcome**: setup loaded persons (1.5K rows in ~30 s) cleanly,
then hung indefinitely on the comments phase. After 90+ minutes
RSS dropped to ~700 kB and the on-disk DB hadn't grown — the
process was stuck in a tight CPU filter loop, not blocked on I/O.

**Root cause** (verified by reading their source):

- `graphlite/src/exec/write_engine/operations/match_insert.rs:506`
  resolves `MATCH (a {id:X})` by calling `graph.get_all_nodes()` and
  `.filter()` in Rust — a **linear filter over the in-memory node
  HashMap**, not a hash lookup. Per-edge.
- `graphlite/src/storage/graph_cache.rs:21`: `nodes: HashMap<String, Node>`
  is keyed by GraphLite-internal id, not by user `id` property. **No
  property index ever**, despite the architecture doc claiming a
  "Property Index".
- `graphlite/src/storage/persistent/sled.rs:97`: `batch_insert` is
  declared but the body is `for (k,v) in entries { self.insert(k,v) }`
  — no `sled::Batch`. Pretending to batch.
- Source comment at `graphlite/src/storage/indexes/traits.rs:61`
  literally reads `// ROADMAP v0.4.0 - Batch index operations for
  bulk data loading` — bulk-load is unbuilt and the maintainers know
  it.

**Other findings during integration**:
- UTF-8 lexer panic on non-ASCII bytes (`graphlite-0.0.1/src/ast/lexer.rs:488`,
  `&input[..N]` slicing without `is_char_boundary` check). Triggered
  by LDBC names like "Amenábar".
- SQL-style `''` escape isn't recognised — only `\'` works. Symptom:
  every Comment INSERT containing `'BBC's'` etc. silently breaks the
  next statement's parse.
- Hard-coded 1000-iteration cap in the lexer (`graphlite-0.0.1/src/ast/lexer.rs:326`,
  "Infinite loop protection") chokes batched INSERTs above ~50
  patterns.

**Largest tested scale (their own benches)**: ~10³ entities
(`benches/session_throughput.rs`: 1K sessions on near-empty graphs;
`benches/catalog_cache_throughput.rs`: 5 schemas / 15 graphs).
LDBC SF0.1 (~604K node+edge entities) is **two-to-three orders of
magnitude past their tested-at scale**.

Full integration log + per-blocker source citations in the
`bench/cross-system-failed-attempts` branch's
[`graphlite/DIVERGENCES.md`](graphlite/DIVERGENCES.md).

### auksys/gqlite (gqlite.org)

Rust core graph database with openCypher subset, multiple backends
(SQLite default, Redb, Postgres). PyPI distribution `gqlitedb`,
module `gqlite`. (NOT to be confused with our gqlite or with the
unrelated PyPI `gqlite` package, which is a name-squatting GraphQL
HTTP client.) Source at gitlab.com/auksys/GQLite. README states
"Development effort has now slowed down" and "still in its early
stage."

**Outcome**: nodes loaded in seconds (288K via UNWIND CREATE,
matching the auksys idiom). Edges then ground at ~440 KB/s of disk
growth indefinitely. Reading their own pokec benchmark
(`crates/gqlitedb/benches/common/pokec.rs`) revealed the canonical
load idiom is one big CREATE statement with shared variables; we
rewrote setup to use it. Setup ran ~10 minutes, made progress, but
extrapolating from rate would have taken hours total. Killed and
rejected.

**Root cause** (verified by reading the engine source):

1. **Properties stored as JSON in a `TEXT` column.** The `gqlite_default_nodes`
   table has `id INTEGER PRIMARY KEY`, `node_key BLOB`, `labels TEXT`,
   `properties TEXT` (JSON). **No index on `node_key`, none on
   `properties`, none on `labels`.** Every `MATCH (n {id:X})` is a
   full-table scan with `json_extract` per row.
2. **No `CREATE INDEX` in the parser.** All four standard syntaxes
   (`CREATE INDEX FOR (n:L) ON (n.p)`, `CREATE INDEX ON :L(p)`, etc.)
   fail with `expected node_pattern or edge_pattern` — the parser
   handles `CREATE` only as the node/edge-creation form. There is no
   DDL surface for indexes in v1.5.1.
3. **Redb backend has the same problem.** `crates/gqlitedb/src/store/redb.rs:540`:
   when matching by a property, does `nodes_table.range::<PersistentKey>(..)`
   — full scan, then linear filter. Switching backend doesn't help.
4. **Interpreter creates one Vec<Node> per CREATE pattern.**
   `crates/gqlitedb/src/interpreter/evaluators.rs:1275`: per pattern,
   calls `store.create_nodes(&mut tx, &name, vec![&n])` — a Vec of
   ONE. The Store trait accepts iterators, but the interpreter never
   gathers them.
5. **No SQLite tuning available.** `Connection::builder().set_option(...)`
   accepts a fixed key set: `path`, `backend`, `url`, `host`, `user`,
   `password`. **No PRAGMA, `journal_mode`, `synchronous`, cache-size,
   batch-mode, or durability knobs.**

**On the upstream's own roadmap** (open GitLab issues, none merged):
- #169 — custom indexes
- #196 — streaming / pipeline execution
- #198 — refactor interpreter into a stream pipeline
- #200 — introduce logical planner

**Designed-for scale** (their own benches):

| File | Nodes | Edges | Patterns |
|---|---|---|---|
| `pokec_micro_import.cypher` | 138 | 138 | 280 |
| **`pokec_tiny_import.cypher`** (largest in `PokecSize` enum) | **4,538** | **12,681** | **17K** |
| `pokec_small_import.cypher` (file exists, NOT in benches) | 10,000 | 121,716 | 132K |
| **LDBC SF0.1 (us)** | **~289K** | **~315K** | **~604K** |

LDBC SF0.1 is **35× their largest benchmark, 5× their largest
data file** (which they ship but don't bench).

Full integration log including all four loading approaches we tried
(per-row UNWIND, CREATE INDEX, id_map dance, canonical single-CREATE)
in the `bench/cross-system-failed-attempts` branch's
[`auksys_gqlite/DIVERGENCES.md`](auksys_gqlite/DIVERGENCES.md).

### webbery/gqlite

C++ graph database with custom JSON-shaped DSL, libmdbx storage.
Source at github.com/webbery/gqlite. Last commit **2023-04-08**
(over 2 years ago at this writing). README markets the DSL as
"Unstable"; CHANGELOG.md is one line. README states the project
is "for testing abilities in ending device" — IoT/edge form
factors.

**Outcome**: did not integrate. Discarded after reading the bison
grammar (`src/gql.y`), the lexer (`src/gql.l`), the C API
(`include/gqlite.h`), and the canonical query examples
(`test/movielens.cpp`, `test/vertex/grammar.gql`), then attempting
the build on Windows MSVC 2022 + bundled CMake.

**Grammar-level blockers** (verified by reading the parser):

1. **DSL cannot express top-level `LIMIT`.** The `limit` token IS
   declared (`gql.y:115`) and emitted by the lexer (`gql.l:147`).
   But **no grammar production rule references it.** Verified by
   `awk` over all production-rule bodies. The complete query
   production is:
   ```
   '{' query_kind '}'
   '{' query_kind ',' a_graph_expr '}'
   '{' query_kind ',' a_graph_expr ',' where_expr '}'
   ```
   No `LIMIT`, no `ORDER BY`, no projection beyond `query_kind`.
   (Caveat: the `limit` keyword does appear in their own
   `test/vertex/grammar.gql:35` inside a `$near` operator clause.
   Tracing it through the grammar still finds no production that
   accepts it there either; the test is likely aspirational. Even
   if it parsed, vector-search top-k semantics aren't the result-
   set LIMIT IC2 needs.)
2. **No label-disjunction syntax.** `query_kind_expr` (`gql.y:436`)
   is precisely `KW_EDGE | LITERAL_STRING | a_graph_properties` —
   single-string label or single property accessor. No array form.
   IC2's "Comment OR Post" requires two separate query-statements
   combined externally.
3. **Per-row load idiom only.** Their own MovieLens loader
   (`test/movielens.cpp`) does one `gqlite_exec` per CSV row with a
   `char[512]` buffer. At LDBC SF0.1 scale (~600K entities), this
   is the same architectural shape that made auksys grind for
   indefinite hours.

**Build-attempt blockers** (Windows MSVC 2022, May 2026):
1. Submodule `libmdbx`'s CMake fails on `git describe --tags` after
   `--depth 1` clone (no tags reachable). Workaround: unshallow +
   synthetic tag.
2. README claim about bundled `tool/{flex,bison}.exe` working
   without dependencies is wrong: MSBuild custom commands don't
   honor `WORKING_DIRECTORY` for cwd-based binary lookup. Worked
   around by pre-generating parser + prefixing `tool/` to PATH.
3. Generated `libmdbx/version.c` fires preprocessor `#error
   "API version mismatch"` because `MDBX_VERSION_MAJOR/MINOR`
   between `mdbx.h` (0,11) and the CMake-templated file (0,0)
   don't match. Worked around by patching out the `#error`.
4. Linker fails: `LNK1181: cannot open input file
   'jump_x86_64_ms_pe_masm.obj'`. Project has hand-written x86_64
   coroutine assembly stubs but CMake never `enable_language(ASM_MASM)`s,
   so MSBuild compiles the `.cpp` consumers but never assembles the
   `.asm` files.

We stopped at #4. Each is patchable, but cumulatively they say the
project's CMake configuration hasn't been exercised on a fresh
Windows checkout in years — consistent with the 2-year-stale last
commit.

**Combined verdict**: the grammar-level blockers alone make the
bench impossible (no LIMIT means IC2 returns thousands of rows
unbounded; no label-disjunction means the query is structurally
different). The build issues are the second-order signal — even
if all four were patched, the DSL still can't express IC2.

Full discard log + grammar citations + build evidence in the
`bench/cross-system-webbery` branch's
[`webbery_gqlite/SKIPPED.md`](webbery_gqlite/SKIPPED.md).

---

## The architectural pattern

Five external systems evaluated. Three failed. They failed in
*structurally similar* ways — not just "this one's slow" but
"this one's missing the same kind of infrastructure the others
also miss." Worth naming for the writeup:

1. **No real bulk-load path.** Every rejected system has a per-row
   exec idiom (one DSL/Cypher statement per CSV row) and a roadmap
   item or open issue for "bulk insert" that's never been built.
   GraphLite's `batch_insert` is a fake (a `for` loop over single
   inserts). auksys's interpreter passes `vec![&n]` (one node) to
   the bulk-capable Store API. Webbery's reference loader uses
   `sprintf` into a 512-byte buffer per row.

2. **No property indexes.** The rejected systems all store property
   values somewhere unindexed: GraphLite in a label-keyed HashMap
   that's filtered linearly, auksys as JSON in a SQLite TEXT column
   with no JSON-extract index, webbery (we couldn't fully verify
   storage layout). The result is the same: every `MATCH (n {id:X})`
   in their query language degrades to O(N) per call. At LDBC scale
   this is fatal.

3. **Parser features missing for canonical query shapes.** auksys's
   parser rejects `CREATE INDEX` in any form. Webbery's grammar has
   no top-level `LIMIT` and no label-disjunction. GraphLite's lexer
   panics on non-ASCII bytes and rejects SQL-style `''` escapes.
   Any one of these forces query rewrites; together they suggest
   the parsers haven't been exercised against the openCypher /
   ISO-GQL-compliant test suites that would catch these.

4. **"Small embedded graph DB" niche, untested at workload scale.**
   Each project's README pitches the same value: small, embedded,
   fast, low-resource. None publish numbers at LDBC SF0.1 or
   larger. Their own benches max out at ~10³-10⁴ entities. LDBC's
   smallest official scale factor is ~10⁵; the published benchmark
   targets are SF100 (10⁷) and up.

The two systems that work:

- **graphqlite** sidesteps the index problem with a Python API that
  hands the user the external→rowid map directly. It's not a graph
  database; it's a Cypher front-end on SQLite. Boring, useful,
  small-scope, works.
- **Kuzu** had real engineering depth (vectorized columnar execution,
  CIDR paper, multi-engineer team funded by Kùzu Inc.) and tested
  themselves against LDBC SF100 internally. The system is now
  archived but the engineering is real. We pin to v0.11.3 and treat
  the frozen wheel as a stable reference.

Our gqlite is the third working system and the fastest — which is
the bench's headline finding — but the comparison is not
"gqlite wins because the others are bad." graphqlite is fine for
its scope; Kuzu is engineering-comparable to gqlite. The systems
**we beat by 1-2 orders of magnitude** are the ones we beat
because they're architecturally weaker, not because they're
abandoned.

## Ablation results

Headline finding: **gqlite's IC2 latency is bought entirely by two
specific architectural choices, not by being written in Rust.** With
either disabled, gqlite is ~30× *slower* than Kuzu and ~17× slower
than graphqlite — it was never just "compiled-language wins."

Run via `bench/cross-system/run_all.sh --ablate` (see the README's
"Ablation mode" section for the orchestration shape and env-var
table). Same SF0.1 dataset, same 15 IC2 param rows, 5 measured iters
+ 2 warmup, canonical run captured at
`bench/cross-system/results/20260505_162656/comparison.txt`.

### Cross-row median IC2 latency

| Mode | Env / args | Median (ms) | Peak RSS over baseline | × baseline |
|---|---|---|---|---|
| `lazy-baseline` | (none) | **0.23** | 432 MiB | 1× |
| `lazy-no-auto-indexes` | `GQLITE_DISABLE_AUTO_INDEXES=1` | **182.69** | **315 MiB** (-117) | 794× slower |
| `lazy-no-fold` | `GQLITE_DISABLE_INDEX_FOLD=1` | **186.15** | 431 MiB (-1) | 810× slower |
| `disk-baseline` | `--backend disk` | **189.94** | **293 MiB** (-139) | 826× slower |

The RSS deltas tell their own story:
- **auto-indexes cost ~117 MiB**: that's the `(label, prop) → NodeId`
  hash + btree maps `LazyGraphStore::build_auto_indexes_bulk()` keeps
  resident. Disabling the build saves the memory but kills the
  start-node lookup.
- **fold pass costs essentially nothing** (~1 MiB): the optimization
  is a code path, not a data structure. The slowdown from disabling
  it is 100% wasted CPU, no RAM saved.
- **disk backend saves ~139 MiB**: the LRU page cache that lazy
  keeps for label/prop pages. This *is* a real RAM saving — it just
  comes with an 826× latency hit.

For context, on the same run:
- kuzu-cypher: 6.52 ms median, 108 MiB RSS (28× slower than
  gqlite-baseline; ~29× *faster* than any ablated gqlite mode)
- graphqlite-cypher: 11.18 ms median, 8 MiB RSS (49× slower than
  gqlite-baseline; ~16× faster than any ablated gqlite mode)

Result counts match across all 15 param rows in every mode (`OK row N:
count=20` × 15) — disabling the optimisations or switching backends
slows the engine but doesn't change the answer set.

### What each disabled flag costs

- **`GQLITE_DISABLE_AUTO_INDEXES=1`**: skips `LazyGraphStore::build_auto_indexes_bulk()`
  at open. Without secondary indexes on `(label, prop)`, IC2's start-
  node lookup `MATCH (p:Person {id: $personId})` degrades from O(1)
  hash lookup to a scan over the candidate-list backing store.
- **`GQLITE_DISABLE_INDEX_FOLD=1`**: keeps the indexes built, but
  disables the LTJ pre-pass that resolves the `Eq` predicate to a
  single NodeId pre-loop and drops `p` from the VEO. Without folding,
  the indexed lookup happens once per LTJ leapfrog round-trip instead
  of once per query — the index is built but barely used.
- **`--backend disk`**: switches from `LazyGraphStore` (LRU page cache
  + secondary indexes built at open) to `DiskGraphStore` (topology in
  RAM, everything else read straight from the page file with no
  caching). The 795× slowdown is in the same range as the two
  optimization-flag modes, but the cause is structurally different:
  **`DiskGraphStore` doesn't override `lookup_node_eq` /
  `lookup_node_range`** (only `LazyGraphStore` does, in
  `src/store/lazy.rs:784-788`), so the LTJ fold pass falls through
  to the default trait impl that returns `None` and the start-node
  lookup degrades to a position scan. **Auto-indexes and the fold
  pass aren't backend-portable today.**

The first two flags target the same hot path from different angles:
the auto-build supplies the index, the fold pass *uses* it. The disk
backend is missing both surfaces entirely. All three slowdowns
(794×/810×/826×) cluster around the same magnitude — the signature
of a single critical optimisation surface (the indexed start-node
lookup) rather than three independent contributions.

### What this is NOT showing

- **Not a TripleIndex ablation.** No env var disables the LTJ
  TripleIndex cache; ablating that would require a small ldbc_bench
  / Runtime change. The cache is the second-biggest gqlite IC2 win
  (CLAUDE.md "Secondary indexes" reports it as a 158× speedup over
  the auto-indexes-only baseline). Documenting it here as a known
  follow-up.
- **Not a language ablation.** This says "gqlite without these
  optimisations is slower than the external Python-wrapped systems."
  It does NOT say "Rust without optimisations equals Python." We
  haven't isolated FFI overhead vs engine work in the external
  systems. Don't read the table that way.
- **`disk-baseline` is not a "disk I/O cost" measurement.** The 826×
  slowdown vs `lazy-baseline` is dominated by the missing index hook,
  not by disk reads. A future `DiskGraphStore` that implements
  `lookup_node_eq` would land somewhere between disk-baseline and
  lazy-baseline — exact position TBD. Right now we cannot separate
  "disk is slow because I/O" from "disk is slow because no indexes"
  because the two are coupled in the implementation.
- **Not a multi-IC story.** Only IC2 is wired up today. The bench
  scaffolding accepts `--ic <n>`; once gqlite's parser supports more
  ICs, the same `--ablate` flag surfaces them in the same table.

The takeaway for the writeup is the first paragraph above:
gqlite's headline number is the optimisations doing their job, not
language choice. Both flags identify the same critical surface
(indexed start-node lookup), and removing either makes gqlite
slower than Kuzu/graphqlite — credit where it's due.

## Coverage of "alternative LDBC IC2 reference points"

Reviewers asking "did you consider X?" — we considered:

- **Neo4j Community** — heavy (JVM, server process), the LDBC
  reference standard. Out of scope for this round; sensible
  follow-up if the bench grows.
- **Memgraph** — actively-developed, has LDBC SNB results
  documented, requires a server. Sensible follow-up.
- **FalkorDB** — Redis module, vectorized, alive. Sensible
  follow-up.
- **DuckPGQ** — graph extension to DuckDB. Possible but probably
  out of scope (different data model).
- **Kuzu forks (bighorn, LadybugDB)** — community continuations
  after Kuzu's archival. Both are weeks-to-months old at this
  writing; revisit in 6 months.

None of these were excluded for being "bad"; they were excluded for
scope (server-process integrations need Docker, ~1 day of work
each; vs. the embedded/Python-wheel systems we did integrate).

## How this maps to the bench's git topology

(Author note: this paragraph is for code reviewers; readers
following the writeup story can skip it.)

The five-branches-on-origin model is:

- `bench/cross-system-phase0` — IC-agnostic refactor (Phase 0).
  Mergeable against `main`.
- `bench/cross-system-kuzu` — Kuzu integration. On phase0.
- `bench/cross-system-redesign` — full-LDBC + system-outer-loop +
  this SURVEY.md. On kuzu.
- `bench/cross-system-failed-attempts` — `graphlite/` + `auksys_gqlite/`
  per-system DIVERGENCES.md. Branched off phase0; draft.
- `bench/cross-system-webbery` — `webbery_gqlite/SKIPPED.md`.
  Branched off kuzu; draft.

The two draft branches (failed-attempts, webbery) carry the
documentation-only scaffolding for the rejected systems. The
working-systems story lives on phase0 → kuzu → redesign.
