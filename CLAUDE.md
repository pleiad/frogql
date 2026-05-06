# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

froGQL — a Rust graph database implementing ISO GQL path pattern matching with single-file storage. Distributed as a CLI binary (`frogql`, modelled on `sqlite3`), an embeddable library, and PyO3 Python bindings published to PyPI as `frogql`. Part of an academic research project (test names occasionally reference paper sections, e.g. `test_paper_example_s2_4`); a separate Python reference interpreter exists in a sister project but is not required to develop or use this crate.

The crate package is still named `gqlrust` for legacy reasons; the user-facing binary, Python module, and PyPI package are all `frogql`. Many internal identifiers, env vars (`GQLITE_TRACE_OPEN`, `GQLITE_DISABLE_AUTO_INDEXES`, etc.), and doc-comments still say "gqlite" — they pre-date the rebrand and are not part of any user surface.

## Commands

```bash
# Full integration test sweep (skip bench_test which has pre-existing failures)
cargo test --lib
cargo test --test parser_test --test runtime_test --test store_runtime_test \
           --test text2gql_test --test parse_and_run_test --test count_test \
           --test null_test --test record_test --test list_test \
           --test compile_diagnostics --test elaborate_test --test float_test \
           --test graph_type_test --test typecheck_smoke --test typecheck_test \
           --test optional_match_test --test multi_match_test \
           --test aggregates_proptest --test lattice_proptest --test multi_match_proptest \
           --test exists_fold_test --test exists_runtime_test \
           --test parser_dm_test --test lazy_mut_test --test dm_runtime_test \
           --test dm_persistence_test --test dm_schema_test --test dm_default_test \
           --test dump_test

# Single test
cargo test --test runtime_test test_join_star_any_label -- --exact

# Strict clippy (run before every commit)
cargo clippy --workspace --all-targets -- -D clippy::all

# Build all binaries (defaults: `repl` + `bench` features on)
cargo build --release

# Interactive REPL
./target/release/frogql movies.gdb --import-csv path/to/csv_dir/   # create + open
./target/release/frogql movies.gdb                                 # open existing
./target/release/frogql movies.gdb --no-typecheck                  # skip typecheck for the session

# Python bindings (builds cdylib, installs `frogql` into the active venv)
cd python && source <your-venv>/bin/activate && pip install maturin && maturin develop --release
# For wheels to ship to other machines:
cd python && maturin build --release   # output in target/wheels/
```

### Pre-commit checklist for Rust changes (non-negotiable)

