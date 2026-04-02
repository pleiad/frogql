# GQLite Architecture & Implementation Notes

This document captures the full design and implementation state of GQLite for context continuity.

## What This Is

A Rust implementation of a GQL (ISO Graph Query Language) graph database with single-file storage inspired by SQLite. Built as a research prototype accompanying an academic paper on GQL path pattern matching. The Python reference implementation lives in `../pygql/`.

**Repo:** `pleiad/gqlite` on GitHub.

## Architecture Overview

```
┌──────────────────────────────────────────────────────────┐
│  gqlite::compile(query)                                  │
│  Public entry point: parse → optimize → return AST       │
└──────────────┬───────────────────────────────────────────┘
               │
    ┌──────────┼──────────┐
    │          │          │
    ▼          ▼          ▼
 Parser    Optimizer   Runtime Engine
 (lexer +  (predicate  (evaluates AST against graph)
 recursive  pushdown)   generic over GraphAccess trait
 descent)              │
                       │
            ┌──────────┼──────────────┐
            │          │              │
            ▼          ▼              ▼
         Graph    LazyGraphStore  DiskGraphStore
      (in-memory)  (lazy records) (disk indexes)
            │          │              │
            │     ┌────┴────┐    ┌────┴────┐
            │     │  Pager  │    │  Pager  │
            │     │(LRU     │    │(LRU     │
            │     │ cache)  │    │ cache)  │
            │     └────┬────┘    └────┬────┘
            │          │              │
            │     File I/O       File I/O
            │    (.gql file)    (.gql file)
```

## Query Pipeline

```
"(x: Person)-[:Knows]->(y)" 
    → parse()       → PathPattern AST
    → optimize()    → rewritten AST (predicate pushdown)
    → runtime.run() → IntermediateResult { rows: Vec<ResultRow> }
```

## Module Map

### `src/lib.rs`
Entry point: `pub fn compile(query: &str) -> Result<PathPattern, String>` — parse + optimize.

### `src/parser/` — GQL Parser
- `lexer.rs` — Hand-written tokenizer. Handles compound tokens (`]->`, `<-[`, `~[`, `]~`).
- `grammar.rs` — Recursive descent parser. Precedence: union (`|`) < concat (adjacency) < quantifiers (`{n,m}`, `*`, `+`, `?`). Expressions: logical < comparison < arithmetic < unary.

### `src/optimizer/` — AST Optimization
- `pushdown.rs` — Predicate pushdown. Extracts `x.attr is type` from WHERE AND-chains, merges into descriptors. `((x)-[y]->(z) WHERE x.a is bool) → (x:{a:bool})-[y]->(z)`. Only AND conjuncts; OR stays as filter.

### `src/syntax/` — AST Types
- `path_pattern.rs` — `PathPattern` enum: Node, EdgeRight/Left/Undirected/AnyDirection, Concat, Union, Filter, Repeat, Questioned.
- `descriptor.rs` — `Descriptor`: optional variable name + `DescriptorType`.
- `expr.rs` — `Expr` enum: Const, AttrLookup, Binop, Unop, Type. `BinOp`/`UnOp` enums with `delta()` for type checking.

### `src/typing/` — Lattice-Based Type System
- `simple_type.rs` — `SimpleType`: Z (int), S (str), B (bool), Star (*), Zero (⊥), Union, List. Meet/join/subtype.
- `label_type.rs` — `LabelType`: Label, Star, Top, Empty, And, Or, Neg. Boolean algebra. `from_list()`, `is_subtype()`, `meet()`, `as_simple_label()`.
- `property_type.rs` — `PropertyType`: Open `{}` (extra attrs OK), Closed `{{}}` (exact), Zero. Meet/subtype.
- `descriptor_type.rs` — `DescriptorType` = LabelType + PropertyType.
- `variable_type.rs` — `VariableType`: Node, EdgeDirectional, EdgeNonDirectional, Union, List, Zero. `Schema` struct.

### `src/runtime/` — Query Execution
- `engine.rs` — `Runtime<G: GraphAccess>`. Key optimizations:
  - **Label-indexed scanning**: `run_node_pattern` checks for simple label, uses `nodes_with_label()` index.
  - **Adjacency-driven concat**: `run_concat_pattern` detects edge/node on right side, uses `outgoing_edges()`/`incoming_edges()` instead of cross-product.
  - **Hash-join fallback**: for complex right-side patterns, groups by first-node-id for O(n+m) instead of O(n×m).
  - **Repetition hash-join**: builds hash map on grouped results once, reuses for each iteration.
