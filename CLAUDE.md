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
           --test dump_test --test dm_set_test --test dm_remove_test \
           --test dm_label_test --test dm_delete_expr_test --test memory_mut_test \
           --test path_prefix_test --test shortest_bfs_test

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

# Browser WASM bindings (frogql-wasm)
rustup target add wasm32-unknown-unknown            # one-time
cargo test -p frogql-wasm                           # host tests of the engine core (query_json/dm_json)
cargo build -p frogql-wasm --target wasm32-unknown-unknown
# Generate the publishable npm package (web target); release-wasm.yml runs the same:
cargo install wasm-pack && wasm-pack build wasm --target web --out-dir pkg
```

### Pre-commit checklist for Rust changes (non-negotiable)

1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets -- -D clippy::all`
3. **`cargo test`** — run the full sweep above. Skipping this has burned commits before, e.g. a `--` line-comment lexer change broke `-->` edge sugar across three test suites. fmt + clippy alone do not catch lexer/grammar regressions.
4. Stage + commit.

## Workspace layout

Cargo workspace with four members and `resolver = "2"`:
- `.` (root) — the `gqlrust` library crate + CLI binaries under `src/bin/`:
  - `frogql` — interactive REPL with line editing (rustyline). **Requires `repl` feature.**
  - `bench_queries` — generic benchmark runner
  - `bench_setup` — downloads + extracts LDBC datasets via ureq + zstd + tar. **Requires `bench` feature.**
  - `ldbc_bench` — LDBC interactive-complete benchmark driver (queries in `bench/ldbc-queries/*.toml`). **Requires `bench` feature.**
  - `internal_bench` — gqlite-only diagnostic bench (typechecker on/off, lazy/disk backend, RSS, scaling)
  - `convert_edgelist` — edge-list format converter
- `python/` — the `frogql-py` crate: a `cdylib` exposing a PyO3 extension module named `frogql`. Depends on `gqlrust = { path = "..", default-features = false }` so the wheel ships only the library half (no rustyline/ureq/etc.). Built and installed with maturin (`maturin develop` for local dev, `maturin build --release` for wheels). Maturin installs into whichever venv is active.
- `node/` — the `frogql-node` crate: a `cdylib` exposing a napi-rs extension named `frogql`. Same `default-features = false` discipline as `python/`. Built and packaged via `@napi-rs/cli` (see `node/package.json` scripts). Distributed on npm as a host package (`frogql`) plus five platform sub-packages (`frogql-darwin-x64`, `frogql-darwin-arm64`, `frogql-linux-x64-gnu`, `frogql-linux-arm64-gnu`, `frogql-win32-x64-msvc`) declared in `optionalDependencies`; npm picks the right one at install time. The host's `index.js` (platform dispatcher) and `index.d.ts` (TS types) are auto-generated by `napi build` but **checked into git** — required so the publish job can ship them without rebuilding.
- `wasm/` — the `frogql-wasm` crate: a `cdylib` (+ `rlib` for host tests) exposing a `wasm-bindgen` module for the **browser**. Same `default-features = false` discipline. Wraps **`MemoryGraphStore`** only (no filesystem in the browser): `open_json(json)` → `Connection` with `execute(query, limit?)`, `to_json()`, `schema()`, `node_count`, `edge_count`. Read queries return row objects; INSERT/SET/DELETE work in RAM via the overlay; DDL / `CREATE INDEX` are rejected (no catalog/index in-memory). Persistence is the JSON string from `to_json()` round-tripped through `open_json` (store it in IndexedDB). Build for the browser with `wasm-pack build --target bundler` (needs `wasm-pack` + the `wasm32-unknown-unknown` target); the engine core (`query_json`/`dm_json`) has host-target unit tests runnable via `cargo test -p frogql-wasm`. See `docs/internals/wasm-browser-plan.md`.

`resolver = "2"` is required: with v1, building the python crate `--target X` (cross-compile) unifies features globally and drags in `gqlrust`'s default `repl` + `bench` features even when `default-features = false` is set on the dep. That pulled `ureq → ring`, which fails to cross-build on the manylinux2014 aarch64 container. Resolver v2 computes features per-target and isolates the wheel build. Same trick keeps the node crate's wheels clean.

Top-level dirs: `src/` (library: parser, elaborate, typing, optimizer, runtime, model, store), `tests/` (integration), `examples/*.gdb` (committed sample databases), `docs/internals/` (architecture write-ups — see `JOIN_STRATEGY_NOTES.md`, `implemented-optimizations.md`, `storage-architecture.md`, `iso-gql-gaps.md`), `bench/` (LDBC scaffolding; `bench/data/` is gitignored, downloaded via `bench_setup`).

**Python API** (`python/src/lib.rs`): `frogql.open(path)`, `frogql.import_json`, `frogql.import_csv`, and a `Connection` class (`execute(query, limit)`, `schema()`, `graph_types()`, `node_count`, `edge_count`). `execute` returns a list of dicts: `{alias: value}` rows when `RETURN` is present (unaliased projections fall back to `col0`, `col1`, …); otherwise `{var: {kind, id, labels, props}}` per pattern variable plus a `_paths` key (list per comma-join sub-pattern, each a list of node/edge dicts in match order). `Connection` is `unsendable` (not thread-safe). `frogql.open` eagerly warms the LTJ TripleIndex; the Arc is reused across every `execute`.

**Node API** (`node/src/lib.rs`): same module + class surface as Python, camelCased per napi-rs convention: `open(path)`, `importJson`, `importCsv`, and `Connection` with `execute(query, limit?)`, `save()`, `schema()`, `graphTypes()`, `nodeCount`, `edgeCount`. Polymorphic `execute()` returns `unknown` in TS; cast to one of the exported interfaces (`SchemaSummary`, `GraphTypeSummary`, `NodeRef`, `EdgeRef`, `DmCounters`, `DdlOk`, `IndexResult`, `IndexSummary`) per statement kind. `schema()` and `graphTypes()` return strongly-typed structs directly. `Connection` is `unsafe impl Send` — napi runtime is single-threaded per V8 isolate so Sync is not required. `open()` eagerly warms the TripleIndex, same as Python.

## Dependencies and feature gating

Always-on: `serde` + `serde_json` (model serialization), `thiserror` (error types). These are the only deps the Python wheel pulls.

Optional, behind features (default on for local `cargo build`):
- `repl = ["dep:rustyline"]` — only `src/bin/frogql.rs` uses it.
- `bench = ["dep:sysinfo", "dep:toml", "dep:ureq", "dep:zstd", "dep:tar"]` — `bench_setup` (download/extract) and `ldbc_bench` (RSS reporting + TOML query specs) only.

`default = ["repl", "bench"]` so plain `cargo build`, CI, and local dev see the same dependency surface as before. The `python/Cargo.toml` opts out via `default-features = false`; combined with workspace `resolver = "2"`, the wheel build never touches `ring/ureq/zstd/tar/rustyline/sysinfo/toml`. **Do not** add a new always-on dep for a bench- or REPL-only crate; gate it behind the appropriate feature and add `required-features = [...]` to the bin entry.

Dev: `proptest` (used by `aggregates_proptest`, `lattice_proptest`, `multi_match_proptest`).

## Releases (PyPI + npm in lock-step)