1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets -- -D clippy::all`
3. **`cargo test`** — run the full sweep above. Skipping this has burned commits before, e.g. a `--` line-comment lexer change broke `-->` edge sugar across three test suites. fmt + clippy alone do not catch lexer/grammar regressions.
4. Stage + commit.

## Workspace layout

Cargo workspace with two members and `resolver = "2"`:
- `.` (root) — the `gqlrust` library crate + CLI binaries under `src/bin/`:
  - `frogql` — interactive REPL with line editing (rustyline). **Requires `repl` feature.**
  - `bench_queries` — generic benchmark runner
  - `bench_setup` — downloads + extracts LDBC datasets via ureq + zstd + tar. **Requires `bench` feature.**
  - `ldbc_bench` — LDBC interactive-complete benchmark driver (queries in `bench/ldbc-queries/*.toml`). **Requires `bench` feature.**
  - `internal_bench` — gqlite-only diagnostic bench (typechecker on/off, lazy/disk backend, RSS, scaling)
  - `convert_edgelist` — edge-list format converter
- `python/` — the `frogql-py` crate: a `cdylib` exposing a PyO3 extension module named `frogql`. Depends on `gqlrust = { path = "..", default-features = false }` so the wheel ships only the library half (no rustyline/ureq/etc.). Built and installed with maturin (`maturin develop` for local dev, `maturin build --release` for wheels). Maturin installs into whichever venv is active.

`resolver = "2"` is required: with v1, building the python crate `--target X` (cross-compile) unifies features globally and drags in `gqlrust`'s default `repl` + `bench` features even when `default-features = false` is set on the dep. That pulled `ureq → ring`, which fails to cross-build on the manylinux2014 aarch64 container. Resolver v2 computes features per-target and isolates the wheel build.

Other top-level directories:
- `src/` — library crate (`parser/`, `elaborate/`, `typing/`, `optimizer/`, `runtime/`, `model/`, `store/`, `lib.rs`)
- `tests/` — integration tests (one file per concern; see test list above)
- `examples/` — pre-built `.gdb` databases (movies, fraud_detection, bom, ldbc-sf01, etc.) plus matching `*_queries.json` query bundles
- `docs/` — `storage-architecture.md`, `JOIN_STRATEGY_NOTES.md`, `implemented-optimizations.md`, `iso-gql-gaps.md`, `graph-type-catalog-plan.md`, `typechecker_migration.md`, `rules.md`
- `bench/` — benchmark scaffolding: `BENCHMARK_PLAN.md`, `LDBC_BENCH_PLAN.md`, `TYPECHECKER_BENCHMARK.md`, `ldbc-queries/*.toml`, `queries/`, `scripts/`, `results/` (benchmark datasets in `bench/data/` are gitignored and downloaded via `bench_setup`)

Python API surface (`python/src/lib.rs`): `frogql.open(path)`, `frogql.import_json(db, json)`, `frogql.import_csv(db, dir)`, and a `Connection` class with `execute(query, limit)`, `schema()`, `graph_types()`, `node_count`, `edge_count`. `execute` returns a list of dicts:
- With `RETURN`: `{alias: value}` rows. Without an explicit `AS`, the runtime falls back to `col0`, `col1`, ... — the alias is what the parser stores via `it.alias()`, and unaliased projections have no canonical name.
- Without `RETURN`: each row is `{var: {kind, id, labels, props}}` for every pattern variable, plus a special `_paths` key holding the matched path(s): `_paths` is a list (one entry per sub-pattern in a comma-join), each a list of node/edge dicts in match order. Mirrors what the REPL prints in its `path` column.

`Connection` is `unsendable` (not thread-safe across Python threads). `frogql.open` eagerly warms the LTJ TripleIndex so the first `execute()` runs at warm-cache speed; the same Arc is reused across every subsequent call.

## Dependencies and feature gating

Always-on: `serde` + `serde_json` (model serialization), `thiserror` (error types). These are the only deps the Python wheel pulls.

Optional, behind features (default on for local `cargo build`):
- `repl = ["dep:rustyline"]` — only `src/bin/frogql.rs` uses it.
- `bench = ["dep:sysinfo", "dep:toml", "dep:ureq", "dep:zstd", "dep:tar"]` — `bench_setup` (download/extract) and `ldbc_bench` (RSS reporting + TOML query specs) only.

`default = ["repl", "bench"]` so plain `cargo build`, CI, and local dev see the same dependency surface as before. The `python/Cargo.toml` opts out via `default-features = false`; combined with workspace `resolver = "2"`, the wheel build never touches `ring/ureq/zstd/tar/rustyline/sysinfo/toml`. **Do not** add a new always-on dep for a bench- or REPL-only crate; gate it behind the appropriate feature and add `required-features = [...]` to the bin entry.

Dev: `proptest` (used by `aggregates_proptest`, `lattice_proptest`, `multi_match_proptest`).

## Releases (PyPI)

Tag-driven publishing via `.github/workflows/release.yml`. Pushing a `v*` tag triggers builds for Linux x86_64+aarch64, macOS x86_64+arm64, Windows x86_64 (manylinux2014, abi3-py38: one wheel per (os, arch) covers CPython 3.8+) plus an sdist, then uploads to PyPI using the `MATURIN_PYPI_TOKEN` secret. The `release` job runs in the `pypi` GitHub Environment (configure required reviewers there for manual approval before publish).

Cutting a release: bump three places in lock-step — `python/pyproject.toml`, `python/Cargo.toml`, and `Cargo.lock` (auto via any `cargo build`) — commit, then `git tag vX.Y.Z && git push origin vX.Y.Z`. The version inside `pyproject.toml` is what PyPI receives; the tag name only triggers the workflow. PyPI rejects re-publishing a version, so bump even for hotfixes.

For local downstream development against a not-yet-published change: `cd python && maturin develop --release` from the active venv of the downstream project. Replaces any pip-installed `frogql` with the local build; re-run after each Rust change. Use without `--release` for fast debug iteration (10-50× slower runtime but seconds to compile).

## Architecture

### Compiler pipeline

`parse → elaborate → typecheck → optimize → run`. Elaboration (`src/elaborate/`) performs ISO-mandated semantic lowering: `(x:L {k: v})` becomes `(x:L) WHERE x.k = v`. The `:` vs `is` split inside descriptors distinguishes value filters from type ascriptions — `{name is str}` stays in the descriptor's `PropertyType`, while `{name: 'Alice'}` is hoisted. Descriptors carry a `value_filters` field that the parser populates and elaboration drains; after elaboration it is always empty. The optimizer is reserved for performance-preserving transforms (predicate pushdown, label-index selection, LTJ join rewriting).

### Entry points

- `compile(query) → PathPattern` — parse + elaborate + typecheck + optimize a path pattern string
- `compile_query(input) → Query` — parse a full `MATCH ... WHERE ... RETURN` query
- `compile_query_with(&Schema, input) → Query` — same, but typechecks against a custom schema (used by REPL/Python bindings to honor the active GRAPH TYPE)
- `compile_query_with_diagnostics(input) → CompileResult` and `compile_query_with_diagnostics_with(&Schema, input)` — same as above but return structured `CompileError` with span info; used by tooling that wants more than a `String`
- `compile_unchecked` / `compile_query_unchecked` — skip the typechecker (escape hatch for bench/test scaffolding)
- `parser::parse_statement(input) → Statement` — top-level parser that distinguishes a query from catalog DDL (`CREATE` / `USE` / `DROP / SHOW / VALIDATE GRAPH TYPE`)
- `Runtime::new(graph).run(&pattern)` — execute against any `GraphAccess` backend
- `Runtime::with_triple_index(graph, Arc<TripleIndex>)` — construct with a pre-built shared index (REPL / Python `Connection` build once at open and pass the same Arc to every Runtime)
- `Runtime::warm_triple_index() -> Arc<TripleIndex>` — force the cache to build now and hand back the Arc for sharing
- `Runtime::run_query(&query, limit)` — execute with RETURN projection
- `Runtime::run_with_limit(&pattern, limit)` — early termination after N results
- `Runtime::invalidate_caches()` — drops cached `TripleIndex` + EXISTS memo; called after every successful DML so the next query rebuilds against the post-mutation graph
- `runtime::dm::run_dm(&store, &dm, schema_for_validation)` — execute one ISO §13 data-modifying statement (INSERT / DELETE / DETACH DELETE). `schema_for_validation` is `Some(&Schema)` only when G2000 should fire (active type is neither DEFAULT nor absent)
- `LazyGraphStore::open_or_create(path)` — sqlite3-style; creates an empty `.gdb` if `path` doesn't exist, then opens it
- `LazyGraphStore::save(path)` — atomic save of the merged base+overlay view (tmp+rename); refreshes DEFAULT before persisting
- `LazyGraphStore::materialize_to_graph()` — decode the merged view into an in-RAM `Graph` with compacted IDs; used by `save` and the dump utility
- `LazyGraphStore::refresh_default_if_dirty()` — re-run `infer_simple_schema` if the catalog's `default_dirty` flag is set; idempotent

### Graph-type catalog

`LazyGraphStore` owns a `RefCell<GraphTypeCatalog>` (`src/runtime/catalog.rs`) loaded from the page chain at `header.catalog_root` on open. The REPL and Python bindings route input via `parse_statement`, dispatch DDL through `catalog_mut()` + `save_catalog()`, and compile queries with `catalog.active_schema()`. The reserved name `DEFAULT` is auto-populated by `infer_simple_schema(&store)` (`src/typing/inference.rs`) at import time and on `USE GRAPH TYPE DEFAULT`; both `CREATE` and `DROP` of `DEFAULT` are rejected. Persistence detail lives in `docs/storage-architecture.md` §3.5.

DDL surface today: `CREATE / USE / DROP GRAPH TYPE`, plus inspection / validation:

- `SHOW GRAPH TYPES` — list all entries with active markers
- `SHOW GRAPH TYPE <name>` — pretty-print one entry (uses `typing::format::format_schema`)
- `SHOW CURRENT GRAPH TYPE` — name + content of the active entry
- `VALIDATE GRAPH TYPE <name>` — walks the data via `typing::validate::validate_against_data` and caches the verdict in `catalog.validations`

`USE` does not validate. The walk is opt-in because it is O(N + E); the typechecker still constrains queries against the active schema either way.

REPL meta-commands follow the SQLite dot-prefix convention (see `src/bin/frogql.rs`): `.schema` aliases `SHOW GRAPH TYPE DEFAULT`, `.schema simple` switches to the grouped by-label renderer in `print_schema_simple` (which lists every node type unconditionally — the earlier "standalone-only" filter hid all nodes on connected graphs and was removed in commit `e23d04d`), `.graph-types` aliases `SHOW GRAPH TYPES`, `.save` materialises the merged base+overlay view to the open `.gdb` atomically (see *Data Modification*), `.dump-json <path>` writes a pg_dump-style JSON snapshot, `.help` lists meta-commands and DDL surface, and `.quit` / `.exit` (plus bare `quit` / `exit`) leave the REPL.

### Data Modification (ISO §13, MVP-0)

DML lands as a layered overlay on top of `LazyGraphStore`. The on-disk file stays untouched until `.save`; mutations live in `RefCell<MutationOverlay>` (`src/store/overlay.rs`). The compiler pipeline shares the parser path with queries: `parse_statement` returns either `Statement::Query` or `Statement::DataModification(DmStatement)`, dispatched to `Runtime::run_query` or the free function `runtime::dm::run_dm`.

Surface accepted in MVP-0:
- `INSERT <path pattern list>` standalone or after a `MATCH` chain.
- `[DETACH | NODETACH] DELETE x [, y, ...]` with bare variable references.
- Optional trailing `RETURN <items>` projecting the post-mutation working table.
- `SET` and `REMOVE` are reserved at the lexer level but the parser rejects them with "not implemented in this version" so they don't collide with property names. They land in MVP-1 along with `DELETE` over arbitrary value expressions (Feature GD04) and `INSERT` properties referencing `mu` (e.g. `MATCH (a) INSERT (b {who: a.name})`).

Architecture notes:
- **Overlay**, not in-place. `MutationOverlay` carries `new_nodes`, `new_edges`, tombstones, and adjacency maps for new edges only. Reads merge base + overlay (`LazyGraphStore`'s `GraphAccess` impl filters tombstones and surfaces overlay entries). The base CSR / page cache stay read-only — keeps SF1 working sets in bounded RAM and avoids invalidating the page cache on every mutation.
- **`GraphAccessMut`** (`src/model/graph_access.rs`) takes `&self` (RefCell-backed), so the existing `Runtime` lifetime that plumbs `&G` everywhere stays untouched. Implemented properly on `LazyGraphStore`; `Graph` (in-RAM JSON fixture) gets stub `unimplemented!` impls because it isn't the production backend.
- **Atomicity**. ISO §13.5 Note 196 + §13.2 GR5/GR6 demand all-or-nothing per statement. `run_dm` builds a list of bindings before mutating, applies them inside a closure, and on any error calls `store.rollback_session()` which clears the overlay. Coarser than per-statement: it discards earlier successful DML in the same session too — accepted limitation in MVP-0 (no transaction boundary smaller than the connection until WAL).
- **Match-chain elaboration**. `run_dm` runs the MATCH chain through `elaborate::elaborate_query` before executing it, so `(a:Person {name: 'Alice'})` lowers `{name: 'Alice'}` into a WHERE filter. Skipping elaboration silently ignores `value_filters` on descriptors and matches too many rows; this bit hard during E2E testing and is the reason the elaborate call sits inside `run_dm`.
- **G2000 validation**. Per-element check via `typing::validate::validate_node_against_schema` / `validate_edge_against_schema`, called from `apply_insert_pattern` only when the active GRAPH TYPE is non-DEFAULT. DEFAULT skips validation because it is data-derived and re-inferred lazily (see below).
- **DEFAULT lifecycle**. `GraphTypeCatalog.default_dirty` (in-RAM only, `#[serde(skip)]`) flips after every successful DML. `LazyGraphStore::refresh_default_if_dirty` re-runs `infer_simple_schema` on next access; called automatically by `handle_show("DEFAULT")` and `LazyGraphStore::save`. Eager refresh after each DML would cost O(N+E) per mutation; lazy + dirty flag keeps DML O(1) and amortises the inference at most once per dirty cycle.
- **TripleIndex invalidation**. Every successful DML calls `Runtime::invalidate_caches()` (REPL) or clears `Connection.triple_index` (Python). The next query rebuilds the six-ordering index from the merged base+overlay view via `TripleIndex::from_graph(&store)` — ~670ms on SF0.1, ~0ms on a fresh DB. Maintaining the six sorted Vecs incrementally would cost O(E) per insert (memmove); rebuild lazy is the right trade for batch-mutate-then-batch-query workloads.

Persistence:
- `LazyGraphStore::open_or_create(path)` mirrors SQLite: opening a non-existent path writes an empty `.gdb` first, then opens it. The REPL emits `creating new database: <path>` to surface the create.
- `LazyGraphStore::save(path)` materialises merged base+overlay into a temporary `Graph`, calls `save_graph_atomic` (writes to `<path>.tmp`, then atomic `rename`), keeps the existing pager fd alive (POSIX rename keeps the old inode while the fd holds it), and refreshes DEFAULT before persisting. Subsequent reads on the same `LazyGraphStore` continue to work coherently against the snapshot at open + overlay; reopening from the file produces the post-save image.
- `Connection.save()` (Python) and `.save` (REPL) both call into this. Auto-commit is OFF by design: forgetting `.save` loses the overlay, mirroring SQLite's explicit-commit semantics.

Dump:
- `store::dump::dump_to_json_file(&store, path)` produces a JSON document in the exact shape `Graph::from_json_value` consumes — round-trip property holds. Available via `.dump-json <path>` in the REPL. The pg_dump-style GQL dump (a script of INSERTs that reconstructs the graph using a temporary `_dump_id` property) is deferred to MVP-1 because it depends on `MATCH+INSERT` with bindings and `REMOVE`, both still pending.

Test files added in this layer (each maps to one of the 8 phases of `~/.claude/plans/que-dice-el-gql-encapsulated-pumpkin.md`): `parser_dm_test.rs`, `lazy_mut_test.rs`, `dm_runtime_test.rs`, `dm_persistence_test.rs`, `dm_schema_test.rs`, `dm_default_test.rs`, `dump_test.rs`. 58 tests total.

### ID system

All node/edge IDs are `u32` internally (`pub type Id = u32` in `model/value.rs`). String names exist only for display via `node_name(id)` / `edge_name(id)` on the trait. `PathValue` variants are `Node(u32)`, `EdgeDirectional(u32)`, `EdgeUndirectional(u32)`, `Nothing`, and `Group(Vec<PathValue>)` — the last is reserved for repetition grouping (`{n,m}` quantifiers) and is NOT a user-facing list. User lists live in `Value::List`. PathValue is NOT Copy because of the Group variant.

### GraphAccess trait

The runtime is generic over `GraphAccess`. Node and edge methods are separate: `node_labels(id)` / `edge_labels(id)`, `node_props(id)` / `edge_props(id)`. The runtime knows which to call from context (filtering nodes vs edges). Three backends:
- `Graph` — in-memory from JSON, all data in RAM
- `LazyGraphStore` — topology (edge_src/tgt) + label index in RAM, labels/props read from disk via LRU page cache. No string names in memory.
- `DiskGraphStore` — topology in RAM, everything else from disk

### Parser grammar hierarchy

```
full_query  = MATCH? query (WHERE expr)? (RETURN items)? (LIMIT INT)?
query       = path_pattern ("," path_pattern)*     ← Join (lowest precedence)
path_pattern = path_term ("|" path_term)*           ← Union
path_term   = path_factor+                          ← Concat (juxtaposition)
path_factor = path_primary quantifier?              ← Repeat {n,m}
```

`MATCH` keyword is optional — bare path patterns like `(x)-[]->(y)` still work. `OPTIONAL MATCH` is supported as a top-level match clause. `is` and `IS` are aliases for the `typed`/`TYPED` type-predicate keyword; `IS NULL` / `IS NOT NULL` are dedicated null tests detected via lookahead before the type-predicate path. The `AS` keyword is ambiguous between type cast (in expressions) and alias (in RETURN); `return_comparison()` excludes `AS` from operators so it's available for aliases.

`LIMIT N` populates `Query.limit: Option<u32>`; the runtime combines it with any caller-supplied cap via `min` (smaller wins). `LIMIT 0` short-circuits to an empty binding table per ISO/IEC 39075:2024.

#### Comments and operator aliases (ISO §3.10)

The lexer's `skip_whitespace` consumes both forms of GQL comments:
- `-- ...` to end of line. **Disambiguation:** `--` followed by `>` is NOT a comment — it's the start of `-->` (unlabeled forward-edge sugar from §5.x). The lexer peeks the third char and falls through to the `-` arm if it's `>`.
- `/* ... */` block, non-nesting.

Operator aliases lexed to the same token:
- `<>` and `!=` both produce `Token::Ne` (ISO §3.10 lists `<>` as the canonical form).
- `is` / `IS` are aliases for `typed`/`TYPED`; `IS NULL` / `IS NOT NULL` are dedicated null tests via lookahead.

Regression tests for these live in `tests/parser_test.rs` (`test_lexer_*`).

### Typechecker

The checker walks each `PathPattern` and produces a `(PathType, TypeEnvironment)` pair. `PathType` tracks the *shape* of a single concatenation and is the source of the `guaranteed_empty` short-circuit. `TypeEnvironment` tracks variable bindings.

For multi-clause queries (`MATCH … MATCH …`, with or without `OPTIONAL`), `check_match_chain` threads the environment through the chain but does NOT propagate `PathType` across matches — joins and left-joins produce independent paths. The first match's path stays as the chain's path for short-circuit purposes; later matches contribute only via the environment.

Two key environment operators (`src/typing/type_environment.rs`):
- `meet` (TJoin rule): for shared keys `x_i`, bind `refine(schema, meet(T_{i1}, T_{i2}))`. Singletons in either side are kept as-is.
- `outer_join` (TLEFTJOIN rule): for shared keys, `T_{i1} ⊔ refine(schema, meet(T_{i1}, T_{i2}))` — left's type joined with the refined meet so an unsatisfiable optional collapses gracefully to the left binding instead of poisoning the env. Left-only keys stay; right-only keys become `T_k ⊔ Null`.

The `refine` operation (the `S ⊢ T ▷ T'` judgment) is `VariableType::refine(schema, &meet)`: meet first, then refine against the schema. Used inside both env operators.