- `assignment.rs` — `Assignment`: variable→PathValue bindings. `can_unify()`, `unify()`, `fill_nones()`, `to_group()`, `concat_group()`.
- `result.rs` — `ResultRow` (path + assignment), `IntermediateResult` (vec of rows), `ExprResult`.

### `src/model/` — Graph Data Model
- `value.rs` — `Value` (Int/Str/Bool), `PathValue` (Node/EdgeDirectional/EdgeUndirectional/Nothing/List), `Path` (with `can_concat`, `concat`, `first_node_id`, `last_node_id`).
- `graph.rs` — `Graph` struct: nodes, edges_d, edges_u, labels, props, endpoints, src, tgt + indexes (label_to_nodes/edges, outgoing/incoming/undirected_adj). Constructors: `from_file()` (JSON), `from_json_str()`, `from_json_value()`, `from_raw()`, `open()` (.gql), `save()`.
- `graph_access.rs` — `GraphAccess` trait: 17 methods. Core: nodes(), edges_directed/undirected(), labels(), props(), src(), tgt(), endpoints(), is_directed(), edge_path_value(). Index-aware: nodes_with_label(), directed/undirected_edges_with_label(). Adjacency: outgoing_edges(), incoming_edges(), undirected_edges_of().

### `src/store/` — Persistent Storage
- `string_table.rs` — Deduplicated string interning on pages. `intern()` → u32, `resolve()` → &str. Multi-page with overflow. `str_to_id` HashMap is public (used by DiskGraphStore).
- `record.rs` — Binary encode/decode for node/edge cells on slotted pages. Node: user_id_sid, label_sids, props (name_sid + typed value). Edge: node record + src_iid + tgt_iid + directed flag.
- `io.rs` — `save_graph()` and `load_graph()`. Writes node/edge data pages + on-disk indexes (label index, adjacency, ID index). `load_graph()` reads pages and rebuilds Graph via `from_raw()`.
- `lazy.rs` — `LazyGraphStore`: compact indexes in memory (IDs, topology), records read on demand through page cache. Uses `RefCell<Pager>` for interior mutability. `Box::leak` for returning references to lazily-loaded data.
- `disk.rs` — `DiskGraphStore`: adds on-disk label/adjacency indexes. Reads index pages through cache. Same `Box::leak` pattern.
- `disk_index.rs` — On-disk index format: sorted page chains. Label index (label_sid → page chain of element IDs), adjacency (node_iid → page chain of triples), ID index (sorted pairs for binary search). Page chain format: 8-byte header (type, count, next_page), then fixed-size entries.

### `src/pager/` — Page-Level I/O
- `page.rs` — 4KB slotted pages. Header: type, cell_count, cell_area_start. Cell pointer array grows forward, cell data grows backward. `insert_cell()`, `cell_offset()`, `read_at()`, `free_space()`.
- `header.rs` — File header (page 0): magic `GQLDB\0`, version, page_size, page_count, free_list_head, node/edge counts, root page pointers (string_table, node_data, edge_data, label_index, adjacency, edge_label_index, node_id_index, edge_id_index).
- `pager.rs` — `Pager`: create/open database file, read/write pages through LRU cache. `allocate_page()` (reuses from LIFO free list or extends file), `free_page()`. Configurable cache size (default 2000 pages = ~8MB). Cache stats (hits/misses).

## Three Storage Modes

| Mode | Memory | Speed | Use Case |
|------|--------|-------|----------|
| `Graph` | O(graph_size) ~6 bytes/element | Fastest (HashMap lookups) | <1M elements |
| `LazyGraphStore` | O(indexes) ~2 bytes/element | ~1.8x slower (page cache reads) | 1M-10M elements |
| `DiskGraphStore` | O(indexes + disk_index_roots) | ~2-3x slower (disk index reads) | Same as Lazy currently |

**Current limitation:** Both Lazy and Disk still hold user ID strings (`Vec<String>`) and ID-to-index HashMaps in memory because `GraphAccess` trait returns `&str`. To get true O(cache_size) memory, the trait would need to use u32 internal IDs everywhere with string resolution only at result formatting time.