One git tag fires all three registries (PyPI wheel, native npm, browser WASM npm). Pushing `v*` triggers:
- `.github/workflows/release.yml` → builds wheels (Linux x86_64+aarch64, macOS x86_64+arm64, Windows x86_64; manylinux2014, abi3-py38) + sdist, uploads via `MATURIN_PYPI_TOKEN`, runs in the `pypi` GitHub Environment for required-reviewers gating.
- `.github/workflows/release-npm.yml` → 5-target build matrix (mac arm64 native, mac x64 cross-compiled from arm64, linux x64 native, linux arm64 via zig, windows x64 native), publishes the host `frogql` package plus the 5 platform sub-packages via `NPM_TOKEN`, runs in the `npm` GitHub Environment. Pre-release versions (any with a `-` like `0.2.0-rc.3`) land on dist-tag `next`; clean `v0.2.0` lands on `latest`.
- `.github/workflows/release-wasm.yml` → single platform-independent build (`wasm-pack build wasm --target web`), publishes the **`frogql-wasm`** npm package (unscoped, consumed as `import init, { open_json } from "frogql-wasm"`). Reuses the `npm` Environment + `NPM_TOKEN`. WebAssembly is portable, so there's no build matrix. Same dist-tag logic and idempotent skip-if-exists as the napi job. The `web` target (not `bundler`) is deliberate: it needs no `vite-plugin-wasm` in the consumer.

Cut a release by bumping **five files** in lock-step plus regenerating `Cargo.lock` (auto on any `cargo build`):
- `python/pyproject.toml` (PEP 440 form: `0.2.0rc3`)
- `python/Cargo.toml` (semver: `0.2.0-rc.3`)
- `node/Cargo.toml`
- `node/package.json` (host version + the 5 `optionalDependencies` versions)
- `wasm/Cargo.toml` (semver; the published `frogql-wasm` version is derived from it by wasm-pack)

Then `git tag vX.Y.Z && git push origin vX.Y.Z`. Both registries reject re-publishing, so always bump. The npm release also requires `node/index.js` + `node/index.d.ts` to be committed at the tagged SHA; regenerate them with `npm run build` inside `node/` whenever the API surface changes and commit the diff.

**npm publish quirks** to know about:
- The host's `npm publish` runs with `--ignore-scripts`. A `prepublishOnly` hook would call `napi prepublish` which recursively re-publishes every platform sub-package and trips 409s on re-runs.
- The platform sub-packages publish from `npm/<triple>/` dirs created at workflow time by `napi create-npm-dir` (reads `napi.triples.additional` from `node/package.json`); `napi artifacts` then moves the downloaded `.node` binaries into each.
- The publish step is idempotent: each candidate is checked with `npm view <pkg>@<version>` first and skipped if already on the registry. Platform publish failures are soft (warning) so a temporary block on one target doesn't kill the run; the host publish always proceeds.
- **First-time publish of any `*-win32-*` platform package can trip npm's anti-squatting spam filter.** When that happens, open a ticket at https://www.npmjs.com/support describing the multi-package release pattern; whitelisting takes 24–72 h.

Local downstream dev:
- Python: `cd python && maturin develop --release` from the venv replaces the pip-installed wheel with a local build; drop `--release` for fast debug iteration.
- Node: `cd node && npm install && npm run build` produces `frogql.<platform>.node` + refreshes `index.js`/`index.d.ts` in place; `npm test` runs the smoke suite, `npm run typecheck` runs `tsc --noEmit` against `__test__/types.test.ts`.

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
- `LazyGraphStore::materialize_to_graph()` — decode the merged view into an in-RAM `MemoryGraphStore` with compacted IDs; used by `save` and the dump utility
- `LazyGraphStore::refresh_default_if_dirty()` — re-run `infer_simple_schema` if the catalog's `default_dirty` flag is set; idempotent

### Graph-type catalog

`LazyGraphStore` owns a `RefCell<GraphTypeCatalog>` (`src/runtime/catalog.rs`) loaded from the page chain at `header.catalog_root` on open. The REPL and Python bindings route input via `parse_statement`, dispatch DDL through `catalog_mut()` + `save_catalog()`, and compile queries with `catalog.active_schema()`. The reserved name `DEFAULT` is auto-populated by `infer_simple_schema(&store)` (`src/typing/inference.rs`) at import time and on `USE GRAPH TYPE DEFAULT`; both `CREATE` and `DROP` of `DEFAULT` are rejected. Persistence detail lives in `docs/internals/storage-architecture.md` §3.5.

DDL surface today: `CREATE / USE / DROP GRAPH TYPE`, plus inspection / validation:

- `SHOW GRAPH TYPES` — list all entries with active markers
- `SHOW GRAPH TYPE <name>` — pretty-print one entry (uses `typing::format::format_schema`)
- `SHOW CURRENT GRAPH TYPE` — name + content of the active entry
- `VALIDATE GRAPH TYPE <name>` — walks the data via `typing::validate::validate_against_data` and caches the verdict in `catalog.validations`

`USE` does not validate. The walk is opt-in because it is O(N + E); the typechecker still constrains queries against the active schema either way.

REPL meta-commands follow the SQLite dot-prefix convention (see `src/bin/frogql.rs`): `.schema` aliases `SHOW GRAPH TYPE DEFAULT`, `.schema simple` switches to the grouped by-label renderer in `print_schema_simple` (which lists every node type unconditionally — the earlier "standalone-only" filter hid all nodes on connected graphs and was removed in commit `e23d04d`), `.graph-types` aliases `SHOW GRAPH TYPES`, `.save` materialises the merged base+overlay view to the open `.gdb` atomically (see *Data Modification*), `.dump-json <path>` writes a pg_dump-style JSON snapshot, `.dump-gql <path>` writes a GQL script that recreates the graph, `.help` lists meta-commands and DDL surface, and `.quit` / `.exit` (plus bare `quit` / `exit`) leave the REPL.

### Data Modification (ISO §13, MVP-0 + MVP-1)

DML is a layered overlay on `LazyGraphStore`: mutations live in `RefCell<MutationOverlay>` (`src/store/overlay.rs`); the on-disk `.gdb` stays untouched until `.save`. Compiler shares the parser path with queries — `parse_statement` returns `Statement::Query` or `Statement::DataModification(DmStatement)`, dispatched to `Runtime::run_query` or `runtime::dm::run_dm`.

Surface today: `INSERT`, `SET <x.prop = expr | x = {...}>`, `REMOVE x.prop`, `SET / REMOVE <x:Label | x IS Label>`, `[DETACH | NODETACH] DELETE <expr-list>`, optional trailing `RETURN`. At most one DML op per statement; multi-DML chains (`MATCH … INSERT … SET …`) deferred. Full surface and per-feature semantics in `docs/data-modification.md`.