### Join strategy: Leapfrog Triejoin (LTJ)

The runtime uses Leapfrog Triejoin (LTJ) as its primary strategy for joins and concatenations of directed/undirected edges. LTJ is a worst-case-optimal multi-way join algorithm: it binds variables one at a time by intersecting candidate lists across all participating patterns simultaneously, with no intermediate materialisation. The implementation follows the CompactLTJ paper (Arroyuelo et al., VLDBJ 2025).

**The problem it solves.** The previous strategy (pairwise hash-join) materialised both sides of every join before unifying them. For multi-way joins like a 4-clique with 6 sub-queries, intermediate tables grew exponentially (a node of degree 100 produces 10K pairs at the first join, 1M triples at the second, etc.) and most rows were discarded later. LTJ removes this blowup.

**Core idea: everything is a triple.** Each directed edge is modelled as a triple `(src, label, tgt)`, RDF-style. Triples are stored in six sorted orderings (`TripleIndex`): SPO, SOP, POS, PSO, OSP, OPS. Each ordering allows efficient prefix lookups via binary search.

#### From queries to triples

Comma-joins and edge concatenations both decompose into triple sets — a concat is just a join over shared intermediate nodes.

**Example 1: edge chain.** `(a)->(b)->(c)->(d)` decomposes into:

