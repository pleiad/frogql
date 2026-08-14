# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

froGQL — a Rust graph database implementing ISO GQL path pattern matching with single-file storage. Distributed as a CLI binary (`frogql`, modelled on `sqlite3`), an embeddable library, and PyO3 Python bindings published to PyPI as `frogql`. Part of an academic research project (test names occasionally reference paper sections, e.g. `test_paper_example_s2_4`); a separate Python reference interpreter exists in a sister project but is not required to develop or use this crate.

The crate package is still named `gqlrust` for legacy reasons; the user-facing binary, Python module, and PyPI package are all `frogql`, and every environment variable is prefixed `FROGQL_`. Some internal identifiers and doc-comments still say "gqlite"; they are not part of any user surface and should be renamed opportunistically.

## Commands

```bash
# Local dev shortcuts (see `justfile`; these wrap the commands below).
# Install the runner once: `cargo install just` (or `winget install Casey.Just`).
# `just` with no args lists every recipe.
just lint        # fmt --check + cargo check + clippy -D warnings (CI parity)
just lint-fix    # rewrite formatting + apply machine-applicable clippy fixes
just fmt         # format only, no compile
just test        # full sweep: cargo test (lib + all tests/*.rs)
just repl movies.gdb [--import-csv dir/ | --no-typecheck]   # rebuild + open REPL

# Underlying commands (what the recipes wrap):
# Full test sweep — lib unit tests + every integration target. Use the
# unqualified form so the set never drifts as tests are added:
cargo test                       # everything (lib + all tests/*.rs)
cargo test --lib                 # just the in-crate unit tests (fast)
# Sweep wall clock is dominated by a first-execution cost per binary,
# not by your code. Measured on macOS (2026-08, same commit, back to back):
#   7.5 min  after relinking every target
#    55  s   with the same binaries already run once
# The tests themselves total 5.5 s of that (bench_test 3.1, the vector
# equivalence suite 0.9, everything else under 0.1 each). The gap is
# ~5-8 s per freshly linked file, spent at 0% CPU — the process is
# blocked, not computing. A byte-identical copy at a new path pays it
# again, so it is keyed on the file, not its contents.
#
# It only bites when many targets relink at once — touching store/,
# pager/ or model/ rebuilds everything. The checks serialise because
# `cargo test` runs targets one at a time, so warming them in parallel
# first collapses it (~29 s for ~200 binaries against ~30 min serial):
#   find target/debug/deps -type f -perm +111 ! -name '*.d' \
#     | xargs -P 16 -I{} sh -c '{} --list >/dev/null 2>&1'
# Do NOT re-diagnose this as "macOS rescanning" without measuring — an
# earlier version of this note claimed that and was wrong.
cargo test --test runtime_test --test typecheck_test --test parser_test

# Single test / single file
cargo test --test runtime_test test_join_star_any_label -- --exact
cargo test --test parallel_edge_test          # one integration file

# Strict clippy (run before every commit)
cargo clippy --workspace --all-targets -- -D clippy::all

# Build all binaries (defaults: `repl` + `bench` features on)
cargo build --release

# Interactive REPL
./target/release/frogql movies.gdb --import-csv path/to/csv_dir/   # create + open
./target/release/frogql movies.gdb                                 # open existing
./target/release/frogql movies.gdb --no-typecheck                  # skip typecheck for the session
./target/release/frogql movies.gdb --no-auto-indexes               # skip the secondary-index auto-build at open

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

### Diagnostic env vars

Runtime/store toggles for A/B testing and tracing (all read at query/open time; set to `1` unless noted). Each optimization has a kill switch so a differential test can pin "optimized ≡ baseline". User-facing tutorial with measured costs per mode: `docs/modes-options.md`.

| Var | Effect |
|---|---|
| `FROGQL_LTJ_COMPACT` | build the LTJ `TripleIndex` as compact CLTJ (six LOUDS succinct tries, issue #66) instead of the default six sorted arrays — ~2.9× smaller, 1.4–2.1× slower on IC latency (IC11 faster); differential suite `tests/compact_ltj_test.rs`, size/build stats via the `ltj_index_stats` bin |
| `FROGQL_DISABLE_ANYDIR_LTJ` | force the hash-join fallback for any-direction (`-[e]-`) patterns instead of the mirrored-index LTJ (`try_ltj_mixed`); checked at the *call site* (`pattern_extract::anydir_ltj_disabled`) so the mirror is never built when disabled |
| `FROGQL_DISABLE_SEEDED_REPEAT` | force the legacy global repetition path instead of the seeded adjacency traversal |
| `FROGQL_DISABLE_REPEAT_UNROLL` | keep bounded `{n,m}` repetitions as `Repeat` instead of unrolling to a Union |
| `FROGQL_DISABLE_SHORTEST_BFS` | force the generic k-shortest-walk enumerator instead of the BFS fast-path |
| `FROGQL_DISABLE_INDEX_FOLD` | skip the LTJ secondary-index constant-folding / range-folding pre-pass |
| `FROGQL_DISABLE_OPTIONAL_PUSHDOWN` | evaluate OPTIONAL MATCH globally + left-join instead of per-row bind-pushdown |
| `FROGQL_DISABLE_EXISTS_PIN` | force materialise-once for correlated EXISTS instead of pinned LTJ probes |
| `FROGQL_DISABLE_VALUE_SUBQUERY_PIN` | force materialise-once for `VALUE { … }` subqueries |
| `FROGQL_DISABLE_AUTO_INDEXES` | skip the secondary-index auto-build at open |
| `FROGQL_ORDERBY_FORCE=pdqsort\|topk` | force one ORDER BY strategy (bypass the btree-LTJ-real top-k) |
| `FROGQL_DEBUG_INDEXES` | print auto-built indexes + pinned variables |
| `FROGQL_TRACE_OPEN` | print per-phase open latency (see *Open-time performance*) |
| `FROGQL_VEC_STRATEGY=post\|pre\|interleave\|memo` | which vector-search evaluation strategy runs (default `post`). `interleave` and `memo` are the two in-LTJ algorithms; `inltj` is an accepted alias for `interleave`. See `docs/internals/vector-search.md` |
| `FROGQL_VEC_SOURCE=hnsw\|localsort\|globalsort` | where the nearest-first ranking comes from. `hnsw` and `globalsort` share the same walk and differ only in build cost + exactness; `localsort` ranks just the current visit's candidates and never re-scans. The two exact sources are what the strategies are pinned equivalent in |
| `FROGQL_VEC_LEVEL=<n>` | VEO position of the vector-search variable (`interleave` / `memo` only, clamped to just before the first lonely var) |
| `FROGQL_VEC_TAU_EPS=<f>` | relative slack on the top-k threshold cut; an approximate cursor's order is only approximately sorted |
| `FROGQL_DISABLE_VECTORS` | ignore every vector sidecar; queries see no vector attribute |
| `FROGQL_DEBUG_VEC` | print the executed vector-search arm and its counters |

### Pre-commit checklist for Rust changes (non-negotiable)

1. `just fmt` (or `just lint-fix` to also apply clippy fixes)
2. `just lint` — fmt-check + `cargo check` + clippy `-D warnings`
3. **`just test`** — run the full sweep. Skipping this has burned commits before, e.g. a `--` line-comment lexer change broke `-->` edge sugar across three test suites. fmt + clippy alone do not catch lexer/grammar regressions.
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
  - `vec_build` — offline builder for a vector-attribute sidecar (`<db>.vec.<attr>`) + its HNSW
  - `vec_bench` — post-filter vs pre-filter vs in-LTJ vector-search harness
- `python/` — the `frogql-py` crate: a `cdylib` exposing a PyO3 extension module named `frogql`. Depends on `gqlrust = { path = "..", default-features = false }` so the wheel ships only the library half (no rustyline/ureq/etc.). Built and installed with maturin (`maturin develop` for local dev, `maturin build --release` for wheels). Maturin installs into whichever venv is active.
- `node/` — the `frogql-node` crate: a `cdylib` exposing a napi-rs extension named `frogql`. Same `default-features = false` discipline as `python/`. Built and packaged via `@napi-rs/cli` (see `node/package.json` scripts). Distributed on npm as a host package (`frogql`) plus five platform sub-packages (`frogql-darwin-x64`, `frogql-darwin-arm64`, `frogql-linux-x64-gnu`, `frogql-linux-arm64-gnu`, `frogql-win32-x64-msvc`) declared in `optionalDependencies`; npm picks the right one at install time. The host's `index.js` (platform dispatcher) and `index.d.ts` (TS types) are auto-generated by `napi build` but **checked into git** — required so the publish job can ship them without rebuilding. Toolchain is **@napi-rs/cli 3.x** with Rust crates `napi = "3.10"` / `napi-derive = "3.5"` (`napi-build` stays `"2"` — no 3.x is published). The 3.x config in `package.json` uses `napi.binaryName` + `napi.targets` (the 2.x `napi.name` + `napi.triples.additional` are deprecated). The 3.x-generated loader detects musl through a guarded three-strategy chain (filesystem → `process.report` → child-process `ldd`), each falling back to `null`/glibc; this replaced the 2.x loader whose unguarded `process.report.getReport().header` threw `Cannot read properties of undefined` in embedded hosts like the VS Code extension host on Linux, aborting activation before any frogql call. **Keep `napi` at ≥3.10**: `napi-derive` 3.5's generated code references `bindgen_prelude` symbols absent from `napi` 3.8, so a looser `"3"` can resolve an incompatible runtime (the committed `Cargo.lock` pins the exact pair).
- `wasm/` — the `frogql-wasm` crate: a `cdylib` (+ `rlib` for host tests) exposing a `wasm-bindgen` module for the **browser**. Same `default-features = false` discipline. Wraps **`MemoryGraphStore`** only (no filesystem in the browser): `open_json(json)` → `Connection` with `execute(query, limit?)`, `to_json()`, `schema()`, `node_count`, `edge_count`. Read queries return row objects; INSERT/SET/DELETE work in RAM via the overlay; DDL / `CREATE INDEX` are rejected (no catalog/index in-memory). Persistence is the JSON string from `to_json()` round-tripped through `open_json` (store it in IndexedDB). Build for the browser with `wasm-pack build --target bundler` (needs `wasm-pack` + the `wasm32-unknown-unknown` target); the engine core (`query_json`/`dm_json`) has host-target unit tests runnable via `cargo test -p frogql-wasm`. See `docs/internals/wasm-browser-plan.md`.

`resolver = "2"` is required: with v1, building the python crate `--target X` (cross-compile) unifies features globally and drags in `gqlrust`'s default `repl` + `bench` features even when `default-features = false` is set on the dep. That pulled `ureq → ring`, which fails to cross-build on the manylinux2014 aarch64 container. Resolver v2 computes features per-target and isolates the wheel build. Same trick keeps the node crate's wheels clean.

Top-level dirs: `src/` (library: parser, elaborate, typing, optimizer, runtime, model, store), `tests/` (integration), `examples/*.gdb` (committed sample databases), `docs/internals/` (architecture write-ups — see `JOIN_STRATEGY_NOTES.md`, `implemented-optimizations.md`, `storage-architecture.md`, `iso-gql-gaps.md`), `bench/` (LDBC scaffolding; `bench/data/` is gitignored, downloaded via `bench_setup`).

**Python API** (`python/src/lib.rs`): `frogql.open(path)`, `frogql.import_json`, `frogql.import_csv`, and a `Connection` class (`execute(query, limit)`, `schema()`, `graph_types()`, `node_count`, `edge_count`). `execute` returns a list of dicts: `{alias: value}` rows when `RETURN` is present (unaliased projections fall back to `col0`, `col1`, …); otherwise `{var: {kind, id, labels, props}}` per pattern variable plus a `_paths` key (list per comma-join sub-pattern, each a list of node/edge dicts in match order). `Connection` is `unsendable` (not thread-safe). `frogql.open` is SQLite-style create-on-open (`LazyGraphStore::open_or_create`): a missing path yields an empty DB (DEFAULT active) ready for `INSERT` + `save()`, mirroring `sqlite3.connect`. It also eagerly warms the LTJ TripleIndex; the Arc is reused across every `execute`.

**Node API** (`node/src/lib.rs`): same module + class surface as Python, camelCased per napi-rs convention: `open(path)`, `importJson`, `importCsv`, and `Connection` with `execute(query, limit?)`, `save()`, `schema()`, `graphTypes()`, `nodeCount`, `edgeCount`. Polymorphic `execute()` returns `unknown` in TS; cast to one of the exported interfaces (`SchemaSummary`, `GraphTypeSummary`, `NodeRef`, `EdgeRef`, `DmCounters`, `DdlOk`, `IndexResult`, `IndexSummary`) per statement kind. `schema()` and `graphTypes()` return strongly-typed structs directly. `Connection` is `unsafe impl Send` — napi runtime is single-threaded per V8 isolate so Sync is not required. `open()` is SQLite-style create-on-open (missing path → empty DB) and eagerly warms the TripleIndex, same as Python.

## Dependencies and feature gating

Always-on: `serde` + `serde_json` (model serialization), `thiserror` (error types). These are the only deps the Python wheel pulls.

Optional, behind features (default on for local `cargo build`):
- `repl = ["dep:rustyline"]` — only `src/bin/frogql.rs` uses it.
- `bench = ["dep:sysinfo", "dep:toml", "dep:ureq", "dep:zstd", "dep:tar"]` — `bench_setup` (download/extract) and `ldbc_bench` (RSS reporting + TOML query specs) only.

`default = ["repl", "bench"]` so plain `cargo build`, CI, and local dev see the same dependency surface as before. The `python/Cargo.toml` opts out via `default-features = false`; combined with workspace `resolver = "2"`, the wheel build never touches `ring/ureq/zstd/tar/rustyline/sysinfo/toml`. **Do not** add a new always-on dep for a bench- or REPL-only crate; gate it behind the appropriate feature and add `required-features = [...]` to the bin entry.

Dev: `proptest` (used by `aggregates_proptest`, `lattice_proptest`, `multi_match_proptest`).

## Releases (PyPI + npm in lock-step)

One git tag fires all four registries (crates.io crate, PyPI wheel, native npm, browser WASM npm) plus the standalone CLI binaries. Pushing `v*` triggers:
- `.github/workflows/release.yml` → **owned by [`dist`](https://axodotdev.github.io/cargo-dist) (cargo-dist); do not hand-edit.** Builds the `frogql` CLI for five targets on native runners (`macos-14`, `macos-15-intel`, `ubuntu-22.04`, `ubuntu-22.04-arm`, `windows-2022` — no cross-compilation), packages `.tar.xz` / `.zip` archives, emits `frogql-installer.sh` + `frogql-installer.ps1`, and creates the GitHub Release every other artifact hangs off. Config lives in `dist-workspace.toml` (workspace-level: targets, installers, `install-path = "~/.local/bin"`) and `[package.metadata.dist]` in the root `Cargo.toml` (package-level: `default-features = false`, `features = ["repl"]`, and `binaries."*" = ["frogql"]` so the bench/dev bins stay out of the archives). Turning off `bench` here is load-bearing for the same reason the Python crate does it — it pulls `ureq → ring`. Regenerate with `dist init` (bumps `cargo-dist-version` and rewrites the workflow) after editing either config; `dist plan` reports drift, `dist build` builds the host target locally.
- `.github/workflows/release-pypi.yml` → builds wheels (Linux x86_64+aarch64, macOS x86_64+arm64, Windows x86_64; manylinux2014, abi3-py38) + sdist, uploads via `MATURIN_PYPI_TOKEN`, runs in the `pypi` GitHub Environment for required-reviewers gating. **Also** carries the `crates-io` job: publishes the root library crate (`frogql` on crates.io) via `cargo publish --locked` with `CARGO_REGISTRY_TOKEN`, runs in the `crates-io` GitHub Environment, idempotent (skips if the version is already on crates.io). The crate ships only the library half via the `include` whitelist in the root `Cargo.toml` (no `examples/*.gdb`, `tests/`, or `bench/` — those blow past crates.io's 10 MiB compressed limit).
- `.github/workflows/release-npm.yml` → 5-target build matrix (mac arm64 native, mac x64 cross-compiled from arm64, linux x64 native, linux arm64 via zig, windows x64 native), publishes the host `frogql` package plus the 5 platform sub-packages via `NPM_TOKEN`, runs in the `npm` GitHub Environment. Pre-release versions (any with a `-` like `0.2.0-rc.3`) land on dist-tag `next`; clean `v0.2.0` lands on `latest`.
- `.github/workflows/release-wasm.yml` → single platform-independent build (`wasm-pack build wasm --target web`), publishes the **`frogql-wasm`** npm package (unscoped, consumed as `import init, { open_json } from "frogql-wasm"`). Reuses the `npm` Environment + `NPM_TOKEN`. WebAssembly is portable, so there's no build matrix. Same dist-tag logic and idempotent skip-if-exists as the napi job. The `web` target (not `bundler`) is deliberate: it needs no `vite-plugin-wasm` in the consumer.

Cut a release by bumping **six files** in lock-step plus regenerating `Cargo.lock` (auto on any `cargo build`):
- `Cargo.toml` (root crate, semver — this is the version `cargo publish` ships to crates.io; the source of truth)
- `python/pyproject.toml` (PEP 440 form: `0.2.0rc3`)
- `python/Cargo.toml` (semver: `0.2.0-rc.3`)
- `node/Cargo.toml`
- `node/package.json` (host version + the 5 `optionalDependencies` versions)
- `wasm/Cargo.toml` (semver; the published `frogql-wasm` version is derived from it by wasm-pack)

Then `git tag vX.Y.Z && git push origin vX.Y.Z`. All four registries reject re-publishing, so always bump. The npm release also requires `node/index.js` + `node/index.d.ts` to be committed at the tagged SHA; regenerate them with `npm run build` inside `node/` whenever the API surface changes and commit the diff.

**npm publish quirks** to know about:
- The host's `npm publish` runs with `--ignore-scripts`. A `prepublishOnly` hook would call `napi pre-publish` (3.x; `prepublish` in 2.x) which recursively re-publishes every platform sub-package and trips 409s on re-runs.
- The platform sub-packages publish from `npm/<triple>/` dirs created at workflow time by `napi create-npm-dirs` (napi 3.x; reads `napi.targets` from `node/package.json`); `napi artifacts --output-dir artifacts --npm-dir npm` then moves the downloaded `.node` binaries into each. The aarch64-linux-gnu cell cross-compiles with `napi build … --use-napi-cross` (3.x replacement for the 2.x `--zig` flag).
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
- `LazyGraphStore` — topology (edge_src/tgt) + label index in RAM, labels/props read from disk via LRU page cache. **Element names are not resident**: node and edge external names live in their own page chain (`PageType::NameTable`, `header.names_root`) and `open` reads only its page numbers. They are ~90% of a graph's string data and no query resolves them — the only callers of `node_name` / `edge_name` are `materialize_to_graph` (behind `.save`) and the dump utilities, all full graph walks. The first such call loads the whole table in one pass and caches it; a query session never pays it. A legacy file has `names_root == 0` and keeps names in the main string table, so resolution falls back there. What *is* resident is the shared dictionary: labels, property keys, and every distinct string property value — 43.7 MiB at SF0.3 against 219.5 before the split, of which the labels and keys a match actually touches are 3 727 entries and 0.1%. (The `str_to_id` dedup map is lazy on top of that and stays empty on a read-only session; anything that interns doubles the dictionary's cost.)
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

`VariableType` has two **terminal** variants that carry no descriptor,
never refine against the schema, and stay inert under every lattice
operation: `Path` (a named path variable, §16.6) and `Scalar(SimpleType)`
(a value bound by a clause rather than by the pattern — today only the
distance of `NEAREST ... AS d`). Both exist so such a variable is
projectable and orderable like any other; adding one is ~8 small arms
across `variable_type.rs`, `path_type.rs`, `checker.rs`, `format.rs`, and
a registration in `tests/lattice_proptest.rs`. Reach for a terminal
`VariableType` before a new `SimpleType`: a new terminal *value* type
ripples through the whole lattice and is only warranted when the language
must actually distinguish it (vectors, for instance, stay plain float
lists).

### Join strategy: Leapfrog Triejoin (LTJ)

Primary strategy for joins and concatenations of directed/undirected edges. Worst-case-optimal multi-way join: each directed edge is a triple `(src, label, tgt)` indexed in six sorted orderings (`TripleIndex`: SPO, SOP, POS, PSO, OSP, OPS); LTJ binds variables one at a time by leapfrog-intersecting candidate lists across triples, no intermediate materialisation. CompactLTJ paper (Arroyuelo et al., VLDBJ 2025). Module structure: `runtime/ltj/{triple_index, compact, iterator, veo, algorithm, pattern_extract}.rs`. Full algorithm walkthrough, examples, and benchmark numbers in `docs/internals/JOIN_STRATEGY_NOTES.md`.

**Two physical representations** (issue #66), selected at index-build time; both drive the same `LtjAlgorithm` through the `LtjIterator` enum:
- **Array** (default): six fully-materialized sorted `Vec<(u32,u32,u32,u32)>`. The iterator recomputes its range from scratch per call (simple, cache-friendly).
- **Compact CLTJ** (`FROGQL_LTJ_COMPACT=1`): a port of the reference `cltj_index_spo_basic` — six LOUDS succinct tries (`compact.rs`: topology bitvector with sampled select-0 + bit-packed symbol sequence), navigated by a stateful handle-stack iterator (`CompactLtjIterator`, ported from `ltj_iterator_basic.hpp`). Property-graph divergence from the RDF reference: parallel edges collapse into one trie leaf, so the index keeps an SPO-ordered eid side table (`leaf_offsets`/`leaf_eids`) that the base case consults for ISO bag multiplicity. At SF0.1 (1.49 M triples): 47.6 MiB vs 136.6 MiB (**2.87× smaller**), build 1.13 s vs 0.92 s, IC medians 1.4–2.1× slower (succinct-navigation trade-off; IC11 runs 1.5× *faster* compact). Equivalence pinned by `tests/compact_ltj_test.rs`; per-repr size/build stats via the `ltj_index_stats` bin. The metatrie tier (root-sharing across ordering pairs) and the paper's RDF/BGP benchmark remain open in #66.

**Activation** (`run_join`, `run_concat_pattern`): kicks in automatically when the pattern decomposes into triples — chains / comma-joins of directed (`-[]->`), reverse (`<-[]-`), and undirected (`~[e]~`) edges, with or without labels. **Any-direction** (`-[e]-`) chains / comma-joins also run through LTJ — pure *and* mixed with directed / `~` edges. `try_ltj_mixed` decomposes with a per-triple `EdgeKind::AnyDir` tag and routes each iterator to the index its edge kind selects: any-direction triples query a separate mirrored index (`TripleIndex::from_graph_anydir`, every edge stored in both senses), the rest the plain index. The leapfrog intersection joins candidates across the two indexes transparently (node ids are global; both indexes assign label ids in the same edge order). `has_any_direction` gates the choice between `try_ltj` (plain only, mirror stays lazy) and `try_ltj_mixed`; `FROGQL_DISABLE_ANYDIR_LTJ=1` forces the fallback. Falls back to pairwise hash-join only for Unions and Repeats not handled by the unroll optimiser.

**Base-case validation is load-bearing.** The leapfrog only descends into
values the index holds, so a tuple reaching the base case is a real match
*by construction* — nothing downstream re-checks it. That guarantee
disappears when every variable is fixed before the search (the
secondary-index constant fold plus caller pins), because then there is no
leapfrog at all. The base case therefore rejects any tuple whose iterator
reports no edge id. Removing that check silently invents edges:
`(a)-[:follows]->(b) WHERE a.id = 0 AND b.id = 3` returned a match whether
or not the edge existed. Pinned by `tests/ltj_all_pinned_test.rs`.

**ISO bag multiplicity** (issue #71): the base case (`algorithm.rs`) emits one result row per physical edge at the bound `(s, p, o)` — parallel edges sharing `(src, label, tgt)` are distinct matches even when the edge variable is not projected, and a directed edge yields two matches under `-[e]-` (one per endpoint binding). `RETURN DISTINCT` is the way to set-dedup. Oracles: `tests/{iso_multiplicity,anydir_iso}_test.rs`. The scan/hash-join and seeded-repetition paths iterate physical edge ids and are ISO-consistent with LTJ across shapes (`tests/anydir_path_consistency_test.rs`).

**`decompose_flat_chain` accepts adjacent / trailing edges**: when two edges sit adjacent (`~[:knows]~~[:knows]~`, written by users or produced by the unroll pass) it synthesises a fresh anonymous variable as the boundary, mirroring the runtime `Concat` evaluator's `last_node_id` / `first_node_id` path-merge. Without this, two consecutive edges fell off the LTJ path entirely — 270× regression on `(person)~[:knows]~~[:knows]~(other)<-[:hasCreator]-(msg)` style chains.

**In-loop filters** (`FilterKind`): `NodeLabel`, `NodeProperty`, `NodeAttrCmp` (`=`, `!=`, `<`, `<=`, `>`, `>=`), `NodeInSet` (btree-resolved range). Placed at the VEO level where all dependencies are bound; pushed down by the optimizer from WHERE conjuncts.

**Current limits**:
1. Repetitions `{n,m}`: unrolled by `optimizer::unroll_repeat` for bounded ranges with no named inner variables and a single-edge inner of **any orientation** (directed, `~`-undirected, or any-direction `-[]-` — the last routes each unrolled arm through the mirrored index via `try_ltj_mixed`, issue #71). Shapes that can't unroll (named edge/node vars, range > `MAX_UNROLL = 8`, `lb = 0`) stay on the repetition path; when such a repetition is the right operand of a concat with a seed on the left (`(a)-[e]-{1,3}(b)` with `e` **projected**), `run_concat_pattern`'s `try_concat_with_edge_repetition` expands it level-by-level from the already-filtered left rows instead of materializing the whole graph's walk set (the issue-#57 OOM fix; `FROGQL_DISABLE_SEEDED_REPEAT=1` forces the legacy global path; differential coverage in `tests/seeded_repetition_test.rs`). The seeded traversal is ISO bag-correct — the adjacency evaluators iterate physical edge ids, so it agrees with the unrolled-LTJ path (`tests/anydir_path_consistency_test.rs`). Unbounded repetition (`*`/`+`/`{n,}`) is not an LTJ shape; it requires a §16.6 prefix and runs through the dedicated finite searches (`run_repetition_shortest` / `run_repetition_unbounded_mode`, see *Path-pattern prefixes*).
2. Any-direction edges (without tilde): pure **and mixed** directed+any-direction chains / joins are LTJ-eligible via per-triple index routing (`try_ltj_mixed`, see *Activation*), and bounded unused-edge any-direction **repetitions** unroll into mirror-LTJ arms (limit 1). All any-direction paths — mixed LTJ, the seeded adjacency traversal, and the plain adjacency/hash-join fallback — are ISO bag-consistent (`tests/anydir_path_consistency_test.rs`). Mixed LTJ is a real WCO win: `(t1)-[:USED_DEVICE]->(d)-[]-(t2)` on the fraud DB runs ~4.7× faster / ~2× less RSS than the fallback. Full status in `docs/internals/anydir-ltj-plan.md`.
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

**BFS fast-path for single-edge shortest** (`try_shortest_bfs`, runtime side, `engine.rs`). The walk-enumeration heap above grows like `b^d` and OOMs on social graphs, so the `Selected` arm first tries a node-dominated BFS that settles each node once (O(V+E) per source). It activates only for the canonical `(src) [single-edge]{lb,ub} (tgt)` shape under WALK + `SHORTEST 1 PATHS|GROUPS` (`ANY`/`ALL SHORTEST`), with `lb ≤ 1` and an unlabelled-or-single-label edge that binds no variable; any other shape returns `None` and falls back to `run_repetition_shortest`, so semantics never regress. It drives the BFS from the smaller endpoint set (reverse-walking the adjacency when driven from the target), reconstructs paths through a predecessor DAG (all minimum-length paths for `GROUPS`, one for `PATHS`), and reproduces the generic enumerator's coincident-endpoint behavior under `+`/`{1,…}` — the length-0 self path for `*`, and the trivial undirected closed walk (`n-e-m-e-n`, ub-gated) under a non-zero lower bound; a directed coincident pair defers to the generic path. `FROGQL_DISABLE_SHORTEST_BFS=1` forces it off (the `shortest_bfs_test` differential suite asserts BFS ≡ generic). Collapses LDBC IC1/IC13 from tens of seconds to ~20–30 ms and unblocks IC14's OOM (now ~0.7 s median, real results).

**Typechecker gates** (`src/typing/checker.rs`): `check_unbounded_repetition` rejects an unbounded repeat whose nearest enclosing prefix does not license it (`PathPrefix::unbounded_support`), and rejects `SHORTEST` over `{n,}` with `n ≥ 2` (only `*`/`+` are supported; use a restrictive mode for higher lower bounds). `check_selective_isolation` enforces ISO §16.6 SR 5–8: a selective pattern (any non-`ALL` search) may share only its boundary (endpoint) variables with the rest of the query, so it stays evaluable in isolation; sharing an interior variable is an error. A `Selected` wrapper does not change variable types (`check_path_pattern` recurses through it).

### EXISTS / NOT EXISTS

`Expr::Exists { body }` and `Expr::NotExists { body }` are two distinct AST variants (the optimiser folds each to a different constant). Body is a `Box<Query>` accepting `MATCH`+`WHERE` clauses (one or more, including `OPTIONAL MATCH`); `RETURN`, `GROUP BY`, `LIMIT`, and `DISTINCT` are rejected by the parser since the body's purpose is proving non-emptiness, not projecting.

**Scoping** (typechecker `check_subquery_body`): the body is checked under a clone of the outer environment so outer-bound variables resolve via correlation, then the inner environment is discarded. References to inner-only vars from `RETURN` / outer `WHERE` produce the existing "variable not found" error. Both predicates type as `SimpleType::B`.

**Optimisation** (`src/optimizer/existential.rs`): runs after the per-pattern pushdown passes. Walks every `Expr` reachable from `Query` (WHERE filters, GROUP BY, RETURN, recursive into nested existentials), runs the typechecker on each body against the active schema, and rewrites empty bodies to literals — `false` for `Exists`, `true` for `NotExists`. Catches shape-driven emptiness (a label or property the schema rejects). The pass does not thread outer-scope correlation into the body, so refinement-aware emptiness is left for a future pass; no literal-Boolean propagation either, so an inner-only fold inside an outer body does not collapse the outer.

**Runtime** (`engine.rs::eval_exists`): three regimes share `Runtime::exists_cache: RefCell<HashMap<usize, ExistsCache>>` keyed by the body's heap address.
- *Uncorrelated* (no shared variable with outer μ): `run_match_chain(body, limit=1)` once, cache the bool; subsequent rows reuse it.
- *Correlated — pinned* (`ExistsCache::CorrelatedPinned`, the default when the body is LTJ-pinnable): per outer row, collapse the body to one pattern and run it with the correlation variables pinned to that row's node ids via `try_ltj_with_pins` (`pinned_run_multi`, which unwraps the `Filter` carrying the body's WHERE so the predicate still runs), then memoise the non-emptiness verdict by correlation tuple. The body is evaluated once per *distinct correlation tuple*, never over the whole graph. This is the LDBC IC4 / IC10 anti-join shape (one person → a handful of friends): collapses the per-param IC4 cost from a fixed full-graph materialisation (~3 s on a slow host, the old floor) to a few targeted probes. `exists_body_pinned` returns `None` (→ fall back to materialise-once) when a correlation value is not a `Node`, the body is OPTIONAL/`Selected`/non-decomposable, **or the body contains an undirected edge** (`PathPattern::has_undirected_edge` — pinning both endpoints of a `~[...]~` does not constrain the LTJ to that specific pair, so it would yield false positives; the undirected `~[:knows]~` in IC7's `isNew` takes this fallback). Force off with `FROGQL_DISABLE_EXISTS_PIN=1`.
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
- **`VALUE { MATCH ... RETURN <1 item> ORDER BY ... LIMIT 1 }`** — `Expr::ValueSubquery { body: Box<Query> }`, a correlated scalar/record subquery (arg-max per group). Parser `parse_value_body` (allows RETURN/ORDER BY/LIMIT, rejects GROUP BY/DISTINCT, exactly one item). Typecheck reuses `check_subquery_body` then types the single RETURN item. Runtime `eval_value_subquery` has two regimes, same split as correlated EXISTS (`ValueSubqueryCache { map, pinned }`, keyed by body heap ptr, cleared per top-level `run_query`): **pinned** (default when pinnable, `value_subquery_pinned`) runs the body per outer row with the correlation vars pinned to that row's node ids (`pinned_run_multi`), projects + ORDER BY + LIMIT 1, and memoises the value by correlation tuple — body evaluated once per *distinct tuple*, never globally; **materialise-once** (fallback) runs the body once, buckets all rows by correlation key, projects each bucket. Pinned bails (→ materialise-once) on the same conditions as `exists_body_pinned` (non-Node correlation value, OPTIONAL/`Selected`, non-decomposable, undirected edge) plus a parameter correlation or a grouping/aggregate body (those need `run_aggregated`, not the plain arg-max projection). Force off with `FROGQL_DISABLE_VALUE_SUBQUERY_PIN=1`. This is IC7's dominant cost: its `VALUE { (person)<-[:hasCreator]-(m)<-[l:likes]-(liker) ... LIMIT 1 }` body, run uncorrelated, materialised the whole graph's like relation (109 k triples / param); pinned to `(person, liker)` it touches a handful — IC7 1.3 s → ~9 ms.
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

`Value::Null` is a first-class variant, and the 3VL *unknown* truth value **is** the null value (ISO: no separate `Unknown`). A missing property/record key on a bound element reads as `Success(Value::Null)` — FPPC rule `Ra` "ok null" — so 3VL sees a null (lenient), while a genuine type error stays a `Failure`. Explicit nulls round-trip through the on-disk format.

- **Residual `WHERE` / general expressions** (`engine.rs` `run_expr` / `eval_binop`): classic 3VL. Arithmetic / comparison / `IN` propagate null (any null operand → null); `AND`/`OR` use the SQL truth tables (`false` absorbing for `AND`, `true` for `OR`; else null); `NOT null → null`; `null AS T → null` for any (nullable-by-default) target. A genuine **type error** (non-bool in boolean position, failed non-null cast, div-by-zero) is a `Failure` and **empties the path** — dropped in `WHERE`, null cell in `RETURN` — regardless of essential/inessential position (the strict subset of ISO `UA004`; we never short-circuit-suppress an inessential error). The typechecker's `cod` override for `OR`/`AND` (result `Bool` unless *both* operands meet ⊥ against `Bool`) keeps the static emptiness judgment an under-approximation. Known ISO gap: an *essential* type error and a `NOT NULL`-site null should raise a hard data exception (`22G12`/`22G03`); froGQL empties instead (FPPC "type errors yield empty outputs") — tracked as a future *ISO data-exceptions* workstream in `docs/internals/iso-gql-gaps.md`.
- **3VL in `cmp_values`** (`runtime/mod.rs`): null on either side yields `false`, so a *pushed-down* predicate involving null is dropped from the result. Used by the LTJ filter loop (`NodeAttrCmp`) and the standard scan (`filter_node`/`filter_edge`); the residual-`WHERE` path above yields `null` (which likewise drops the row via `get_bool`), so the two agree on the keep/drop decision.
- **Aggregate null elimination** (`engine.rs` `collect_aggregate_values`): both `ExprResult::Failure` and `Success(Value::Null)` are dropped before the reducer runs. Empty aggregates emit `Value::Null`.
- **Wire format**: `PropValue::Null` carries tag byte 6 (no payload). Nested nulls inside lists / records survive the round-trip. Top-level nulls are encoded as key absence — the property is omitted from the on-disk record.
- **Surface syntax**: the lexer accepts `null` / `NULL`. The parser emits `Expr::Const(Value::Null)`. The typechecker maps the literal to `SimpleType::Star` so `WHERE x = null` does not collapse the surrounding type derivation. `IS NULL` and `IS NOT NULL` (parsed via `try_is_null` lookahead) produce an `Expr::IsNull { operand, negated }` that returns `Value::Bool` regardless of operand type; a missing attribute reads as `Value::Null` (so `IS NULL` matches it), and an unbound-variable `Failure` is likewise treated as null for the test.

### CSV loader

`csv_loader::load_from_csv_dir(path)` reads `spanner_import_config.json` to discover node/edge files. Node files are identified by NOT having SRC_ID/DST_ID columns (case-insensitive). The ID column is found by trying: `vid`, `<Label>_id` (case-insensitive), any `*_id` column, then first column. Edge labels are inferred by stripping known node type names from the config's `label` field or the filename. All column lookups are case-insensitive.

### Storage format (.gdb files)

4 KB pages, slotted-page layout for variable-length records. Header page 0 stores root pointers to string table, label indexes, adjacency index, a CSR adjacency root, and an **element-name table root** (`names_root`, bytes 104-107). Node/edge records reference strings by ID; the `user_id_str_id` field indexes the *name* table, everything else the shared string table. The split exists so `open` can skip the names — see *GraphAccess trait* above. `names_root == 0` marks a legacy file whose names sit in the shared table. Property values are tagged with `VALUE_TYPE_*` constants in `store/record.rs` (Int=0, Str=1, Bool=2, Float=3, List=4, Record=5, Null=6).

Adjacency has two on-disk representations. The current writer emits **only CSR**; the legacy format is read-only-supported for backward compat:

- **CSR (written, header `csr_adjacency_root`)** — six `Vec<u32>` page chains: `[out_offsets, out_flat, in_offsets, in_flat, und_offsets, und_flat]`. Loaded in O(N + E) total via six big sequential reads; node `n`'s edges are `flat[offsets[n]..offsets[n+1]]`. Stored in memory as three `AdjCsr { offsets: Vec<u32>, flat: Vec<u32> }` on `LazyGraphStore`. Built and written by every `save_graph` call (added commit `34d97c0`).
- **Legacy per-node chains (header `adjacency_root`)** — one page chain per node listing `(edge_id, other_node, kind)` triples (kind 0=out, 1=in, 2=und). **No longer written**: it spent one 4KB page per node, ~93% of them near-empty (1.33 GiB of 1.42 GiB at SF0.1). Dropping it took the SF0.1 `.gdb` from 1.39 GiB to 136 MiB (~10.5×), comparable to the 89 MiB source CSV. `LazyGraphStore` still reads it (rebuilding CSR via bucket-sort) for pre-CSR files; `DiskGraphStore` now builds adjacency in RAM from the edge-topology arrays it already loads, so it ignores both on-disk adjacency roots. Re-save old files to shrink them.

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

`FROGQL_TRACE_OPEN=1` prints the per-phase timings. The current dominant phases (TripleIndex, secondary index) are both cheap to write to disk and would drop to a memory-map at the cost of ~12% (TripleIndex) and ~3% (secondary index) extra `.gdb` file size — not yet implemented.

Existing `.gdb` files written before commit `34d97c0` lack the CSR adjacency root and load via the legacy per-node chains (~5 s topology phase). Re-import or re-save to upgrade in place.

### Vector search (`NEAREST`)

Non-ISO extension for "which nodes satisfy this pattern **and** are among
the k nearest to a query vector". Built to compare three evaluation
strategies, not as a product feature. Full write-up in
`docs/internals/vector-search.md`.

Surface: `NEAREST <k> [ROWS] <var>.<attr> TO <expr> [AS <distvar>]`, a
clause between the MATCH chain and RETURN (so the distance variable is in
scope for projection / GROUP BY / ORDER BY). Modelled on the SPARQL
magic-predicate idiom `?img proc:hnswIterator ("idx" ?v ?d)`. `<expr>` is
a float list or `VECTOR(<node id>, '<attr>')`. `NEAREST`/`ROWS`/`TO` are
grammar-level soft keywords; `VECTOR` follows the `ELEMENTS`/`DATE` rule.
Two k-modes: distinct bindings (default) or rows (`ROWS`).

Vectors live in **sidecar files** (`<db>.vec.<attr>`), one per attribute,
outside the `.gdb` — a node record has no extra area, so per-node vectors
would otherwise become properties that every `node_props()` call decodes.
`src/vector/` holds metric, sidecar format, `VectorSet`/`VectorStore`, the
`NnCursor` trait, and HNSW (build + an unbounded best-first cursor that
emits on an `ef` lookahead). Reached from the runtime via
`GraphAccess::vectors(attr)`, defaulted to `None` — the LTJ
pattern-extraction functions hold only `graph: &G`, so anything on
`Runtime` would be unreachable from where candidate sets are produced.

Orthogonal to the strategy is `VecSource` — where the nearest-first
ranking comes from. `Hnsw` and `GlobalSort` produce a corpus-wide
ranking and share their walk exactly (every visit re-scans from rank 0
testing membership); they differ only in build cost and exactness.
`LocalSort` ranks only the current visit's candidates, so it never looks
outside the level. Pre-filter has no per-visit set and serves `LocalSort`
as `GlobalSort`, reporting the source that actually ran.

The two axes generate **eleven runnable arms**, covering five algorithms:
post-filter (1), `interleave` (2), the global pre-sort variant of it
(3, `interleave+globalsort`), pre-filter (4), and `memo` (5). Algorithms
2 and 3 share one module and differ only in the source — which is why the
source is a first-class axis and not an index/no-index flag.

Strategies in `src/runtime/vsearch/`: `post_filter` (run then rank; the
universal fallback and, with an exact source, the recall oracle),
`pre_filter` (pin the search variable per neighbour and re-run), and
`in_ltj`, which holds **two** algorithms selected by `NnMode`. All return
`IntermediateResult` so everything downstream is identical across arms.

`interleave` and `memo` bind the same variable at the same VEO level and
differ only in **when the ranking is consulted**, which is the axis under
study — hence two named strategies rather than one with a flag. The level
is reached once per binding of everything above it. `interleave` walks
the ranking inside each visit; the visits are each sorted but their
concatenation is not, so the cut is per visit and the ranking is
re-walked. `memo` hoists it out: phase 1 collects every candidate with
**all** the prefixes reaching it (one node is reachable by many paths, so
a key holds several and phase 2 must resume every one), phase 2 walks the
ranking once and resumes only what it accepts, making the cut global.

Measured on 20 000 items (`docs/internals/vector-search.md` §Results):
off level 0 `memo` pops 4 163 neighbours against `interleave`'s 198 964,
**48× fewer, and is still 1.4× slower in wall clock** — the join
dominates, so the ranking was never the bottleneck it looked like. At
level 0 there is one visit, nothing to re-walk, and `memo`'s table is
pure overhead. Both in-LTJ arms beat post-filter and pre-filter by 10×+.
Do not delete either arm: the pair is the result.

Two invariants worth not breaking: the sidecar **fingerprint** (ids are
graph-internal and `save()` renumbers them, so a stale sidecar points at
the wrong nodes — `vectors()` also returns `None` under unsaved node
insert/delete), and the **VEO override happening before filter placement**
(reordering afterwards leaves filters reading unbound variables, which is
silently wrong). `tests/vector_strategy_equiv_test.rs` pins all four
strategies equal under the exact sources, at every VEO level.

### Optimizer

Pipeline (`src/optimizer/mod.rs`): `pushdown` → `unroll_repeat` per pattern; then `existential::fold_empty_existentials` and `order_by_alias::optimize` over the full Query. Detailed per-pass write-ups (rationale, before/after numbers, file lists) in `docs/internals/implemented-optimizations.md`.

Passes the runtime relies on (each one-line summary; flags + file refs are the operational bits):
- **LTJ multi-way join + concat** — see *Join strategy* above.
- **Type-predicate pushdown** (`is T` → descriptor `PropertyType`).
- **Value-predicate pushdown** (`x.attr <op> literal` for `=`, `!=`, `<`, `<=`, `>`, `>=` → `value_preds` on node descriptor; emitted as `FilterKind::NodeAttrCmp`; nodes only).
- **Selected-path boundary pushdown** (`pushdown.rs`, ISO §16.6) — into a `Selected` pattern, a mode-only prefix (`PathSearch::All`, e.g. `ACYCLIC`) admits *all* constraints, but a selective prefix (`ANY`/`SHORTEST`) admits **only boundary (endpoint) variable** constraints; interior-node predicates stay as a post-selection `Filter`. Sound because selection partitions per `(source, target)` pair: restricting endpoints before the search equals filtering after, whereas filtering an interior node before would change which paths are shortest. `Constraints::retain_vars` keeps only the boundary vars; `walk_kinds` and `merge_constraints` carry the split.
- **Index-driven constant folding** — `Eq` predicates that hit a hash index pre-bind the variable and drop it from VEO; empty hit short-circuits to zero rows. `pattern_extract::fold_indexed_constants`.
- **Range index folding** — `<` / `<=` / `>` / `>=` predicates that hit a btree precompute the matching set and replace `NodeAttrCmp` with `FilterKind::NodeInSet`. `pattern_extract::fold_range_filters`.
- **Repeat unrolling** (`unroll_repeat.rs`) — `(P){lb, ub}` → `Union(P^lb..P^ub)` for bounded ranges with a single-edge inner and empty freevars, of **any orientation** (directed / `~`-undirected / any-direction; any-direction arms route to the mirrored index via `try_ltj_mixed`, issue #71 — before that index existed they were excluded because they fell to the global hash-join, the issue-#57 OOM). Inserts anonymous `Node(None)` boundaries and distributes Union over Concat. `MAX_UNROLL = 8` (covers `{1,8}` and tighter). Fixed-length `lb == ub` short-circuits to a single flat concat with no Union envelope. `FROGQL_DISABLE_REPEAT_UNROLL=1`.
- **Seeded repetition traversal** (`engine.rs::try_concat_with_edge_repetition`, runtime side) — for `Concat(left, (edge){lb,ub})` where the inner is a single edge and `ub` is bounded, expands level-by-level from the filtered `left` rows via the adjacency `concat_with_*` steps, binding a named edge var as `PathValue::Group` (legacy `to_group`/`concat_group` semantics). Avoids `run_repetition_range`'s global materialization when the left side is selective. `FROGQL_DISABLE_SEEDED_REPEAT=1`; differential suite `tests/seeded_repetition_test.rs`.
- **ORDER BY alias resolution** (`order_by_alias.rs`) — `SortKey::Column(idx)` → `SortKey::Expr(AttrLookup)` when the alias is a pure attr lookup. All-or-nothing: aggregates / `GROUP BY` / non-resolvable specs bail.
- **OPTIONAL MATCH bind-pushdown** (`engine.rs::optional_via_bind_pushdown`, runtime side) — per outer row, pin shared Node-typed vars via `try_ltj_with_pins` instead of evaluating the inner globally then left-outer-joining. SQLite-style correlated nested-loop. `FROGQL_DISABLE_OPTIONAL_PUSHDOWN=1`. Cuts LDBC IS5 ~93× on SF0.1.
- **BTree-LTJ-real top-k** (`engine.rs::try_btree_ltj_real`, runtime side) — drive the sort variable from a btree, run `pinned_run` per id (handles Union/Filter wrappers), early-exit at `LIMIT k` (single-spec) or first cohort boundary past `k` (multi-spec). For `Or`-of-leaves descriptors (`Comment|Post`), `or_candidate_labels_for_var` + `BtreeMergeCursor` k-way-merge per-label btrees in interleaved key order. The pin retains `NodeAttrCmp` / `NodeInSet` filters (pin fixes the NodeId, not the property value). `FROGQL_ORDERBY_FORCE=pdqsort|topk` forces it off.
- **DISTINCT dedup** — both projection paths use `HashSet<GroupKey>` for O(N). Non-aggregate path was O(N²) via `Vec::contains` until commit `f6f6519`. `GroupKey` (`engine.rs`) wraps `Vec<Value>` because `Value::Float(f64)` can't directly derive `Hash`.

### Secondary indexes

`LazyGraphStore` owns a `RefCell<SecondaryIndex>` (`src/store/secondary_index.rs`). On open, `build_auto_indexes_bulk` (single O(N) pass over node records) builds **hash + btree** for every `(label, prop)` whose values are unique within the label — captures LDBC's `*_id` columns and the temporal `creationDate`s. Hash and BTree coexist on the same `(label, prop)`. `IndexKey` covers `Int`, `Str`, `Bool` only (floats / lists / records / nulls not indexable).

DDL: `CREATE [HASH | BTREE] INDEX [<name>] ON :Label(prop) [USING HASH | BTREE]`, `DROP INDEX <name>`, `SHOW INDEXES` (or `.indexes`). Both prefix and suffix syntaxes work; HASH is the default kind. Re-declaring the same kind on the same `(label, prop)` is the only conflict.

GraphAccess trait methods: `lookup_node_eq(label, prop, value) -> Option<Vec<Id>>`, `lookup_node_range(label, prop, lo, hi) -> Option<Vec<Id>>`, `lookup_node_ordered(label, prop, asc) -> Option<Vec<Id>>`. `MemoryGraphStore` (in-RAM JSON backend) returns `None` from all three and falls back to scan — it has no secondary index, so queries stay correct but unaccelerated.

**Persistence (commit `2153319`).** Auto entries are memory-only (rebuilt every open, deterministic). DDL entries (`auto = false`) ARE persisted in the `.gdb` via `header.secondary_index_root` → chained `PageType::SecondaryIndex` pages → JSON-encoded `Vec<PersistedSpec>`. Save side: `save_graph_with_catalog_and_indexes_atomic` (`store/io.rs`). Load side: `LazyGraphStore::open` reads the list and replays each entry via `build_declared` after the auto-build. See `store/secondary_index_io.rs` and `docs/secondary-indexes.md`.

**Backward compat**: legacy `.gdb` files have `secondary_index_root == 0` (the slot was previously reserved + zeroed); `read_specs` reports an empty list, behaviour identical to the pre-persistence path. TODO comment in `pager/header.rs` to drop the legacy interpretation eventually.

Diagnostic env vars: `FROGQL_DEBUG_INDEXES=1` (auto-built indexes + pinned variables), `FROGQL_DISABLE_INDEX_FOLD=1` (LTJ pre-pass off, A/B), `FROGQL_DISABLE_AUTO_INDEXES=1` (skip the auto-build at open), `FROGQL_TRACE_OPEN=1` (per-phase open timings).

LDBC IC2 on `bench/data/ldbc-sf0.1.gdb` (15 params × 3 iters, lazy backend, `--limit 20`): 2417 ms (no indexes) → 1377 ms (auto hash+btree) → **8.7 ms** (TripleIndex cached + warmed at open, 276× total). Reference: GraphQLite (SQLite + Cypher) measures 32.8 ms median on the same query.

## Benchmarks

Two benches, deliberately split (full operational doc in `bench/cross-system/README.md`):

- **Internal bench** (`internal_bench` bin, `bench/INTERNAL_BENCHMARK.md`) — gqlite's own components in isolation (typechecker on/off, lazy/disk backend, RSS). Engine diagnostics, not cross-engine comparisons.
- **External / cross-system bench** (`bench/cross-system/`) — gqlite vs other graph databases on LDBC SNB Interactive Complex (IC) query latency. The headline numbers.

**Rebuild the `.gdb` with the binary under measurement** (`bench_setup --rebuild --skip-download`, ~10 s) before any run whose numbers matter. A `.gdb` built by an older binary keeps its old persisted DEFAULT schema in the catalog, and a stale schema can degrade compile-time numbers with no visible error — a 2026-07 run against a stale file measured `compile_chk` at 129 ms on a case that typechecks in 41 µs against a freshly built file (~3000×). Same reason the harness scripts must be preceded by `cargo build --release` after every pull (they only compile if the binary is *missing*, not stale).

### Cross-system harness (`bench/cross-system/`)

`run_all.sh` orchestrates **systems on the outer loop, ICs on the inner**: per system, set up once (load full LDBC SF0.1 into its native format), run each requested IC, then exit (per-system memory reclaimed at process exit). Output lands in `results/<timestamp>/` — per-(system,IC) CSV (`query;backend;params;row;iter;result_count;elapsed_ns`), `comparison.txt`, `setup_times.txt`, `run_info.txt`.

- **IC source-of-truth** is `bench/ldbc-queries/ic<n>.toml` — the canonical GQL the gqlite path runs directly via `ldbc_bench`. Each external system translates it to its own dialect file (`<system>/ic<n>.cypher` or `.gql`); per-system deviations live in `<system>/DIVERGENCES.md`. Only ICs with `status = "implemented"` run (currently IC2,5,6,8,9,11).
- **Row-equivalence oracle**: every runner sha256-hashes its iter-0 result and emits a `ROW … hash=<hex>` stderr line. `_lib/row_hash.py` **must byte-mirror** the `canonicalize_*` functions in `src/bin/ldbc_bench.rs` so all runners produce identical hashes for the same logical rows; `compare_results.py` cross-checks them. A mismatch is a real per-system translation bug, not noise — with ORDER BY in every toml the iter-0 result is deterministic, so byte-equal blobs ⇒ byte-equal results. This is what makes a cross-system latency comparison legitimate.
- **Measurement caveat** (quote it when quoting numbers): gqlite is benched through its Rust binary (`ldbc_bench`, no Python in the path); external systems through their Python wheels (~1–2 ms FFI per call). Each is measured via its primary user-facing interface, not normalized — latency is indicative, the row-equivalence check is the apples-to-apples part.

**Adding a system**: create `bench/cross-system/<sys>/` with `setup.py` (load the full LDBC SF0.1, IC-agnostic — counts must match gqlite's 327 588 nodes / 1 477 965 edges), `run.py` (emit the CSV schema + the ROW hash via `_lib/row_hash.py`), `requirements.txt`, `ic<n>.{cypher,gql}` translations, `README.md`, `DIVERGENCES.md`; then register it in `run_all.sh` (`ALL_SYSTEMS`, `SETUP_CMD`, `SETUP_MARKER`, `RUNNER`, `REBUILD_FLAG`) and add the `backend`→system mapping in `compare_results.py`'s `_normalize`. `kuzu/` is the closest template for an embedded Python engine; `grafeo/` is the GQL-native one (and shows the dialect carve-outs: no second top-level `MATCH`, no `(:A|B)` pattern alternation, `GROUP BY` by property-expression not alias, sub-labels via `type=` filter).

Run + chart: `bench_setup` (downloads LDBC SF0.1) → `install_python_deps.sh` → `bench/cross-system/run_all.sh --only gqlite,<sys> --ics 2,5,6,8,9,11` → `python bench/cross-system/plot_results.py` (median-per-IC grouped bars, PNG + SVG). A separate synthetic micro-bench (same-data, same-query, result-verified latency on a generated social graph) lives in `bench/grafeo-vs-frogql/`. `bench/data/` and `results/` are gitignored.

## Conventions

- Labels in patterns require the `:` prefix: `-[:Transfer]->`, not `-[Transfer]->`.
- The `bench_test` integration target is slow (~1 min — it opens the committed example `.gdb` files and measures RSS). It currently passes; skip it during tight iteration, but `cargo test` (the full sweep) does include it.
- `bench/data/` is gitignored (large datasets, downloaded via `cargo run --bin bench_setup`).
- Example databases in `examples/*.gdb` ARE committed (small, useful for testing).
- Property values are tagged with `VALUE_TYPE_*` constants in `store/record.rs` (Int=0, Str=1, Bool=2, Float=3, List=4, Record=5, Null=6); changing the order is a breaking on-disk format change.

## Extending the surface

- **New DML op** (e.g. `MERGE`): lexer (token), grammar (`parse_*` arm), `src/syntax/dm.rs` (variant in `DmOp`), `src/runtime/dm.rs` (`apply_*` per binding), `GraphAccessMut` (if it touches disk), test file `tests/dm_<op>_test.rs`. The MATCH chain must run through `elaborate::elaborate_query` before iteration so descriptor `value_filters` lower into WHERE.
- **New built-in expression**: `Token` + lexer arm, parser of `factor` / `call`, `Expr::*`, runtime in `engine.rs::run_expr`, typechecker in `typing/` if it returns a non-trivial type.
- **New query clause** (e.g. `NEAREST`): field on `Query` in `src/syntax/query.rs`, parsed in `grammar.rs::finish_query_after_matches`, recursed in `elaborate::elaborate_query`, validated in `checker::check_query`, dispatched in `engine::run_query`. Every `Query { .. }` literal ends with `..Query::empty()` for exactly this reason — adding a field should not touch a dozen construction sites. Prefer a **grammar-level soft keyword** (`eat_keyword`, the `TRAIL`/`SHORTEST` treatment) over a lexer token, so the word stays usable as a variable, label, or property name.
- **New clause-bound scalar**: goes in `Assignment.scalars`, not a new `PathValue` variant. `PathValue` models graph elements and is matched exhaustively in a dozen places that have nothing sensible to do with a float; `scalars` is confined to `assignment.rs` plus one branch in `engine.rs`'s `Expr::Var`.
- **ISO syntactic sugar**: lives in `src/elaborate/`. Anything that changes *which* rows the query produces. The optimizer is reserved for performance-preserving transforms only.
- **Persisting something new in `.gdb`**: `pager/header.rs` (root `u32`), `store/io.rs::save_graph_*` (write side), `store/lazy.rs::open` (load side), `docs/internals/storage-architecture.md` (spec), and `fig:layout` in `latex/main.tex` if it ships in the paper.

## Anti-patterns

- Do **not** add an always-on dep for a bench- or REPL-only crate. Gate it behind the `bench` / `repl` feature and add `required-features = [...]` to the bin entry. The Python wheel (`frogql` on PyPI) MUST stay independent of `repl` / `bench` — never reference `rustyline`, `ureq`, `zstd`, `tar`, `sysinfo`, or `toml` from library code.
- Do **not** put semantic lowering in `src/optimizer/`. The optimizer is performance-preserving; anything that changes which rows the query produces belongs in `src/elaborate/`.
- Do **not** persist structures that rebuild cheaply at open. The TripleIndex (LTJ, ~670 ms on 1.5 M edges) and the auto-built secondary indexes (~420 ms on 327 K nodes) are deliberately memory-only; the file-size vs. open-time trade-off is in `docs/internals/storage-architecture.md`.
- Do **not** skip `cargo test` before commit even if `cargo fmt` and `cargo clippy` pass. Lexer / grammar regressions slip past linters; the `--` line-comment change that broke `-->` edge sugar across three suites is the standing precedent.
- Do **not** call `run_dm` on a raw query. The MATCH chain must go through `elaborate::elaborate_query` first, otherwise descriptor `value_filters` (`{name: 'Alice'}`) get silently ignored at runtime and the DM matches too many rows.
- Do **not** re-gitignore `node/index.js` or `node/index.d.ts`. They're auto-generated by `napi build` but committed to git (canonical napi-rs pattern). The npm publish job needs them at the checked-out SHA; the platform `.node` binaries arrive via build-job artifacts but the dispatcher JS + TS types do not. `0.2.0-rc.2` shipped a broken host package on npm because they were excluded from the tarball — only LICENSE + README + package.json reached the registry, and `require('frogql')` returned "Cannot find module".
- Do **not** hand-edit `.github/workflows/release.yml`. `dist` regenerates it wholesale from `dist-workspace.toml` + `[package.metadata.dist]`, and `dist plan` fails the moment the file drifts. Change the config, then run `dist init`. The PyPI/crates.io half of the release lives in `release-pypi.yml`.
- Do **not** add a `[[bin]]` that uses an optional dep without `required-features`. Auto-discovery leaves it ungated, and it builds fine locally (defaults are on) while breaking the CLI release, which builds `--no-default-features --features repl`. `internal_bench` shipped that way until the `dist` build caught it.
- Do **not** add a `prepublishOnly` script to `node/package.json` or any platform sub-package. npm fires the hook from `npm publish`, which would shell out to `napi pre-publish` (3.x; `prepublish` in 2.x) and recursively republish every platform — bypassing the workflow's idempotency loop and tripping 409 on whichever sub-package landed first.

## Pending and roadmap

ISO/IEC 39075:2024 features and known carve-outs live in `docs/internals/iso-gql-gaps.md`. Storage-format roadmap (incremental secondary indexes under DML overlay, persisting the TripleIndex, WAL) lives in `docs/internals/storage-architecture.md`.