Operational invariants Claude must keep in mind:
- **Atomicity**: per-statement all-or-nothing. `run_dm` collects bindings first, applies in a closure, on error calls `store.rollback_session()` (clears the entire overlay — coarser than per-statement, no smaller transaction boundary until WAL).
- **Match-chain elaboration**: `run_dm` runs the MATCH chain through `elaborate::elaborate_query` before iterating. **Skipping elaboration silently drops `value_filters` on descriptors** (`{name: 'Alice'}` becomes equivalent to `{}`) and matches too many rows. The elaborate call lives inside `run_dm` for this reason.
- **G2000 validation**: per-element check via `typing::validate::*`, fires from `apply_insert_pattern` only when the active GRAPH TYPE is non-DEFAULT (DEFAULT is data-derived).
- **DEFAULT lifecycle**: `GraphTypeCatalog.default_dirty` (in-RAM only, `#[serde(skip)]`) flips after every successful DML; `refresh_default_if_dirty` re-runs `infer_simple_schema` lazily on `handle_show("DEFAULT")` and `LazyGraphStore::save`. Eager refresh would cost O(N+E) per mutation.
- **Cache invalidation**: every successful DML calls `Runtime::invalidate_caches()` (REPL) or clears `Connection.triple_index` (Python); next query rebuilds the six-ordering index from base+overlay (~670 ms SF0.1).

**Persistence (`.save`).** `LazyGraphStore::save(path)` materialises merged base+overlay into a temporary `MemoryGraphStore` and calls `save_graph_with_catalog_and_indexes_atomic` (writes graph + catalog + persisted DDL index list to `<path>.tmp`, atomic `rename`). `LazyGraphStore::open_or_create(path)` mirrors SQLite — non-existent path writes an empty `.gdb` first.

**Auto-commit is OFF by design.** Forgetting `.save` loses the overlay (DML) AND any DDL declared this session (`CREATE INDEX` / `DROP INDEX` only mutate the in-memory `RefCell<SecondaryIndex>` via `build_declared`; the persisted list at `header.secondary_index_root` rewrites only on `.save`). Same trade-off as SQLite's explicit-commit model — lets users experiment without writing.

**Dump**: `store::dump::dump_to_json_file` (round-trips through `MemoryGraphStore::from_json_value`) and `dump_to_gql_file` (emits an `INSERT`-per-node + `MATCH+INSERT`-per-edge script; uses a synthetic `_dump_id` property). Available via `.dump-json` / `.dump-gql` REPL meta-commands.

Tests: `parser_dm_test.rs`, `lazy_mut_test.rs`, `dm_runtime_test.rs`, `dm_persistence_test.rs`, `dm_schema_test.rs`, `dm_default_test.rs`, `dump_test.rs`, `dm_set_test.rs`, `dm_remove_test.rs`, `dm_label_test.rs`, `dm_delete_expr_test.rs`.

### ID system

All node/edge IDs are `u32` internally (`pub type Id = u32` in `model/value.rs`). String names exist only for display via `node_name(id)` / `edge_name(id)` on the trait. `PathValue` variants are `Node(u32)`, `EdgeDirectional(u32)`, `EdgeUndirectional(u32)`, `Nothing`, and `Group(Vec<PathValue>)` — the last is reserved for repetition grouping (`{n,m}` quantifiers) and is NOT a user-facing list. User lists live in `Value::List`. PathValue is NOT Copy because of the Group variant.

### GraphAccess trait

The runtime is generic over `GraphAccess`. Node and edge methods are separate: `node_labels(id)` / `edge_labels(id)`, `node_props(id)` / `edge_props(id)`. The runtime knows which to call from context (filtering nodes vs edges). Three backends:
- `MemoryGraphStore` — in-memory from JSON, all data in RAM. Full read + DML backend: implements `GraphAccess` and `GraphAccessMut` via the same `RefCell<MutationOverlay>` as `LazyGraphStore` (reads merge base + overlay; mutations stage in the overlay). Parity covered by `tests/memory_mut_test.rs`.
- `LazyGraphStore` — topology (edge_src/tgt) + label index in RAM, labels/props read from disk via LRU page cache. No string names in memory.
- `DiskGraphStore` — topology in RAM, everything else from disk

### Parser grammar hierarchy

```
full_query   = MATCH? query (WHERE expr)? (RETURN items)? (GROUP BY ...)? (ORDER BY ...)? (LIMIT INT)?
query        = operand ("," operand)*               ← Join (lowest precedence)
operand      = path_prefix? path_pattern            ← Selected (ISO §16.6 path-pattern prefix)
path_pattern = path_term ("|" path_term)*           ← Union
path_term    = path_factor+                         ← Concat (juxtaposition)
path_factor  = path_primary quantifier?             ← Repeat {n,m} / `*` / `+` / `?`
```

`MATCH` keyword is optional — bare path patterns like `(x)-[]->(y)` still work. `OPTIONAL MATCH` is supported as a top-level match clause. `is` and `IS` are aliases for the `typed`/`TYPED` type-predicate keyword; `IS NULL` / `IS NOT NULL` are dedicated null tests detected via lookahead before the type-predicate path. The `AS` keyword is ambiguous between type cast (in expressions) and alias (in RETURN); `return_comparison()` excludes `AS` from operators so it's available for aliases.

#### Path-pattern prefixes (ISO §16.6)

A `path_prefix` is parsed per comma operand (`parse_path_prefix` in `path_pattern_operand`), so it scopes to one `<path pattern>` and does not leak across a comma-join or union. A non-trivial prefix wraps its pattern in `PathPattern::Selected { prefix, pattern }`; the trivial `WALK ALL` is dropped (stored as a plain pattern) so the runtime skips the materialize-and-select pass. The prefix carries a `PathMode` (restrictive) and a `PathSearch` (selective), both in `src/syntax/path_prefix.rs`:

- **Path modes** (`WALK` default, `TRAIL`, `SIMPLE`, `ACYCLIC`) constrain which walks count: TRAIL forbids repeated edges, ACYCLIC forbids repeated nodes, SIMPLE forbids repeated nodes except a closing first==last cycle.
- **Path searches** (`ALL` default, `ANY [N]`, `SHORTEST [N] [PATHS]`, `SHORTEST N GROUPS`) pick a subset per `(first node, last node)` boundary partition. The surface forms `ANY SHORTEST` / `ALL SHORTEST` normalize to `SHORTEST 1 PATHS` / `SHORTEST 1 GROUPS` (ISO §16.6 SR 2c).

`ANY` lexes to its own `Token::Any` (so `ANY <pattern>` is distinct from a `*` type wildcard); in label position `(x:ANY)` it stays an alias for the `*` any-label wildcard (`label_primary` accepts both). `TRAIL/SIMPLE/ACYCLIC/SHORTEST/GROUPS/PATHS/WALK` are soft keywords, matched case-insensitively only in prefix position, so they remain usable as labels and variable names elsewhere. Tests in `tests/path_prefix_test.rs`.

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

Primary strategy for joins and concatenations of directed/undirected edges. Worst-case-optimal multi-way join: each directed edge is a triple `(src, label, tgt)` indexed in six sorted orderings (`TripleIndex`: SPO, SOP, POS, PSO, OSP, OPS); LTJ binds variables one at a time by leapfrog-intersecting candidate lists across triples, no intermediate materialisation. CompactLTJ paper (Arroyuelo et al., VLDBJ 2025). Module structure: `runtime/ltj/{triple_index, iterator, veo, algorithm, pattern_extract}.rs`. Full algorithm walkthrough, examples, and benchmark numbers in `docs/internals/JOIN_STRATEGY_NOTES.md`.

**Activation** (`run_join`, `run_concat_pattern`): kicks in automatically when the pattern decomposes into triples — chains / comma-joins of directed (`-[]->`), reverse (`<-[]-`), and undirected (`~[e]~`) edges, with or without labels. Falls back to pairwise hash-join for any-direction edges (`-[e]-` without tilde), Unions, and Repeats not handled by the unroll optimiser.