## On-Disk File Format (.gql)

```
Page 0: File Header
  Magic: "GQLDB\0"
  Version: 1
  Page size: 4096
  Page count, free list head
  Root pointers: string_table, node_data, edge_data,
                 label_index, adjacency, edge_label_index,
                 node_id_index, edge_id_index

Pages 1+:
  StringTable pages (type=5): length-prefixed UTF-8 strings
  NodeData pages (type=1): slotted pages with encoded node cells
  EdgeData pages (type=2): slotted pages with encoded edge cells
  LabelIndex pages (type=4): sorted page chains
  Adjacency pages (type=3): triple page chains
  Free pages (type=7): linked list via first 4 bytes
```

## Key Design Decisions

1. **Enums not trait objects** for the type system. Python uses isinstance(); Rust enums with match are the direct equivalent.

2. **GraphAccess trait** for storage abstraction. Runtime is `Runtime<G: GraphAccess>` — same query code for all backends.

3. **`Box::leak` for lazy stores**. The trait returns `&LabelType` and `&Props` but lazy stores compute these on the fly. Leaking is bounded by query result size.

4. **Python mutation side-effect in repetition**. Python's `to_group()` mutates assignments in-place, affecting the original `ir`. In Rust, we clone and use the grouped version for both `res` and the hash map. This was a subtle porting bug.

5. **Hash-join over sort-merge**. For concat and repetition, grouping by first-node-id in a HashMap gives O(n+m) expected. The hash map for repetition is built once and reused across iterations.

6. **Predicate pushdown as compilation phase**. `compile()` = parse + optimize. The optimizer rewrites the AST before the runtime ever sees it.

7. **Label syntax: `:` prefix required**. `-[:Transfer]->` not `-[Transfer]->`. Without `:`, `Transfer` is parsed as a variable name, not a label. This matches the Python Lark grammar.

## Test Coverage

189 tests across 6 test files:
- `tests/parser_test.rs` (31) — AST structure from parsed queries
- `tests/runtime_test.rs` (26) — hand-built AST execution
- `tests/parse_and_run_test.rs` (41) — end-to-end: string → compile → run
- `tests/store_runtime_test.rs` (31) — save → reopen → run
- `tests/bench_test.rs` (4) — benchmarks with memory tracking
- `src/` inline tests (56) — unit tests for all modules

## Benchmarks (10K nodes, 55K edges, release mode)

| Query | Graph | Lazy | Disk |
|-------|-------|------|------|
| Label scan: Person | 1.6ms | 4.2ms | 3.2ms |
| 1-hop traversal | 13.5ms | 28.2ms | 58.4ms |
| 2-hop chain | 24.2ms | 44.9ms | 113.7ms |
| Repeat {1,2} | 24.0ms | 35.7ms | 33.8ms |

Memory at 100K nodes / 550K edges: Graph=603 MB, Lazy=212 MB, Disk=169 MB.

## What's NOT Implemented

- **Typechecker** (Phase 3 from original plan) — schema inference + type checking. Port of `pygql/gql/typechecker.py`. Skipped as optional.
- **CLI binary** — no `main.rs`. Library only.
- **True O(cache_size) DiskGraphStore** — needs `GraphAccess` redesign to use u32 internal IDs.
- **Cost-based query planning** — choosing join order by selectivity.
- **Write-back page cache** — current is write-through.
- **Transactions / WAL** — not needed for read-heavy research workload.
- **Unbounded repetition** (`*`, `+`) at runtime — only bounded `{lb, ub}`.

## Relationship to Python Implementation

The Rust runtime produces identical results to `pygql/` for all queries. Tests were ported from:
- `pygql/test/runtime_test.py` → `tests/runtime_test.rs` + `tests/parse_and_run_test.rs`
- `pygql/test/parser_test.py` → `tests/parser_test.rs`

Test databases (`test_data/fraud.json`, `test_data/social-network.json`) are copies of `pygql/dbs/`.

## Building and Testing

```bash
cargo build --release
cargo test                                                    # all 189 tests
cargo test --test bench_test --release -- --nocapture          # benchmarks
cargo test --test bench_test bench_graph_vs_lazy --release -- --nocapture  # 3-way comparison
cargo test --test bench_test bench_memory_scaling --release -- --nocapture  # memory scaling
```