```
Triple 1: (a, _p0, b)    ← first edge
Triple 2: (b, _p1, c)    ← second edge
Triple 3: (c, _p2, d)    ← third edge
```

`_p0`, `_p1`, `_p2` are fresh variables for unconstrained labels. `b` is shared between triples 1 and 2; `c` between 2 and 3. LTJ binds variables in a smart order (the VEO) and intersects candidates step by step.

**Example 2: triangle (3-clique).** `(a)->(b), (b)->(c), (c)->(a)` decomposes into:

```
Triple 1: (a, _p0, b)
Triple 2: (b, _p1, c)
Triple 3: (c, _p2, a)
```

`a` appears in triples 1 and 3, `b` in 1 and 2, `c` in 2 and 3. LTJ does:

```
for each a:
  for each b in out(a):                  ← intersects triples 1 and 2
    for each c in out(b) ∩ in(a):        ← intersects triples 2 and 3
      emit (a, b, c)
```

It never materialises "all (a, b) pairs" before considering `c`.

**Example 3: literal labels.** `(x)-[:Transfer]->(y)` encodes the label as a constant in the triple: `(x, Transfer_id, y)`. The iterator pre-fixes the constant and only walks edges with that label.

**Example 4: anonymous nodes.** Bare nodes like `()` get fresh internal variables (`_ltj_0`, `_ltj_1`, …) that participate in the join but are dropped from the result.