**`decompose_flat_chain` accepts adjacent / trailing edges**: when two edges sit adjacent (`~[:knows]~~[:knows]~`, written by users or produced by the unroll pass) it synthesises a fresh anonymous variable as the boundary, mirroring the runtime `Concat` evaluator's `last_node_id` / `first_node_id` path-merge. Without this, two consecutive edges fell off the LTJ path entirely — 270× regression on `(person)~[:knows]~~[:knows]~(other)<-[:hasCreator]-(msg)` style chains.

**In-loop filters** (`FilterKind`): `NodeLabel`, `NodeProperty`, `NodeAttrCmp` (`=`, `!=`, `<`, `<=`, `>`, `>=`), `NodeInSet` (btree-resolved range). Placed at the VEO level where all dependencies are bound; pushed down by the optimizer from WHERE conjuncts.

**Current limits**:
1. Repetitions `{n,m}`: unrolled by `optimizer::unroll_repeat` for bounded ranges with no named inner variables and single-edge inner. Other bounded shapes (named edge/node vars, range > `MAX_UNROLL = 8`) stay on the hash-join repetition path. Unbounded repetition (`*`/`+`/`{n,}`) is not an LTJ shape; it requires a §16.6 prefix and runs through the dedicated finite searches (`run_repetition_shortest` / `run_repetition_unbounded_mode`, see *Path-pattern prefixes*).
2. Any-direction edges (without tilde): not modelled as triples.
3. WHERE: label and pushed value predicates run inside the loop; arbitrary WHERE post-filters. Var-vs-var predicates (`a <> b`) are not pushed into the LTJ filter set yet — they evaluate post-pattern via `PathPattern::Filter`.
4. TripleIndex not persisted: cached on `Runtime` via `RefCell<Option<Arc<TripleIndex>>>`, built once per Runtime (eagerly at REPL/Connection open via `warm_triple_index()`).
5. Static VEO: variable order is fixed before search. With secondary indexes, `Eq` predicates on indexed `(label, prop)` are point lookups via constant-folding; vars without an index scan their position.

When LTJ can't decompose, the pairwise hash-join takes over: both sides evaluated fully, hash index on first shared variable, filtered cross-product. Multi-way joins like `Q1, Q2, Q3` are left-associative: `Join(Join(Q1, Q2), Q3)`. Guarantees no regression vs the pre-LTJ runtime.

### Repetition and PathValue::Group

`-[x]->{n,m}` binds `x` to a `Group` of matched edges, not a single edge. `to_group()` wraps each value in a singleton group; `concat_group()` concatenates groups. Nested repetitions produce nested groups: `(-[x]->{1,2}){1,2}` gives `x ↦ [[e1], [e2, e3]]`. The zero-repetition base case fills variables with empty groups.

`engine.rs::run_repetition_range` evaluates bounded `{lb,ub}` in a single pass: the inner pattern runs once, the `first → indices` hash is built once, and every level `1..=ub` grows in a single `rows` buffer reusing the previous level's slice by index. Levels below `lb` get drained at the end via one `Vec::drain`. Replaces an earlier per-length loop that scaled as `O((ub-lb+1) × ub)` with `O(ub)`.

**Unbounded repetition** (`*`, `+`, `{n,}` with no upper bound) is infinite under plain `WALK ALL`, so the typechecker rejects it unless a §16.6 prefix makes it finite (see *Path-pattern prefixes* below). The `Repeat` arm of `run_path_pattern` dispatches on `Runtime::unbounded_policy` (a `Cell` set while evaluating a `Selected` operand): `Shortest { count, groups }` routes to `run_repetition_shortest`, `Mode(mode)` to `run_repetition_unbounded_mode`, and `Forbidden` panics (an invariant the typechecker guarantees, so it is only reachable via `compile_query_unchecked`). **The inner pattern must contribute ≥1 edge per application**: an empty-matching inner (e.g. a bare `(x)` node) under unbounded repetition is a hard typecheck error, since a zero-length lap never advances the length-ordered search and would loop forever (the bounded case stays a warning, since it terminates regardless).

### Path-pattern prefixes (ISO §16.6): modes, search, unbounded repetition

`PathPattern::Selected { prefix, pattern }` is evaluated in `engine.rs::run_path_pattern` and `src/runtime/path_select.rs`. The inner pattern runs with no LIMIT (selection ranks the full candidate set), then `apply_path_prefix` filters by mode and reduces by search, and any caller LIMIT is applied afterward. Selection partitions rows by the `(first node id, last node id)` boundary key and acts per partition:

- **Mode filter** (`path_satisfies_mode`): drops rows whose path repeats an edge (TRAIL) or node (ACYCLIC; SIMPLE allows only a closing first==last).
- **`ANY N`** (`select_any`): keep up to `N` rows per partition in production order.
- **`SHORTEST N [PATHS]`** (`select_shortest_paths`): the `N` shortest rows per partition, stable-sorted by edge length (ties broken by production order).
- **`SHORTEST N GROUPS`** (`select_shortest_groups`): every row whose length is among the `N` shortest distinct lengths in its partition.

For **bounded** patterns, `apply_path_prefix` materializes all rows then selects. For **unbounded** repetition, a dedicated finite search avoids materializing the infinite walk set:

- `run_repetition_shortest` (WALK + SHORTEST) is a length-ordered k-shortest **walk** search: a `BinaryHeap` (min-heap on path length, monotone `seq` tie-break) expands paths in non-decreasing length, with a per-`(first,last)` budget that admits ≤`count` paths (PATHS) or ≤`count` distinct lengths (GROUPS) and prunes the rest. It terminates on cycles because per-pair lengths strictly grow, and pruning is sound by optimal substructure (the prefix of a k-shortest walk to a node is itself among the k-shortest walks to its predecessor — `first` is fixed along a concat chain, so the per-pair budget is the per-node k-shortest-walk budget).
- `run_repetition_unbounded_mode` (TRAIL / SIMPLE / ACYCLIC) enumerates with a worklist, pruning any partial path that already violates the mode; bounded by `|E|` (TRAIL) or `|V|` (SIMPLE/ACYCLIC). A restrictive mode takes precedence over a co-present search (`SHORTEST 2 TRAIL …*` enumerates TRAIL-valid paths, then `apply_path_prefix` reduces that finite set by `SHORTEST 2`).

**BFS fast-path for single-edge shortest** (`try_shortest_bfs`, runtime side, `engine.rs`). The walk-enumeration heap above grows like `b^d` and OOMs on social graphs, so the `Selected` arm first tries a node-dominated BFS that settles each node once (O(V+E) per source). It activates only for the canonical `(src) [single-edge]{lb,ub} (tgt)` shape under WALK + `SHORTEST 1 PATHS|GROUPS` (`ANY`/`ALL SHORTEST`), with `lb ≤ 1` and an unlabelled-or-single-label edge that binds no variable; any other shape returns `None` and falls back to `run_repetition_shortest`, so semantics never regress. It drives the BFS from the smaller endpoint set (reverse-walking the adjacency when driven from the target), reconstructs paths through a predecessor DAG (all minimum-length paths for `GROUPS`, one for `PATHS`), and reproduces the generic enumerator's coincident-endpoint behavior under `+`/`{1,…}` — the length-0 self path for `*`, and the trivial undirected closed walk (`n-e-m-e-n`, ub-gated) under a non-zero lower bound; a directed coincident pair defers to the generic path. `GQLITE_DISABLE_SHORTEST_BFS=1` forces it off (the `shortest_bfs_test` differential suite asserts BFS ≡ generic). Collapses LDBC IC1/IC13 from tens of seconds to ~20–30 ms and unblocks IC14's OOM (now ~0.7 s median, real results).

