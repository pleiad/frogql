# GQLite

A Rust implementation of a GQL (Graph Query Language) graph database with single-file storage inspired by SQLite.

GQLite implements ISO GQL path pattern matching over property graphs. It differs from Neo4j's Cypher: nodes and edges support **multiple labels** (combined as conjunction), properties are typed (`int`, `str`, `bool`), edges have explicit directionality (`->` or `~~`), and the query language supports pattern composition with concatenation, union, filtering, and bounded repetition.

## Architecture

```
                          ┌──────────────────────┐
                          │     compile(query)    │
                          │   public entry point  │
                          └──────────┬───────────┘
                                     │
                 ┌───────────────────┼───────────────────┐
                 │                   │                    │
                 ▼                   ▼                    ▼
          ┌─────────────┐   ┌──────────────┐   ┌─────────────────┐
          │   Parser     │   │  Optimizer   │   │    Runtime      │
          │              │──▶│              │──▶│    Engine       │
          │ GQL string   │   │ Predicate    │   │                │
          │ → AST        │   │ pushdown     │   │ Evaluates AST  │
          └─────────────┘   └──────────────┘   │ against graph   │
                                                └────────┬────────┘
                                                         │
                                              GraphAccess trait
                                                ┌────────┴────────┐
                                                │                  │
                                                ▼                  ▼
                                         ┌─────────────┐   ┌─────────────┐
                                         │    Graph     │   │ GraphStore  │
                                         │  (in-memory) │   │(file-backed)│
                                         │  from JSON   │   │   .gql      │
                                         └─────────────┘   └──────┬──────┘
                                                                   │
                                                            ┌──────┴──────┐
                                                            │   Pager     │
                                                            │ 4KB pages   │
                                                            │ free list   │
                                                            └──────┬──────┘
                                                                   │
                                                            ┌──────┴──────┐
                                                            │  File I/O   │
                                                            │ single file │
                                                            └─────────────┘
```

**Query pipeline:** `parse` → `optimize` → `execute`

1. **Parser** — Hand-written recursive descent parser. Tokenizes and parses GQL path pattern strings into an AST.
2. **Optimizer** — Compilation pass that rewrites the AST before execution. Currently implements predicate pushdown: extracts type constraints from WHERE clauses and merges them into pattern descriptors.
3. **Runtime Engine** — Evaluates the compiled AST against a graph. Uses label indexes for fast filtering and adjacency lists for efficient traversal (instead of cross-products).
4. **GraphAccess trait** — Abstraction over graph storage. The runtime is generic: same code executes queries against both in-memory and persistent backends.
5. **Graph** — In-memory backend. Loads from JSON, builds label and adjacency indexes at load time.
6. **GraphStore** — Persistent backend. Stores the graph in a single `.gql` file using a page-based format with a string table, slotted pages for records, and in-memory indexes rebuilt on open.
7. **Pager** — Manages the database file as a sequence of 4KB pages with a free list for page reuse.

## Project Structure

```
src/
├── lib.rs                     # compile(query) — public entry point
│
├── parser/                    # GQL parser (Phase 1: string → AST)
│   ├── lexer.rs               #   tokenizer (handles compound tokens like ]->, <-[, ~[)
│   └── grammar.rs             #   recursive descent parser
│
├── optimizer/                 # AST optimization (Phase 2: AST → optimized AST)
│   └── pushdown.rs            #   predicate pushdown (WHERE → descriptor constraints)
│
├── syntax/                    # AST node types
│   ├── path_pattern.rs        #   PathPattern enum (Node, Edge*, Concat, Union, Filter, Repeat)
│   ├── descriptor.rs          #   Descriptor (variable binding + type constraint)
│   └── expr.rs                #   Expr enum (constants, attribute lookup, binop, unop)
│
├── typing/                    # Lattice-based type system
│   ├── simple_type.rs         #   base types: int, str, bool, *, bottom
│   ├── label_type.rs          #   label boolean algebra: And, Or, Neg, Star
│   ├── property_type.rs       #   record types: Open {}, Closed {{}}, bottom
│   ├── descriptor_type.rs     #   label + property constraint
│   └── variable_type.rs       #   node/edge/union/list types, Schema
│
├── runtime/                   # Query execution (Phase 3: AST + graph → results)
│   ├── engine.rs              #   Runtime<G: GraphAccess> — pattern matching engine
│   ├── assignment.rs          #   variable bindings (unify, group, concat)
│   └── result.rs              #   ResultRow, IntermediateResult, ExprResult
│
├── model/                     # Graph data model
│   ├── value.rs               #   Value (Int/Str/Bool), PathValue, Path
│   ├── graph.rs               #   Graph — in-memory backend (from JSON)
│   └── graph_access.rs        #   GraphAccess trait
│
├── store/                     # Persistent storage (single-file .gql database)
│   ├── graph_store.rs         #   GraphStore — file-backed backend
│   ├── string_table.rs        #   deduplicated string interning on pages
│   └── record.rs              #   binary encode/decode for node/edge cells
│
└── pager/                     # Low-level page I/O
    ├── page.rs                #   4KB slotted pages (cell pointer array + cell data)
    ├── header.rs              #   file header (magic, version, page count, root pointers)
    └── pager.rs               #   file create/open, read/write pages, free list

tests/
├── parser_test.rs             # 31 parser AST tests
├── runtime_test.rs            # 26 hand-built AST runtime tests
├── parse_and_run_test.rs      # 41 end-to-end tests (parse → compile → run)
├── store_runtime_test.rs      # 36 tests against file-backed GraphStore
└── bench_test.rs              # benchmarks (10K nodes, 50K edges)
```

## Getting Started

### Build

```bash
cargo build --release
```

### Run Tests