#### Algorithm in steps

1. **Flatten**: the left-associative `Concat` tree is flattened into `[Node, Edge, Node, Edge, Node, …]`.
2. **Triple extraction**: each consecutive `(Node, Edge, Node)` window emits a triple.
3. **TripleIndex**: index is built with six sorted orderings of all graph triples. Each entry is `(u32, u32, u32, u32)` = (comp0, comp1, comp2, edge_id).
4. **Iterators**: each query triple owns an `LtjIterator` that picks the ordering matching its already-fixed positions. Constants are pre-fixed; the iterator selects the right trie based on which S/P/O slots are bound.
5. **VEO (Variable Elimination Order)**: fixes the order in which variables are bound. Non-lonely variables (in 2+ triples) bind first; lonely variables (in a single triple) last. Within each group, weight breaks ties (`pattern_extract::estimate_var_weights`): equality predicate → 1, range comparison → ~10% of index size, label filter → ~25%, otherwise full index size. Lonely-first inversions are rejected because, without secondary indexes on property values, an equality on a lonely variable still requires a full position scan and elevating it above a structural connector trades a cheap leapfrog for a per-row scan.
6. **Leapfrog seek**: to bind a variable, rotate across iterators containing it, calling `leap(c)` (binary search for the smallest value ≥ c). When all iterators agree on a value, that's a candidate.
7. **Recursive descent**: `down()` on iterators after binding, recurse to the next variable, then `up()` to backtrack.

#### In-loop filters

Filters (node labels like `(x: Person)`, pushed-down property-type and value predicates) are placed at the VEO level where all their variables are bound. They evaluate before descending and prune entire sub-trees. Three `FilterKind` variants:
- `NodeLabel { var, label }` — checks the bound node has the required label.
- `NodeProperty { var, prop }` — checks the bound node has the named property.
- `NodeAttrCmp { var, attr, op, value }` — checks `node.attr <op> value` for `=`, `!=` (also spelled `<>` per ISO §3.10), `<`, `<=`, `>`, `>=`. Pushed down by the optimizer from WHERE conjuncts (see *Optimizer*).

#### When LTJ activates

LTJ kicks in automatically in `run_join` and `run_concat_pattern` whenever the pattern is decomposable into triples:

- **Yes**: chains and comma-joins of directed (`-[]->`), reverse (`<-[]-`), and undirected (`~[e]~`) edges, with or without labels. Reverse edges are normalised at extraction time by swapping the endpoints in the emitted triple. Undirected edges are normalised at index build time: each undirected edge is stored as both `(s, p, t)` and `(t, p, s)` with the same `edge_id`, so a forward lookup catches them from either endpoint.
- **No**: any-direction edges (`-[e]-` without tilde), unions (`|`), repetitions (`{n,m}`), and WHERE clauses with non-pushable expressions.