**Typechecker gates** (`src/typing/checker.rs`): `check_unbounded_repetition` rejects an unbounded repeat whose nearest enclosing prefix does not license it (`PathPrefix::unbounded_support`), and rejects `SHORTEST` over `{n,}` with `n ≥ 2` (only `*`/`+` are supported; use a restrictive mode for higher lower bounds). `check_selective_isolation` enforces ISO §16.6 SR 5–8: a selective pattern (any non-`ALL` search) may share only its boundary (endpoint) variables with the rest of the query, so it stays evaluable in isolation; sharing an interior variable is an error. A `Selected` wrapper does not change variable types (`check_path_pattern` recurses through it).

### EXISTS / NOT EXISTS

`Expr::Exists { body }` and `Expr::NotExists { body }` are two distinct AST variants (the optimiser folds each to a different constant). Body is a `Box<Query>` accepting `MATCH`+`WHERE` clauses (one or more, including `OPTIONAL MATCH`); `RETURN`, `GROUP BY`, `LIMIT`, and `DISTINCT` are rejected by the parser since the body's purpose is proving non-emptiness, not projecting.

**Scoping** (typechecker `check_subquery_body`): the body is checked under a clone of the outer environment so outer-bound variables resolve via correlation, then the inner environment is discarded. References to inner-only vars from `RETURN` / outer `WHERE` produce the existing "variable not found" error. Both predicates type as `SimpleType::B`.

**Optimisation** (`src/optimizer/existential.rs`): runs after the per-pattern pushdown passes. Walks every `Expr` reachable from `Query` (WHERE filters, GROUP BY, RETURN, recursive into nested existentials), runs the typechecker on each body against the active schema, and rewrites empty bodies to literals — `false` for `Exists`, `true` for `NotExists`. Catches shape-driven emptiness (a label or property the schema rejects). The pass does not thread outer-scope correlation into the body, so refinement-aware emptiness is left for a future pass; no literal-Boolean propagation either, so an inner-only fold inside an outer body does not collapse the outer.

**Runtime** (`engine.rs::eval_exists`): three regimes share `Runtime::exists_cache: RefCell<HashMap<usize, ExistsCache>>` keyed by the body's heap address.
- *Uncorrelated* (no shared variable with outer μ): `run_match_chain(body, limit=1)` once, cache the bool; subsequent rows reuse it.
- *Correlated — pinned* (`ExistsCache::CorrelatedPinned`, the default when the body is LTJ-pinnable): per outer row, collapse the body to one pattern and run it with the correlation variables pinned to that row's node ids via `try_ltj_with_pins` (`pinned_run_multi`, which unwraps the `Filter` carrying the body's WHERE so the predicate still runs), then memoise the non-emptiness verdict by correlation tuple. The body is evaluated once per *distinct correlation tuple*, never over the whole graph. This is the LDBC IC4 / IC10 anti-join shape (one person → a handful of friends): collapses the per-param IC4 cost from a fixed full-graph materialisation (~3 s on a slow host, the old floor) to a few targeted probes. `exists_body_pinned` returns `None` (→ fall back to materialise-once) when a correlation value is not a `Node`, the body is OPTIONAL/`Selected`/non-decomposable, **or the body contains an undirected edge** (`PathPattern::has_undirected_edge` — pinning both endpoints of a `~[...]~` does not constrain the LTJ to that specific pair, so it would yield false positives; the undirected `~[:knows]~` in IC7's `isNew` takes this fallback). Force off with `GQLITE_DISABLE_EXISTS_PIN=1`.
- *Correlated — materialise-once* (`ExistsCache::Correlated`, fallback): `build_correlated_set` runs `run_match_chain(body, 0)` once (full body), projects every row onto the correlation set (sorted variable names), stores a `HashSet<Vec<PathValue>>`. Per outer row, build the probe key from μ and check membership — semi-join for `EXISTS`, anti-join for `NOT EXISTS`. Wins when the outer side binds many distinct correlation tuples (amortises the one scan); the pinned regime wins when it binds few.

The four phases live in commits `4d13327` (parse + typecheck), `163ee30` (fold optimiser), `134890f` (uncorrelated runtime), `d5b4a45` (correlated runtime). Tests in `tests/parser_test.rs`, `tests/typecheck_test.rs`, `tests/exists_fold_test.rs`, `tests/exists_runtime_test.rs`. Formal type rules in `latex/extension/main.tex` (`\textsc{TExists}`, `\textsc{TNotExists}`, plus `\isEmpty(\matchseq) \equiv e \lor \mathsf{empty}(\Gamma')` and the rewrite rules `\textsc{ExistsEmpty}` / `\textsc{NotExistsEmpty}`).

### Aggregates and RETURN arithmetic

`RETURN` items are `ReturnItem::Expr { expr, alias }` or `ReturnItem::Aggregate { agg, alias }`. Aggregates also compose **inside** value expressions via `Expr::Agg(Box<Aggregator>)`, so arithmetic over aggregate results works: `COUNT(DISTINCT x) + COUNT(DISTINCT y) AS total`. The parser recognises an aggregate call in `primary_expr` (so it can be a `Binop` operand); a *bare* top-level aggregate is re-folded back into `ReturnItem::Aggregate` so the existing aggregate-projection and ORDER BY-matching paths stay unchanged. `Expr::contains_agg()` classifies a RETURN expr as reduced-over-group (not a grouping key).

Runtime (`engine.rs::run_aggregated`): an aggregate-bearing RETURN expr is evaluated per group by `fold_aggs` — each `Expr::Agg` node is reduced over the group to an `Expr::Const`, then the residual arithmetic runs against the group's representative row (so non-aggregate leaves like `otherPerson.firstName` resolve to their grouped value). The typechecker types `Expr::Agg` as the reducer's result (`COUNT`→Int, `AVG`→Float, `SUM`→numeric, `MIN`/`MAX`→element type) and exempts aggregate-bearing exprs from the GROUP BY key check. This unblocked LDBC IC3 (`COUNT(DISTINCT messageX) + COUNT(DISTINCT messageY) AS totalCount`). Tests in `tests/count_test.rs`.

### Value subqueries, RECORD, scalar builtins, GROUP BY by variable (IC7 cluster)

Six value-expression primitives added together to unblock LDBC IC7 (`bench/ldbc-queries/ic7.toml`, `tests/ic7_test.rs`):

- **Division `/`** — ISO `<solidus>`; `Token::Slash`, `BinOp::Div`, lexed past the `/* */` comment (consumed earlier in `skip_whitespace`). Runtime `eval_binop`: Int/Int truncates, mixed/Float widen, div-by-zero → `Failure` → Null (3VL).
- **`FLOOR` / `CAST`** — a generic `Expr::Call { name, args }` with dispatch in `engine.rs::eval_call`. `FLOOR(x)→Float`. `CAST(x AS INTEGER|FLOAT)` *converts* the value (the target rides as `Expr::Type` in `args[1]`); distinct from `BinOp::As`, which only type-*asserts*. Both are soft keywords (only before `(`). Parser parses the CAST operand with `return_comparison()` so the `AS` separator is not swallowed. This is the home for future builtins (CEIL/ABS/SIZE/…); the path functions (`ELEMENTS`/`PATH_LENGTH`/`CARDINALITY`) also live in this dispatch — see *Named paths and path functions*.
- **`RECORD { k: <expr>, ... }`** — `Expr::Record { fields: Vec<(String, Expr)> }` with expression values. `RECORD` keyword optional (soft, before `{`); shares the brace parser with the bare `{k: v}` literal via `parse_brace_record`, keeping the `:`-lookahead value-vs-type split and a const fast-path (all-`Const` fields fold to `Value::Record`). Types as `SimpleType::Record`; `FieldAccess` already resolves fields.
- **`VALUE { MATCH ... RETURN <1 item> ORDER BY ... LIMIT 1 }`** — `Expr::ValueSubquery { body: Box<Query> }`, a correlated scalar/record subquery (arg-max per group). Parser `parse_value_body` (allows RETURN/ORDER BY/LIMIT, rejects GROUP BY/DISTINCT, exactly one item). Typecheck reuses `check_subquery_body` then types the single RETURN item. Runtime `eval_value_subquery` has two regimes, same split as correlated EXISTS (`ValueSubqueryCache { map, pinned }`, keyed by body heap ptr, cleared per top-level `run_query`): **pinned** (default when pinnable, `value_subquery_pinned`) runs the body per outer row with the correlation vars pinned to that row's node ids (`pinned_run_multi`), projects + ORDER BY + LIMIT 1, and memoises the value by correlation tuple — body evaluated once per *distinct tuple*, never globally; **materialise-once** (fallback) runs the body once, buckets all rows by correlation key, projects each bucket. Pinned bails (→ materialise-once) on the same conditions as `exists_body_pinned` (non-Node correlation value, OPTIONAL/`Selected`, non-decomposable, undirected edge) plus a parameter correlation or a grouping/aggregate body (those need `run_aggregated`, not the plain arg-max projection). Force off with `GQLITE_DISABLE_VALUE_SUBQUERY_PIN=1`. This is IC7's dominant cost: its `VALUE { (person)<-[:hasCreator]-(m)<-[l:likes]-(liker) ... LIMIT 1 }` body, run uncorrelated, materialised the whole graph's like relation (109 k triples / param); pinned to `(person, liker)` it touches a handful — IC7 1.3 s → ~9 ms.
- **`GROUP BY <binding variable>`** — ISO `<grouping element> ::= <binding variable reference>`, so `GROUP BY liker, person` groups by node identity. `run_query` routes a GROUP-BY-without-aggregates query through `run_aggregated` (`needs_grouping = has_aggs || group_by.is_some()`). The typechecker's functional-dependency check (`check_returns_match_group_by`) accepts a non-aggregate projection when it structurally equals a key OR references only bare-`Expr::Var` grouping keys; subquery/aggregate-bearing items are exempt (evaluated on the group's representative row). **Grouping by a property** (`GROUP BY x.city`) only licenses the structural-match shape, not sibling attributes.
- **`ORDER BY <alias>.<field>`** — `SortKey::ColumnField { col, path }` (post-projection): walks a record-valued projected column. Needed because IC7 orders by `latestLike.likeCreationDate` where `latestLike` is a VALUE-subquery record column.