```bash
cargo test                     # all 189 tests
cargo test --test parser_test  # parser only
cargo test --test bench_test --release -- --nocapture  # benchmarks
```

## Usage

### In-Memory Graph (from JSON)

```rust
use std::path::Path;
use gqlite::compile;
use gqlite::model::graph::Graph;
use gqlite::runtime::engine::Runtime;

// Load graph
let graph = Graph::from_file(Path::new("data.json")).unwrap();

// Compile query (parse + optimize)
let pattern = compile("(x: Account)-[:Transfer]->(y)").unwrap();

// Execute
let rt = Runtime::new(&graph);
let results = rt.run(&pattern);

for row in &results.rows {
    println!("{} | {}", row.path, row.assignment);
}
```

### Persistent Storage (.gql file)

```rust
use gqlite::store::graph_store::GraphStore;

// Import JSON into a .gql database file
let store = GraphStore::from_json_file(
    Path::new("mydb.gql"),
    Path::new("data.json"),
).unwrap();

// Later: reopen from disk (no JSON needed)
let store = GraphStore::open(Path::new("mydb.gql")).unwrap();

// Same query interface
let rt = Runtime::new(&store);
let results = rt.run(&compile("(x: Person)-[:Knows]->(y)").unwrap());
```

### Reading Results

```rust
use gqlite::model::value::PathValue;

for row in &results.rows {
    // The matched path (alternating nodes and edges)
    for elem in &row.path.0 {
        match elem {
            PathValue::Node(id) => print!("({id})"),
            PathValue::EdgeDirectional(id) => print!("-[{id}]->"),
            PathValue::EdgeUndirectional(id) => print!("~[{id}]~"),
            _ => {}
        }
    }
    println!();

    // Variable bindings
    if let Some(val) = row.assignment.get("x") {
        println!("  x = {val}");
    }
}
```

## Graph JSON Format

```json
{
  "nodes": [
    {
      "id": "n1",
      "labels": ["Person", "Teacher"],
      "props": { "name": "Alice", "age": 30, "active": true }
    }
  ],
  "edges": [
    {
      "id": "e1",
      "labels": ["Knows"],
      "props": { "since": 2020 },
      "endpoints": ["n1", "n2"],
      "directionality": "->"
    }
  ]
}
```

- **labels**: one or more strings (multiple labels → conjunction `A & B`)
- **props**: values must be `string`, `integer`, or `boolean`
- **directionality**: `"->"` (directed) or `"~~"` (undirected)

## Query Language

### Node Patterns

```
()                              any node
(x)                             any node, bound to variable x
(x: Account)                    node with label Account
(:Person & Teacher)             node with both labels
(x: Account {owner: str})       label + typed property
(:{active: bool})               property constraint, no variable
```

### Edge Patterns

```
->                              any directed edge
-[:Transfer]->                  directed edge with label
-[e: Transfer {amount: int}]->  with variable, label, and property
<-[:Label]-                     directed edge, reverse direction
~[:Knows]~                      undirected edge
-[:Label]-                      any direction
```

**Important:** labels require the `:` prefix — `-[:Transfer]->`, not `-[Transfer]->`.

### Concatenation

Patterns concatenate by juxtaposition. The last node of the left path must equal the first node of the right:

```
(x: Account)-[:Transfer]->(y: Account)
(x)-[]->(y)-[]->(z)
```

### Union

```
(x: Person) | (y: Company)
```

### Filters (WHERE)

```
(x WHERE x.isBlocked = true)
(x WHERE x.amount > 1000000 and x.active = true)
((x)-[y]->(z) WHERE x.owner = 'Jay' and y.amount > 100)
(x WHERE x.isBlocked is bool)         -- type test
(x WHERE x.name as str)               -- type cast
(x WHERE not x.isBlocked)             -- logical negation
(x WHERE -x.amount < 0)               -- arithmetic negation
```

| Category   | Operators                        |
|------------|----------------------------------|
| Comparison | `=`, `!=`, `<`, `>`, `<=`, `>=`  |
| Logical    | `and`, `or`, `not`               |
| Arithmetic | `+`, `-` (unary and binary)      |
| Type       | `is` (type test), `as` (cast)    |

Types: `int`, `bool`, `str`, `*` (any)

### Repetition

```
(-->){1,3}                      1 to 3 hops
(-[:Transfer]->){2,4}           chained transfers, 2 to 4 hops
(x){3}                          exactly 3
```

Only bounded repetition `{lb, ub}` is supported at runtime.

### Multiple Labels

```
(:Person & Teacher)             node with BOTH labels (conjunction)
(:Person | Company)             node with EITHER label (disjunction)
(:!Admin)                       node WITHOUT the label (negation)
```

A JSON node with `"labels": ["Person", "Teacher"]` matches `(:Person)`, `(:Teacher)`, and `(:Person & Teacher)`.

### Open vs Closed Property Types

```
(x: {name: str})                open: has name:str, may have more properties
(x: {{name: str}})              closed: has EXACTLY name:str, nothing else
```

## Optimizations

The compiler applies these automatically during `compile()`:

1. **Predicate pushdown** — Extracts `x.attr is type` constraints from WHERE conjunctions and merges them into descriptors at the binding site:
   ```
   ((x)-[y]->(z) WHERE x.a is bool and y.b is str)
   →  (x: {a: bool})-[y: {b: str}]->(z)
   ```
   Only applies to AND chains. OR expressions remain as filters.

2. **Label-indexed scanning** — When a pattern has a simple label like `(x: Account)`, uses an inverted index instead of scanning all nodes/edges.

3. **Adjacency-driven concatenation** — For patterns like `(x)-[:Transfer]->(y)`, uses adjacency lists from the last node to find connected edges, instead of computing the full cross-product.