If decomposition fails, the runtime falls back to pairwise hash-join — guarantees no regression.

#### Current limits

1. **Repetitions**: `{n,m}` is not unrolled to triples (could be done for fixed bounds; not implemented). Falls back.
2. **Any-direction edges (without tilde)**: not modelled as triples.
3. **WHERE expressions**: label and pushed value predicates run inside the loop; arbitrary WHERE (e.g. `x.age > y.age` involving multiple bound vars) post-filters.
4. **TripleIndex not persisted**: cached on `Runtime` via `RefCell<Option<Arc<TripleIndex>>>` and built once per Runtime (eagerly at REPL/Connection open via `warm_triple_index()`); the same Arc is shared across every Runtime spawned for that connection. Persisting in the .gdb header chain would skip the build entirely at the cost of ~12% file size.
5. **Static VEO**: variable order is fixed before search. An adaptive VEO that re-estimates cardinalities mid-search would help queries with skewed selectivity. With secondary indexes (see below), an `Eq` predicate on an indexed `(label, prop)` is now a true point lookup via constant-folding; vars without an index still scan their position.

#### Benchmark results (soc-LiveJournal1-100k, limit=1000)

| Query | Hash-join (before) | LTJ (after) | Speedup |
|-------|-------------------|-------------|---------|
| 3-clique | 0.57s | 0.041s | 14× |
| 4-cycle | 4.15s | 0.041s | 101× |
| 3-path | 6.33s | 0.040s | 158× |
| 2-tree | 101.8s | 0.039s | 2610× |
| 4-path | 159.8s | 0.039s | 4097× |
| 4-clique | hung | 0.043s | ∞ |

#### Module structure of `runtime/ltj/`

```
ltj/
  mod.rs              — declarations
  triple_index.rs     — TripleIndex: six sorted orderings, binary search, leap, range queries
  iterator.rs         — LtjIterator: navigation with pre-fixed constants, leap/down/up
  veo.rs              — VeoSimple: fixed order, non-lonely first, weight tiebreaker
  algorithm.rs        — LtjAlgorithm: leapfrog seek/search, LtjRunner with filters
  pattern_extract.rs  — concat flatten, triple decomposition, engine integration, weight estimation
```

### Comma-join fallback (pairwise hash-join)

When LTJ cannot decompose a join (unions, repetitions, any-direction edges), the original pairwise hash-join handles it. Both sides are evaluated fully, a hash index is built on the first shared variable, and the filtered cross-product is emitted. Multi-way joins like `Q1, Q2, Q3` are left-associative: `Join(Join(Q1, Q2), Q3)`.

### Repetition and PathValue::Group

`-[x]->{n,m}` binds `x` to a `Group` of matched edges, not a single edge. `to_group()` wraps each value in a singleton group; `concat_group()` concatenates groups. Nested repetitions produce nested groups: `(-[x]->{1,2}){1,2}` gives `x ↦ [[e1], [e2, e3]]`. The zero-repetition base case fills variables with empty groups.

`engine.rs::run_repetition_range` evaluates `{lb,ub}` in a single pass: the inner pattern runs once, the `first → indices` hash is built once, and every level `1..=ub` grows in a single `rows` buffer reusing the previous level's slice by index. Levels below `lb` get drained at the end via one `Vec::drain`. Replaces an earlier per-length loop that scaled as `O((ub-lb+1) × ub)` with `O(ub)`.

### EXISTS / NOT EXISTS

`Expr::Exists { body }` and `Expr::NotExists { body }` are two distinct AST variants (the optimiser folds each to a different constant). Body is a `Box<Query>` accepting `MATCH`+`WHERE` clauses (one or more, including `OPTIONAL MATCH`); `RETURN`, `GROUP BY`, `LIMIT`, and `DISTINCT` are rejected by the parser since the body's purpose is proving non-emptiness, not projecting.

**Scoping** (typechecker `check_subquery_body`): the body is checked under a clone of the outer environment so outer-bound variables resolve via correlation, then the inner environment is discarded. References to inner-only vars from `RETURN` / outer `WHERE` produce the existing "variable not found" error. Both predicates type as `SimpleType::B`.

**Optimisation** (`src/optimizer/existential.rs`): runs after the per-pattern pushdown passes. Walks every `Expr` reachable from `Query` (WHERE filters, GROUP BY, RETURN, recursive into nested existentials), runs the typechecker on each body against the active schema, and rewrites empty bodies to literals — `false` for `Exists`, `true` for `NotExists`. Catches shape-driven emptiness (a label or property the schema rejects). The pass does not thread outer-scope correlation into the body, so refinement-aware emptiness is left for a future pass; no literal-Boolean propagation either, so an inner-only fold inside an outer body does not collapse the outer.

**Runtime** (`engine.rs::eval_exists`): two regimes share `Runtime::exists_cache: RefCell<HashMap<usize, ExistsCache>>` keyed by the body's heap address.
- *Uncorrelated* (no shared variable with outer μ): `run_match_chain(body, limit=1)` once, cache the bool; subsequent rows reuse it.
- *Correlated*: `run_match_chain(body, 0)` once (full body), project every row onto the correlation set (sorted variable names), store as `HashSet<Vec<PathValue>>`. Per outer row, build the probe key from μ and check membership — semi-join for `EXISTS`, anti-join for `NOT EXISTS`. The body runs at most once per `Runtime` regardless of how many outer rows pass through.

The four phases live in commits `4d13327` (parse + typecheck), `163ee30` (fold optimiser), `134890f` (uncorrelated runtime), `d5b4a45` (correlated runtime). Tests in `tests/parser_test.rs`, `tests/typecheck_test.rs`, `tests/exists_fold_test.rs`, `tests/exists_runtime_test.rs`. Formal type rules in `latex/extension/main.tex` (`\textsc{TExists}`, `\textsc{TNotExists}`, plus `\isEmpty(\matchseq) \equiv e \lor \mathsf{empty}(\Gamma')` and the rewrite rules `\textsc{ExistsEmpty}` / `\textsc{NotExistsEmpty}`).