Latent fix made here: `eq_value` (the `GroupKey` equality backing grouping/DISTINCT) had no `Value::Node`/`Edge`/`Null` arms, so two equal nodes compared unequal — grouping by a node variable never collapsed. Now compares reference values by id. Elaboration recurses into subquery bodies (`elaborate_expr` over RETURN / ORDER BY / Filter exprs) so a subquery's own descriptor `value_filters` lower. Tests: `tests/{division,builtin_floor_cast,record_expr,group_by_var,value_subquery,ic7}_test.rs`. Remaining LDBC IC blockers + roadmap in `docs/internals/iso-gql-gaps.md`.

### Named paths and path functions (ISO §16.6 + §20.16)

`MATCH p = (a)-[:knows]->(b)` binds a whole comma operand's matched path to a path variable, materialized as a `Value::Path` for the §20.16 path functions. Unblocks the shared prerequisite of LDBC IC1 / IC13 / IC14 (each still has one further gap: `COLLECT_LIST`, `CASE WHEN`, list comprehension).

- **AST**: `PathPattern::Named { var, pattern }` wraps one operand, *outside* any §16.6 prefix (`Named { var, Selected { prefix, pattern } }`). Parsed in `path_pattern_operand` by an ISO `<path variable declaration>` lookahead (`Name` `=` at the operand start — a comparison `=` never begins an operand). The wrapper is transparent everywhere it is walked (elaborate, optimizer pushdown, the typechecker's isolation/unbounded/var-count passes); it never appears below a Concat, so LTJ on a single operand still fires and only a comma-joined Named operand falls back to hash-join.
- **Value model**: `PathValue::Path(Vec<PathValue>)` (binding-table form, distinct from `Group` so it projects to `Value::Path`, not `Value::List`) and `Value::Path(Vec<Value>)` (projected form, alternating node/edge reference values in match order). Both wired into `path_value_to_value`, `hash_value`, `eq_value`. A path is never a property value — `value_to_prop` (store) errors on it.
- **Runtime** (`engine.rs::run_path_pattern` `Named` arm): evaluate the inner operand, then bind `var → PathValue::Path(row.path().0.clone())` per row. The path is captured from the `ResultRow.paths` the runtime already builds (including the LTJ and SHORTEST paths), not recomputed.
- **Path functions** (`eval_call`): `ELEMENTS` (all elements), `PATH_LENGTH` (edge count), `CARDINALITY` (node + edge count) are ISO §20.16. `NODES` / `EDGES` (node-only / edge-only projections) are **not** ISO — they are a documented translation divergence for the LDBC queries; mark them as such in the toml. All are soft keywords resolved by `path_function_name` (only `NAME(` is special, so `nodes`/`elements`/… stay usable as variables and labels).
- **Types**: terminal `SimpleType::Path` + `VariableType::Path` (only meets/subtypes itself, never refines against schema, `is_empty == false` so a path binding never empties an environment). The checker binds `var → VariableType::Path` in the `TypeEnvironment` and requires each path-function argument to type as `Path` — a provably non-path argument (e.g. a node variable) is a hard type error.

Tests in `tests/named_path_test.rs`. Full ISO context + roadmap in `docs/internals/iso-gql-gaps.md` §2.9.

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

See `docs/internals/storage-architecture.md` for the full spec.

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
| secondary index DDL replay | ~per-DDL | `secondary_index_io::read_specs` + `build_declared` per persisted entry; `0 ms` when the file has no DDL list (legacy or no `CREATE INDEX`) |
| LTJ TripleIndex (eager) | ~670 ms | `Runtime::warm_triple_index` — six sorted orderings of all triples |
| **total** | **~570 ms warm** | (from a 6.30 s baseline before the optimisation series) |

`GQLITE_TRACE_OPEN=1` prints the per-phase timings. The current dominant phases (TripleIndex, secondary index) are both cheap to write to disk and would drop to a memory-map at the cost of ~12% (TripleIndex) and ~3% (secondary index) extra `.gdb` file size — not yet implemented.

Existing `.gdb` files written before commit `34d97c0` lack the CSR adjacency root and load via the legacy per-node chains (~5 s topology phase). Re-import or re-save to upgrade in place.

### Optimizer

Pipeline (`src/optimizer/mod.rs`): `pushdown` → `unroll_repeat` per pattern; then `existential::fold_empty_existentials` and `order_by_alias::optimize` over the full Query. Detailed per-pass write-ups (rationale, before/after numbers, file lists) in `docs/internals/implemented-optimizations.md`.

Passes the runtime relies on (each one-line summary; flags + file refs are the operational bits):
- **LTJ multi-way join + concat** — see *Join strategy* above.
- **Type-predicate pushdown** (`is T` → descriptor `PropertyType`).
- **Value-predicate pushdown** (`x.attr <op> literal` for `=`, `!=`, `<`, `<=`, `>`, `>=` → `value_preds` on node descriptor; emitted as `FilterKind::NodeAttrCmp`; nodes only).
- **Selected-path boundary pushdown** (`pushdown.rs`, ISO §16.6) — into a `Selected` pattern, a mode-only prefix (`PathSearch::All`, e.g. `ACYCLIC`) admits *all* constraints, but a selective prefix (`ANY`/`SHORTEST`) admits **only boundary (endpoint) variable** constraints; interior-node predicates stay as a post-selection `Filter`. Sound because selection partitions per `(source, target)` pair: restricting endpoints before the search equals filtering after, whereas filtering an interior node before would change which paths are shortest. `Constraints::retain_vars` keeps only the boundary vars; `walk_kinds` and `merge_constraints` carry the split.
- **Index-driven constant folding** — `Eq` predicates that hit a hash index pre-bind the variable and drop it from VEO; empty hit short-circuits to zero rows. `pattern_extract::fold_indexed_constants`.
- **Range index folding** — `<` / `<=` / `>` / `>=` predicates that hit a btree precompute the matching set and replace `NodeAttrCmp` with `FilterKind::NodeInSet`. `pattern_extract::fold_range_filters`.
- **Repeat unrolling** (`unroll_repeat.rs`) — `(P){lb, ub}` → `Union(P^lb..P^ub)` for bounded ranges with single-edge inner and empty freevars; inserts anonymous `Node(None)` boundaries and distributes Union over Concat. `MAX_UNROLL = 8` (covers `{1,8}` and tighter). Fixed-length `lb == ub` short-circuits to a single flat concat with no Union envelope. `GQLITE_DISABLE_REPEAT_UNROLL=1`.
- **ORDER BY alias resolution** (`order_by_alias.rs`) — `SortKey::Column(idx)` → `SortKey::Expr(AttrLookup)` when the alias is a pure attr lookup. All-or-nothing: aggregates / `GROUP BY` / non-resolvable specs bail.
- **OPTIONAL MATCH bind-pushdown** (`engine.rs::optional_via_bind_pushdown`, runtime side) — per outer row, pin shared Node-typed vars via `try_ltj_with_pins` instead of evaluating the inner globally then left-outer-joining. SQLite-style correlated nested-loop. `GQLITE_DISABLE_OPTIONAL_PUSHDOWN=1`. Cuts LDBC IS5 ~93× on SF0.1.
- **BTree-LTJ-real top-k** (`engine.rs::try_btree_ltj_real`, runtime side) — drive the sort variable from a btree, run `pinned_run` per id (handles Union/Filter wrappers), early-exit at `LIMIT k` (single-spec) or first cohort boundary past `k` (multi-spec). For `Or`-of-leaves descriptors (`Comment|Post`), `or_candidate_labels_for_var` + `BtreeMergeCursor` k-way-merge per-label btrees in interleaved key order. The pin retains `NodeAttrCmp` / `NodeInSet` filters (pin fixes the NodeId, not the property value). `GQLITE_ORDERBY_FORCE=pdqsort|topk` forces it off.
- **DISTINCT dedup** — both projection paths use `HashSet<GroupKey>` for O(N). Non-aggregate path was O(N²) via `Vec::contains` until commit `f6f6519`. `GroupKey` (`engine.rs`) wraps `Vec<Value>` because `Value::Float(f64)` can't directly derive `Hash`.

### Secondary indexes

`LazyGraphStore` owns a `RefCell<SecondaryIndex>` (`src/store/secondary_index.rs`). On open, `build_auto_indexes_bulk` (single O(N) pass over node records) builds **hash + btree** for every `(label, prop)` whose values are unique within the label — captures LDBC's `*_id` columns and the temporal `creationDate`s. Hash and BTree coexist on the same `(label, prop)`. `IndexKey` covers `Int`, `Str`, `Bool` only (floats / lists / records / nulls not indexable).

DDL: `CREATE [HASH | BTREE] INDEX [<name>] ON :Label(prop) [USING HASH | BTREE]`, `DROP INDEX <name>`, `SHOW INDEXES` (or `.indexes`). Both prefix and suffix syntaxes work; HASH is the default kind. Re-declaring the same kind on the same `(label, prop)` is the only conflict.

GraphAccess trait methods: `lookup_node_eq(label, prop, value) -> Option<Vec<Id>>`, `lookup_node_range(label, prop, lo, hi) -> Option<Vec<Id>>`, `lookup_node_ordered(label, prop, asc) -> Option<Vec<Id>>`. `MemoryGraphStore` (in-RAM JSON backend) returns `None` from all three and falls back to scan — it has no secondary index, so queries stay correct but unaccelerated.

**Persistence (commit `2153319`).** Auto entries are memory-only (rebuilt every open, deterministic). DDL entries (`auto = false`) ARE persisted in the `.gdb` via `header.secondary_index_root` → chained `PageType::SecondaryIndex` pages → JSON-encoded `Vec<PersistedSpec>`. Save side: `save_graph_with_catalog_and_indexes_atomic` (`store/io.rs`). Load side: `LazyGraphStore::open` reads the list and replays each entry via `build_declared` after the auto-build. See `store/secondary_index_io.rs` and `docs/secondary-indexes.md`.

**Backward compat**: legacy `.gdb` files have `secondary_index_root == 0` (the slot was previously reserved + zeroed); `read_specs` reports an empty list, behaviour identical to the pre-persistence path. TODO comment in `pager/header.rs` to drop the legacy interpretation eventually.

Diagnostic env vars: `GQLITE_DEBUG_INDEXES=1` (auto-built indexes + pinned variables), `GQLITE_DISABLE_INDEX_FOLD=1` (LTJ pre-pass off, A/B), `GQLITE_DISABLE_AUTO_INDEXES=1` (skip the auto-build at open), `GQLITE_TRACE_OPEN=1` (per-phase open timings).

LDBC IC2 on `bench/data/ldbc-sf0.1.gdb` (15 params × 3 iters, lazy backend, `--limit 20`): 2417 ms (no indexes) → 1377 ms (auto hash+btree) → **8.7 ms** (TripleIndex cached + warmed at open, 276× total). Reference: GraphQLite (SQLite + Cypher) measures 32.8 ms median on the same query.

## Benchmarks

Two benches, deliberately split (full operational doc in `bench/cross-system/README.md`):

- **Internal bench** (`internal_bench` bin, `bench/INTERNAL_BENCHMARK.md`) — gqlite's own components in isolation (typechecker on/off, lazy/disk backend, RSS). Engine diagnostics, not cross-engine comparisons.
- **External / cross-system bench** (`bench/cross-system/`) — gqlite vs other graph databases on LDBC SNB Interactive Complex (IC) query latency. The headline numbers.

### Cross-system harness (`bench/cross-system/`)

`run_all.sh` orchestrates **systems on the outer loop, ICs on the inner**: per system, set up once (load full LDBC SF0.1 into its native format), run each requested IC, then exit (per-system memory reclaimed at process exit). Output lands in `results/<timestamp>/` — per-(system,IC) CSV (`query;backend;params;row;iter;result_count;elapsed_ns`), `comparison.txt`, `setup_times.txt`, `run_info.txt`.

- **IC source-of-truth** is `bench/ldbc-queries/ic<n>.toml` — the canonical GQL the gqlite path runs directly via `ldbc_bench`. Each external system translates it to its own dialect file (`<system>/ic<n>.cypher` or `.gql`); per-system deviations live in `<system>/DIVERGENCES.md`. Only ICs with `status = "implemented"` run (currently IC2,5,6,8,9,11).
- **Row-equivalence oracle**: every runner sha256-hashes its iter-0 result and emits a `ROW … hash=<hex>` stderr line. `_lib/row_hash.py` **must byte-mirror** the `canonicalize_*` functions in `src/bin/ldbc_bench.rs` so all runners produce identical hashes for the same logical rows; `compare_results.py` cross-checks them. A mismatch is a real per-system translation bug, not noise — with ORDER BY in every toml the iter-0 result is deterministic, so byte-equal blobs ⇒ byte-equal results. This is what makes a cross-system latency comparison legitimate.
- **Measurement caveat** (quote it when quoting numbers): gqlite is benched through its Rust binary (`ldbc_bench`, no Python in the path); external systems through their Python wheels (~1–2 ms FFI per call). Each is measured via its primary user-facing interface, not normalized — latency is indicative, the row-equivalence check is the apples-to-apples part.

**Adding a system**: create `bench/cross-system/<sys>/` with `setup.py` (load the full LDBC SF0.1, IC-agnostic — counts must match gqlite's 327 588 nodes / 1 477 965 edges), `run.py` (emit the CSV schema + the ROW hash via `_lib/row_hash.py`), `requirements.txt`, `ic<n>.{cypher,gql}` translations, `README.md`, `DIVERGENCES.md`; then register it in `run_all.sh` (`ALL_SYSTEMS`, `SETUP_CMD`, `SETUP_MARKER`, `RUNNER`, `REBUILD_FLAG`) and add the `backend`→system mapping in `compare_results.py`'s `_normalize`. `kuzu/` is the closest template for an embedded Python engine; `grafeo/` is the GQL-native one (and shows the dialect carve-outs: no second top-level `MATCH`, no `(:A|B)` pattern alternation, `GROUP BY` by property-expression not alias, sub-labels via `type=` filter).

Run + chart: `bench_setup` (downloads LDBC SF0.1) → `install_python_deps.sh` → `bench/cross-system/run_all.sh --only gqlite,<sys> --ics 2,5,6,8,9,11` → `python bench/cross-system/plot_results.py` (median-per-IC grouped bars, PNG + SVG). A separate synthetic micro-bench (same-data, same-query, result-verified latency on a generated social graph) lives in `bench/grafeo-vs-frogql/`. `bench/data/` and `results/` are gitignored.

## Conventions

- Labels in patterns require the `:` prefix: `-[:Transfer]->`, not `-[Transfer]->`.
- The `bench_test` integration target has pre-existing failures — exclude it from regular runs.
- `bench/data/` is gitignored (large datasets, downloaded via `cargo run --bin bench_setup`).
- Example databases in `examples/*.gdb` ARE committed (small, useful for testing).
- Property values are tagged with `VALUE_TYPE_*` constants in `store/record.rs` (Int=0, Str=1, Bool=2, Float=3, List=4, Record=5, Null=6); changing the order is a breaking on-disk format change.

## Extending the surface

- **New DML op** (e.g. `MERGE`): lexer (token), grammar (`parse_*` arm), `src/syntax/dm.rs` (variant in `DmOp`), `src/runtime/dm.rs` (`apply_*` per binding), `GraphAccessMut` (if it touches disk), test file `tests/dm_<op>_test.rs`. The MATCH chain must run through `elaborate::elaborate_query` before iteration so descriptor `value_filters` lower into WHERE.
- **New built-in expression**: `Token` + lexer arm, parser of `factor` / `call`, `Expr::*`, runtime in `engine.rs::run_expr`, typechecker in `typing/` if it returns a non-trivial type.
- **ISO syntactic sugar**: lives in `src/elaborate/`. Anything that changes *which* rows the query produces. The optimizer is reserved for performance-preserving transforms only.
- **Persisting something new in `.gdb`**: `pager/header.rs` (root `u32`), `store/io.rs::save_graph_*` (write side), `store/lazy.rs::open` (load side), `docs/internals/storage-architecture.md` (spec), and `fig:layout` in `latex/main.tex` if it ships in the paper.

## Anti-patterns

- Do **not** add an always-on dep for a bench- or REPL-only crate. Gate it behind the `bench` / `repl` feature and add `required-features = [...]` to the bin entry. The Python wheel (`frogql` on PyPI) MUST stay independent of `repl` / `bench` — never reference `rustyline`, `ureq`, `zstd`, `tar`, `sysinfo`, or `toml` from library code.
- Do **not** put semantic lowering in `src/optimizer/`. The optimizer is performance-preserving; anything that changes which rows the query produces belongs in `src/elaborate/`.
- Do **not** persist structures that rebuild cheaply at open. The TripleIndex (LTJ, ~670 ms on 1.5 M edges) and the auto-built secondary indexes (~420 ms on 327 K nodes) are deliberately memory-only; the file-size vs. open-time trade-off is in `docs/internals/storage-architecture.md`.
- Do **not** skip `cargo test` before commit even if `cargo fmt` and `cargo clippy` pass. Lexer / grammar regressions slip past linters; the `--` line-comment change that broke `-->` edge sugar across three suites is the standing precedent.
- Do **not** call `run_dm` on a raw query. The MATCH chain must go through `elaborate::elaborate_query` first, otherwise descriptor `value_filters` (`{name: 'Alice'}`) get silently ignored at runtime and the DM matches too many rows.
- Do **not** re-gitignore `node/index.js` or `node/index.d.ts`. They're auto-generated by `napi build` but committed to git (canonical napi-rs pattern). The npm publish job needs them at the checked-out SHA; the platform `.node` binaries arrive via build-job artifacts but the dispatcher JS + TS types do not. `0.2.0-rc.2` shipped a broken host package on npm because they were excluded from the tarball — only LICENSE + README + package.json reached the registry, and `require('frogql')` returned "Cannot find module".
- Do **not** add a `prepublishOnly` script to `node/package.json` or any platform sub-package. npm fires the hook from `npm publish`, which would shell out to `napi prepublish` and recursively republish every platform — bypassing the workflow's idempotency loop and tripping 409 on whichever sub-package landed first.

## Pending and roadmap

ISO/IEC 39075:2024 features and known carve-outs live in `docs/internals/iso-gql-gaps.md`. Storage-format roadmap (incremental secondary indexes under DML overlay, persisting the TripleIndex, WAL) lives in `docs/internals/storage-architecture.md`.
