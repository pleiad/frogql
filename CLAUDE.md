# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

GQLite — a Rust graph database implementing ISO GQL path pattern matching with single-file storage. Built around an interactive REPL (`gqlite`, modelled on `sqlite3`), an embeddable library, and PyO3 Python bindings. Part of an academic research project (test names occasionally reference paper sections, e.g. `test_paper_example_s2_4`); a separate Python reference interpreter exists in a sister project but is not required to develop or use this crate.

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
           --test aggregates_proptest --test lattice_proptest --test multi_match_proptest

# Single test
cargo test --test runtime_test test_join_star_any_label -- --exact

# Strict clippy (run before every commit)
cargo clippy --workspace --all-targets -- -D clippy::all

# Build all binaries
cargo build --release

# Interactive REPL
./target/release/gqlite movies.gdb --import-csv path/to/csv_dir/   # create + open
./target/release/gqlite movies.gdb                                 # open existing

# Python bindings (builds cdylib, installs `gqlite` into the active venv)
cd python && source <your-venv>/bin/activate && pip install maturin && maturin develop --release
# For wheels to ship to other machines:
cd python && maturin build --release   # output in target/wheels/
```

## Workspace layout

Cargo workspace with two members:
- `.` (root) — the `gqlrust` library crate + CLI binaries under `src/bin/`:
  - `gqlite` — interactive REPL with line editing (rustyline)
  - `bench_queries` — generic benchmark runner
  - `bench_setup` — downloads + extracts LDBC datasets via ureq + zstd + tar
  - `ldbc_bench` — LDBC interactive-complete benchmark driver (queries in `bench/ldbc-queries/*.toml`)
  - `typecheck_bench` — typechecker microbench
  - `convert_edgelist` — edge-list format converter
- `python/` — the `gqlite-py` crate: a `cdylib` exposing a PyO3 extension module named `gqlite`. Depends on `gqlrust` via path. Built and installed with maturin (`maturin develop` for local dev, `maturin build --release` for wheels). Maturin installs into whichever venv is active.

Other top-level directories:
- `src/` — library crate (`parser/`, `elaborate/`, `typing/`, `optimizer/`, `runtime/`, `model/`, `store/`, `lib.rs`)
- `tests/` — integration tests (one file per concern; see test list above)
- `examples/` — pre-built `.gdb` databases (movies, fraud_detection, bom, ldbc-sf01, etc.) plus matching `*_queries.json` query bundles
- `docs/` — `storage-architecture.md`, `JOIN_STRATEGY_NOTES.md`, `implemented-optimizations.md`, `iso-gql-gaps.md`, `graph-type-catalog-plan.md`, `typechecker_migration.md`, `rules.md`
- `bench/` — benchmark scaffolding: `BENCHMARK_PLAN.md`, `LDBC_BENCH_PLAN.md`, `TYPECHECKER_BENCHMARK.md`, `ldbc-queries/*.toml`, `queries/`, `scripts/`, `results/` (benchmark datasets in `bench/data/` are gitignored and downloaded via `bench_setup`)

Python API surface (`python/src/lib.rs`): `gqlite.open(path)`, `gqlite.import_json(db, json)`, `gqlite.import_csv(db, dir)`, and a `Connection` class with `execute(query, limit)`, `schema()`, `node_count`, `edge_count`. `execute` returns a list of dicts: RETURN clauses produce `{alias: value}` rows; queries without RETURN produce raw `{var: {kind, id, labels, props}}` dicts. `Connection` is `unsendable` (not thread-safe across Python threads).

## Dependencies

Runtime: `serde` + `serde_json` (model serialization), `thiserror` (error types), `rustyline` (REPL line editing). Bench-only: `sysinfo` (RSS reporting for Memory/Lazy/Disk RAM-cost comparison), `toml` (LDBC query specs), `ureq` + `zstd` + `tar` (LDBC dataset download). Dev: `proptest` (used by `aggregates_proptest`, `lattice_proptest`, `multi_match_proptest`).

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
- `Runtime::run_query(&query, limit)` — execute with RETURN projection
- `Runtime::run_with_limit(&pattern, limit)` — early termination after N results

### Graph-type catalog

`LazyGraphStore` owns a `RefCell<GraphTypeCatalog>` (`src/runtime/catalog.rs`) loaded from the page chain at `header.catalog_root` on open. The REPL and Python bindings route input via `parse_statement`, dispatch DDL through `catalog_mut()` + `save_catalog()`, and compile queries with `catalog.active_schema()`. The reserved name `DEFAULT` is auto-populated by `infer_simple_schema(&store)` (`src/typing/inference.rs`) at import time and on `USE GRAPH TYPE DEFAULT`; both `CREATE` and `DROP` of `DEFAULT` are rejected. Persistence detail lives in `docs/storage-architecture.md` §3.5.

DDL surface today: `CREATE / USE / DROP GRAPH TYPE`, plus inspection / validation:

- `SHOW GRAPH TYPES` — list all entries with active markers
- `SHOW GRAPH TYPE <name>` — pretty-print one entry (uses `typing::format::format_schema`)
- `SHOW CURRENT GRAPH TYPE` — name + content of the active entry
- `VALIDATE GRAPH TYPE <name>` — walks the data via `typing::validate::validate_against_data` and caches the verdict in `catalog.validations`

`USE` does not validate. The walk is opt-in because it is O(N + E); the typechecker still constrains queries against the active schema either way.

The REPL convenience command `schema` (no argument) is an alias for `SHOW GRAPH TYPE DEFAULT`. `schema simple` keeps the alternative grouped renderer in `print_schema_simple`.

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
- `NodeAttrCmp { var, attr, op, value }` — checks `node.attr <op> value` for `=`, `!=`, `<`, `<=`, `>`, `>=`. Pushed down by the optimizer from WHERE conjuncts (see *Optimizer*).

#### When LTJ activates

LTJ kicks in automatically in `run_join` and `run_concat_pattern` whenever the pattern is decomposable into triples:

- **Yes**: chains and comma-joins of directed (`-[]->`), reverse (`<-[]-`), and undirected (`~[e]~`) edges, with or without labels. Reverse edges are normalised at extraction time by swapping the endpoints in the emitted triple. Undirected edges are normalised at index build time: each undirected edge is stored as both `(s, p, t)` and `(t, p, s)` with the same `edge_id`, so a forward lookup catches them from either endpoint.
- **No**: any-direction edges (`-[e]-` without tilde), unions (`|`), repetitions (`{n,m}`), and WHERE clauses with non-pushable expressions.

If decomposition fails, the runtime falls back to pairwise hash-join — guarantees no regression.

#### Current limits

1. **Repetitions**: `{n,m}` is not unrolled to triples (could be done for fixed bounds; not implemented). Falls back.
2. **Any-direction edges (without tilde)**: not modelled as triples.
3. **WHERE expressions**: label and pushed value predicates run inside the loop; arbitrary WHERE (e.g. `x.age > y.age` involving multiple bound vars) post-filters.
4. **TripleIndex rebuilt per query**: no cross-query caching (could go on `Runtime` with a `RefCell`).
5. **Static VEO**: variable order is fixed before search. An adaptive VEO that re-estimates cardinalities mid-search would help queries with skewed selectivity. Note also that without secondary indexes on properties, a literal-equality push to a lonely variable is *not* a true point lookup — it still scans the variable's position before rejecting.

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

### Null semantics

`Value::Null` is a first-class variant. For a **bound** graph variable, **missing property keys** are read as `Success(Value::Null)` in `AttrLookup` (`engine.rs` `run_expr`) — FPPC-style `ok null` / ISO default-nullable missing-property behavior. Explicit nulls round-trip through the on-disk format.

- **Residual `WHERE` and general expressions** (`engine.rs` `run_expr`, `eval_binop`): SQL/GQL-style **three-valued logic** — e.g. null comparisons yield unknown (success value `Null`), `AND`/`OR`/`NOT` follow SQL truth tables, `WHERE` keeps a binding only when the condition is definite `Bool(true)`. `BinOp::As` passes `Null` through so casts do not turn missing reads back into `Failure`.
- **Pushed-down value predicates** (`cmp_values` in `runtime/mod.rs`, `check_value_preds` in `engine.rs`): null on either side yields **`false`** (not full 3VL). Used by LTJ `NodeAttrCmp` and standard `filter_node`/`filter_edge`; arbitrary residual `WHERE` uses the path above.
- **Aggregate null elimination** (`engine.rs` `collect_aggregate_values`): both `ExprResult::Failure` and `Success(Value::Null)` are dropped before the reducer runs. Empty aggregates emit `Value::Null`.
- **Wire format**: `PropValue::Null` carries tag byte 6 (no payload). Nested nulls inside lists / records survive the round-trip. Top-level nulls are encoded as key absence — the property is omitted from the on-disk record.
- **Surface syntax**: the lexer accepts `null` / `NULL`. The parser emits `Expr::Const(Value::Null)`. The typechecker maps the literal to `SimpleType::Star` so `WHERE x = null` does not collapse the surrounding type derivation. `IS NULL` / `IS NOT NULL` → `Expr::IsNull`; operand `Failure` is still treated as null for the test (unchecked queries).

**Follow-up (separate workstream):** pushed-down predicates (`cmp_values`, node scans, LTJ `NodeAttrCmp`) treat null comparisons as **false**, while residual `WHERE` uses full **3VL**. That split can change observable row sets for the same logical filter depending on optimization. Next step is to unify semantics or document proveably ISO-safe shortcuts (cf. ISO/IEC 39075:2024 subclause 5.3.2.4 observable effect).

**Not modeled yet:** `<property exists>` as a value predicate (ISO 19.13 — distinct syntax / feature). Descriptor typing covers some “must have shape” cases at match time — see `docs/iso-gql-gaps.md`.

### CSV loader

`csv_loader::load_from_csv_dir(path)` reads `spanner_import_config.json` to discover node/edge files. Node files are identified by NOT having SRC_ID/DST_ID columns (case-insensitive). The ID column is found by trying: `vid`, `<Label>_id` (case-insensitive), any `*_id` column, then first column. Edge labels are inferred by stripping known node type names from the config's `label` field or the filename. All column lookups are case-insensitive.

### Storage format (.gdb files)

4 KB pages, slotted-page layout for variable-length records. Header page 0 stores root pointers to string table, label indexes, and adjacency index. Node/edge records reference strings by string table ID. Property values are tagged with `VALUE_TYPE_*` constants in `store/record.rs` (Int=0, Str=1, Bool=2, Float=3, List=4, Record=5, Null=6). Adjacency index maps `node_id → Vec<(edge_id, other_node, kind)>` where kind is 0=outgoing, 1=incoming, 2=undirected. See `docs/storage-architecture.md` for the full spec.

### Optimizer

- **Leapfrog Triejoin**: multi-way join + concat optimisation (see above).
- **Type-predicate pushdown**: extracts `x.attr is T` from WHERE conjunctions and merges into the descriptor's property type.
- **Value-predicate pushdown**: extracts `x.attr <op> literal` (for `=`, `!=`, `<`, `<=`, `>`, `>=`) and stores it on the node descriptor's `value_preds` field. Pattern extraction emits a `FilterKind::NodeAttrCmp` per predicate; the LTJ runner evaluates it in-loop. Restricted to nodes today; edge value predicates fall through to the residual WHERE.
- **VEO selectivity-aware tiebreaker**: per-variable weights bias the binding order within each lonely / non-lonely group toward filter-narrowed candidates.
- **Label index selection**: picks the smallest indexed set for compound labels like `A & B` via `LabelType::required_labels()`.

## Key conventions

- Labels in patterns require the `:` prefix: `-[:Transfer]->`, not `-[Transfer]->`.
- Run `cargo clippy --workspace --all-targets -- -D clippy::all` before every commit.
- The `bench_test` integration target has pre-existing failures — exclude it from regular runs.
- `bench/data/` is gitignored (large datasets, downloaded via `cargo run --bin bench_setup`).
- Example databases in `examples/*.gdb` ARE committed (small, useful for testing).
- Property values are tagged with `VALUE_TYPE_*` constants in `store/record.rs` (Int=0, Str=1, Bool=2, Float=3, List=4, Record=5, Null=6); changing the order is a breaking on-disk format change.