### Null semantics

`Value::Null` is a first-class variant. Properties that are absent from a node/edge map are treated as null at query time, and explicit nulls round-trip through the on-disk format.

- **3VL in `cmp_values`** (`runtime/mod.rs`): null on either side yields `false`, so a predicate involving null is dropped from the result. Used by both the LTJ filter loop (`NodeAttrCmp`) and the standard scan (`filter_node`/`filter_edge`).
- **Aggregate null elimination** (`engine.rs` `collect_aggregate_values`): both `ExprResult::Failure` and `Success(Value::Null)` are dropped before the reducer runs. Empty aggregates emit `Value::Null`.
- **Wire format**: `PropValue::Null` carries tag byte 6 (no payload). Nested nulls inside lists / records survive the round-trip. Top-level nulls are encoded as key absence — the property is omitted from the on-disk record.
- **Surface syntax**: the lexer accepts `null` / `NULL`. The parser emits `Expr::Const(Value::Null)`. The typechecker maps the literal to `SimpleType::Star` so `WHERE x = null` does not collapse the surrounding type derivation. `IS NULL` and `IS NOT NULL` (parsed via `try_is_null` lookahead) produce an `Expr::IsNull { operand, negated }` that returns `Value::Bool` regardless of operand type; missing-attribute and unbound-variable failures are treated as null.

### CSV loader

`csv_loader::load_from_csv_dir(path)` reads `spanner_import_config.json` to discover node/edge files. Node files are identified by NOT having SRC_ID/DST_ID columns (case-insensitive). The ID column is found by trying: `vid`, `<Label>_id` (case-insensitive), any `*_id` column, then first column. Edge labels are inferred by stripping known node type names from the config's `label` field or the filename. All column lookups are case-insensitive.

### Storage format (.gdb files)

4 KB pages, slotted-page layout for variable-length records. Header page 0 stores root pointers to string table, label indexes, adjacency index, and (since recently) a CSR adjacency root. Node/edge records reference strings by string table ID. Property values are tagged with `VALUE_TYPE_*` constants in `store/record.rs` (Int=0, Str=1, Bool=2, Float=3, List=4, Record=5, Null=6).

Adjacency has two on-disk representations and the loader prefers whichever is present:

- **CSR (preferred, header `csr_adjacency_root`)** — six `Vec<u32>` page chains: `[out_offsets, out_flat, in_offsets, in_flat, und_offsets, und_flat]`. Loaded in O(N + E) total via six big sequential reads; node `n`'s edges are `flat[offsets[n]..offsets[n+1]]`. Stored in memory as three `AdjCsr { offsets: Vec<u32>, flat: Vec<u32> }` on `LazyGraphStore`. Built and written by every `save_graph` call after the format was added (commit `34d97c0`).
- **Legacy per-node chains (header `adjacency_root`)** — one small page chain per node listing `(edge_id, other_node, kind)` triples (kind 0=out, 1=in, 2=und). The loader still understands this format and rebuilds CSR in memory at open via bucket-sort; legacy `.gdb` files keep working but pay ~30× more time on the topology phase (~5s vs ~0.1s for SF0.1) until they're re-saved into the new format.

See `docs/storage-architecture.md` for the full spec.

`StringTable::str_to_id` (the String→ID dedup map used for writes and label-index lookups) is built lazily on first access. `load()` only fills `id_to_str`; the dedup map is `RefCell<Option<HashMap>>` and populated by the first `intern` or `id_for_str` call. Read-only LazyGraphStore queries never trigger the build, saving ~50% of the load cost.

### Open-time performance

`LazyGraphStore::open` runs a fixed pipeline of phases. Each contributes to total open latency; bottlenecks have moved as the format and in-memory layout evolved. Current breakdown on `bench/data/ldbc-sf0.1.gdb` (CSR-format, 327K nodes / 1.5M edges, warm OS cache):

| Phase | Cost | Where |
|---|---|---|
| pager open | ~0 ms | `Pager::open_with_cache` |
| string table | ~80 ms | `StringTable::load` (id_to_str only — see lazy str_to_id) |
| topology + indexes | ~70 ms | `load_from_indexes` — six CSR sub-chains read sequentially |
| catalog | ~0 ms | `catalog_io::read_catalog` |
| secondary index auto-build | ~420 ms | `build_auto_indexes_bulk` — single pass over node records, u32-keyed buckets |
| LTJ TripleIndex (eager) | ~670 ms | `Runtime::warm_triple_index` — six sorted orderings of all triples |
| **total** | **~570 ms warm** | (from a 6.30 s baseline before the optimisation series) |

`GQLITE_TRACE_OPEN=1` prints the per-phase timings. The current dominant phases (TripleIndex, secondary index) are both cheap to write to disk and would drop to a memory-map at the cost of ~12% (TripleIndex) and ~3% (secondary index) extra `.gdb` file size — not yet implemented.

Existing `.gdb` files written before commit `34d97c0` lack the CSR adjacency root and load via the legacy per-node chains (~5 s topology phase). Re-import or re-save to upgrade in place.

### Optimizer

- **Leapfrog Triejoin**: multi-way join + concat optimisation (see above).
- **Type-predicate pushdown**: extracts `x.attr is T` from WHERE conjunctions and merges into the descriptor's property type.
- **Value-predicate pushdown**: extracts `x.attr <op> literal` (for `=`, `!=`, `<`, `<=`, `>`, `>=`) and stores it on the node descriptor's `value_preds` field. Pattern extraction emits a `FilterKind::NodeAttrCmp` per predicate; the LTJ runner evaluates it in-loop. Restricted to nodes today; edge value predicates fall through to the residual WHERE.
- **Index-driven constant folding**: when a `NodeAttrCmp { Eq, value }` predicate matches a known secondary index on the variable's `(label, prop)`, `pattern_extract::fold_indexed_constants` resolves the predicate to a single NodeId via `GraphAccess::lookup_node_eq`, substitutes `Term::Variable → Term::Constant` in every triple position, drops the satisfied filter, and pre-binds the variable in the result tuple. The variable is excluded from the VEO so leapfrog never enumerates its position. An empty index hit short-circuits the entire pattern to zero rows. See `runtime/ltj/pattern_extract.rs::FoldOutcome`.
- **VEO selectivity-aware tiebreaker**: per-variable weights bias the binding order within each lonely / non-lonely group toward filter-narrowed candidates.
- **Label index selection**: picks the smallest indexed set for compound labels like `A & B` via `LabelType::required_labels()`.

### Secondary indexes

`LazyGraphStore` owns a `RefCell<SecondaryIndex>` (`src/store/secondary_index.rs`) populated at open by `LazyGraphStore::build_auto_indexes_bulk()` — a single O(N) pass over node records that decodes each exactly once and keys bucket maps by `(label_sid, prop_sid)` to avoid `String` allocations during the build. Two flavours coexist on the same `(label, prop)` pair:

- **Hash** for equality (`x.attr = literal`).
- **BTree** for range filters (`<`, `<=`, `>`, `>=`).

Auto-inference builds both kinds for every `(label, prop)` whose values are unique within the label — captures the LDBC IC start lookups (`Person.id`, `Tag.name`, `Country.name`, `TagClass.name`, plus every other `*_id` column) AND the IC2/3/4/9 temporal range filters (`Comment.creationDate`, `Post.creationDate`, `Forum.creationDate`) without any DDL. Floats / lists / records / nulls are not indexable (`IndexKey` covers `Int`, `Str`, `Bool` only). Skip the auto-build entirely with `GQLITE_DISABLE_AUTO_INDEXES=1`.

DDL: `CREATE [HASH | BTREE] INDEX [<name>] ON :Label(prop) [USING HASH | BTREE]`, `DROP INDEX <name>`, `SHOW INDEXES` (or the REPL meta-command `.indexes`). Both prefix (`CREATE BTREE INDEX foo`) and suffix (`USING BTREE`) syntaxes work; HASH is the default kind. HASH and BTREE coexist; re-declaring the same kind on the same `(label, prop)` is the only conflict.

The store exposes two trait methods: `GraphAccess::lookup_node_eq(label, prop, value) -> Option<Vec<Id>>` and `lookup_node_range(label, prop, lo, hi) -> Option<Vec<Id>>`. The in-memory `Graph` returns `None` from both and falls back to scan.

LTJ wiring (`pattern_extract::fold_indexed_constants` and `fold_range_filters`):
- Eq predicates that hit a hash index → constant-fold the variable everywhere (drops the filter, removes the var from VEO, pre-binds it in the result tuple). An empty index hit short-circuits to zero rows.
- Range predicates that hit a btree → precompute the matching sorted set, replace the `NodeAttrCmp` with `FilterKind::NodeInSet { var, set }`. The runner does an O(log n) binary search instead of reading the candidate's property from the page cache.

Memory-only for now (rebuilt every open). Persistence in the .gdb file header chain — so DDL-declared indexes survive close/reopen — is the next roadmap item.

Diagnostic env vars: `GQLITE_DEBUG_INDEXES=1` prints the auto-built indexes and pinned variables; `GQLITE_DISABLE_INDEX_FOLD=1` disables the LTJ pre-pass for A/B benchmarking; `GQLITE_DISABLE_AUTO_INDEXES=1` skips the auto-build at open; `GQLITE_TRACE_OPEN=1` prints per-phase open timings.

Measured impact on LDBC IC2 over `bench/data/ldbc-sf0.1.gdb` (15 params × 3 iters, lazy backend, `--limit 20`):

| Stage | IC2 across-row median |
|---|---|
| No secondary indexes, no Triple cache | 2417 ms |
| Auto hash + btree, no Triple cache | 1377 ms |
| Auto hash + btree, **TripleIndex cached + warmed at open** | **8.7 ms** (276× total) |

For reference, GraphQLite (a SQLite extension with Cypher) measures 32.82 ms median on the same query — gqlite is ~3.75× faster after the cache lands. The single biggest win is the TripleIndex cache; secondary indexes account for the early phases of the speedup but are dwarfed by the savings of not rebuilding the six-ordering edge index per query.

## Key conventions

- Labels in patterns require the `:` prefix: `-[:Transfer]->`, not `-[Transfer]->`.
- Run `cargo fmt --all`, then `cargo clippy --workspace --all-targets -- -D clippy::all`, then **`cargo test`** before every commit (see *Pre-commit checklist*). fmt + clippy alone do not catch parser/lexer regressions.
- The `bench_test` integration target has pre-existing failures — exclude it from regular runs.
- `bench/data/` is gitignored (large datasets, downloaded via `cargo run --bin bench_setup`).
- Example databases in `examples/*.gdb` ARE committed (small, useful for testing).
- Property values are tagged with `VALUE_TYPE_*` constants in `store/record.rs` (Int=0, Str=1, Bool=2, Float=3, List=4, Record=5, Null=6); changing the order is a breaking on-disk format change.
- The Python wheel (`frogql` on PyPI) MUST stay independent of `repl` / `bench` features — never reference `rustyline`, `ureq`, `zstd`, `tar`, `sysinfo`, or `toml` from library code.
- New bins that need optional deps: declare them as explicit `[[bin]]` entries in `Cargo.toml` with `required-features = [...]`. Auto-discovery still picks up bins that use only always-on deps.
