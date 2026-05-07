# GQL Storage Architecture

This document explains how a property graph is stored in gqlrust, from the
conceptual model down to bytes on disk, and what each storage backend keeps
in memory at query time.

## 1. The Property Graph Model

A property graph has **nodes** and **edges**. Each can have labels and properties.

Example graph (fraud detection):

```
          t3: Transfer
          amount=3500000
    ┌──────────────────────┐
    ▼                      │
  ┌────┐  t1: Transfer  ┌────┐  t2: Transfer  ┌────┐
  │ p1 │ ──────────────► │ p2 │ ──────────────► │ a2 │
  └────┘  amount=2500000 └────┘  amount=3000000 └────┘
    ▲       Account        Account                Account
    │       owner="Jay"    owner="Mike"           owner="Scott"
    │       isBlocked=F    isBlocked=T            isBlocked=F
    │
    │ t4: Transfer
    │ amount=2000000
    │
  ┌────┐  t5: Foo         ┌────┐
  │ a1 │ ──────────────►  │ d1 │
  └────┘  amount=2000000   └────┘
    Account                 Dummy & Person
    owner="Aretha"          owner="Fred"
    isBlocked=F             isDummy=T
```

Nodes: a1, a2, p1, p2, d1
Edges: t1(p1→p2), t2(p2→a2), t3(a2→a1), t4(a1→p1), t5(a1→d1)


## 2. Internal IDs

Externally, nodes and edges have string names ("a1", "t3"). Internally,
everything uses sequential **u32 IDs** for performance:

```
Nodes:                          Edges:
  ID  Name   Labels             ID  Name  Src  Tgt  Dir  Labels
  ──  ────   ──────             ──  ────  ───  ───  ───  ──────
   0  "a1"   Account             0  "t1"   2    3    →   Transfer
   1  "a2"   Account             1  "t2"   3    1    →   Transfer
   2  "p1"   Account             2  "t3"   1    0    →   Transfer
   3  "p2"   Account             3  "t4"   0    2    →   Transfer
   4  "d1"   Dummy & Person      4  "t5"   0    4    →   Foo
```

String names are only used for display/serialization. All query
execution, adjacency lookups, and joins use the u32 IDs.


## 3. The .gql File Format

The database file is divided into fixed-size **pages** of 4096 bytes each.
Page 0 is the file header. The remaining pages store data and indexes.

### 3.1 File Layout Overview

```
┌──────────────────────────────────────────────────────┐
│ Page 0: File Header                                  │
│   magic: "GQLDB\0"                                   │
│   node_count: 5                                       │
│   edge_count: 5                                       │
│   pointers to: string table, node data, edge data,   │
│                label index, adjacency index,          │
│                node ID index, edge ID index,          │
│                graph-type catalog                     │
│   active graph-type name (32-byte UTF-8)             │
├──────────────────────────────────────────────────────┤
│ Pages 1-N: String Table                              │
│   All unique strings stored once                      │
│   Each gets a sequential string ID (SID)             │
├──────────────────────────────────────────────────────┤
│ Pages N+1..M: Node Data                              │
│   One cell per node (slotted-page layout)            │
├──────────────────────────────────────────────────────┤
│ Pages M+1..K: Edge Data                              │
│   One cell per edge (slotted-page layout)            │
├──────────────────────────────────────────────────────┤
│ Pages K+1..L: Indexes                                │
│   Label index (which nodes/edges have each label)    │
│   Adjacency index (outgoing/incoming per node)       │
├──────────────────────────────────────────────────────┤
│ Pages L+1...: Graph-type catalog (chained, JSON)     │
│   Named GRAPH TYPE schemas + active pointer           │
│   See §3.5                                            │
└──────────────────────────────────────────────────────┘
```

The **header** carries `catalog_root` (bytes 60–63) and an inline
`active_type_name` (bytes 64–95, 32-byte null-padded UTF-8). Both are
zero on legacy files; readers fall back to "no catalog, no active type"
without bumping the format version.

### 3.2 String Table

All label names, property names, and string values are **deduplicated**
into a string table. Each unique string gets a u32 ID (SID).

```
String Table:
  SID  String
  ───  ──────────
    0  "a1"
    1  "Account"
    2  "owner"
    3  "Aretha"
    4  "isBlocked"
    5  "a2"
    6  "Scott"
    7  "p1"
    8  "Jay"
    9  "p2"
   10  "Mike"
   11  "d1"
   12  "Dummy"
   13  "Person"
   14  "Fred"
   15  "isDummy"
   16  "Transfer"
   17  "amount"
   18  "t1"
   19  "t2"
   20  "t3"
   21  "t4"
   22  "Foo"
   23  "t5"
```

On disk, strings are stored as length-prefixed entries packed into pages:
```
Page layout:
┌────────────────────────────────────────────┐
│ Page Header (8 bytes)                      │
│   page_type = StringTable                  │
│   entry_count = 12                         │
├────────────────────────────────────────────┤
│ [len=2]["a1"]                              │
│ [len=7]["Account"]                         │
│ [len=5]["owner"]                           │
│ [len=6]["Aretha"]                          │
│ ...                                        │
│ (continues until page is full,             │
│  then overflows to next page)              │
└────────────────────────────────────────────┘
```

### 3.3 Node & Edge Records

Each node is stored as a binary **cell** inside a slotted page.

```
Node cell format:
┌──────────────────────────────────┐
│ user_id_sid: u32    (SID of "a1")│   ← 4 bytes
│ label_count: u16    (1)          │   ← 2 bytes
│ label_sids: [u32]   [SID of     │   ← 4 bytes × label_count
│                      "Account"]  │
│ prop_count: u16     (2)          │   ← 2 bytes
│ prop 0:                          │
│   name_sid: u32 (SID of "owner") │   ← 4 bytes
│   type: u8      (1 = String)    │   ← 1 byte
│   value_sid: u32 (SID "Aretha") │   ← 4 bytes
│ prop 1:                          │
│   name_sid: u32 (SID "isBlocked")│
│   type: u8      (2 = Bool)      │
│   value: u8     (0 = false)     │   ← 1 byte
└──────────────────────────────────┘
```

Edge cells are the same as node cells plus:
```
│ src_internal_id: u32  (2)        │   ← 4 bytes (node p1 = ID 2)
│ tgt_internal_id: u32  (3)        │   ← 4 bytes (node p2 = ID 3)
│ directed: u8          (0 = yes)  │   ← 1 byte
```

Cells are packed into pages using a **slotted-page** layout. This is
needed because records have **variable sizes** — a node with 5 properties
takes more bytes than a node with 0 properties. We need to fit multiple
variable-length records into a fixed 4096-byte page and still be able
to find any record by index in O(1).

The naive approach (pack records sequentially) would require scanning
from the start to find record #N, reading each record's length to skip
over it. That's O(N) per lookup.

The slotted-page solves this by splitting the page into two regions
that grow toward each other:

- **Cell pointers** at the top: a fixed-size array of u16 offsets, one
  per record, growing downward (left to right)
- **Cell data** at the bottom: the actual variable-length records,
  growing upward (right to left)

To read cell #2: read `pointer[2]` → offset 3980 → jump there. O(1).
When the two regions meet, the page is full and a new page is allocated.

This is the standard design used in SQLite, PostgreSQL, and most
database storage engines.

```
Slotted Page (4096 bytes):
┌──────────────────────────────────────────────────────┐
│ Header: type=NodeData, cell_count=3, cell_area=3980  │  8 bytes
├──────────────────────────────────────────────────────┤
│ Cell pointers: [4080] [4040] [3980]                  │  6 bytes
│                  ↓      ↓      ↓                     │
│           grows this way →                           │
│                                                      │
│              free space                              │
│                                                      │
│                           ← grows this way           │
│ Cell 2 data (28 bytes starting at offset 3980)       │
│ Cell 1 data (40 bytes starting at offset 4040)       │
│ Cell 0 data (16 bytes starting at offset 4080)       │
└──────────────────────────────────────────────────────┘
```

### 3.4 Indexes

#### Label Index

Maps each label string to the list of node/edge IDs that have it.

```
Root page: [(SID "Account", page 15), (SID "Dummy", page 16), ...]

Page 15 (Account): [0, 1, 2, 3]     ← nodes a1, a2, p1, p2
Page 16 (Dummy):   [4]               ← node d1
```

#### Adjacency Index

Maps each node to its connected edges, with direction.

```
Root page: [(node 0, page 20), (node 1, page 21), ...]

Page 20 (node 0 = a1):
  (edge 3, node 2, outgoing)    ← t4: a1 → p1
  (edge 4, node 4, outgoing)    ← t5: a1 → d1
  (edge 2, node 1, incoming)    ← t3: a2 → a1

Page 21 (node 1 = a2):
  (edge 1, node 3, outgoing)    ← t2: a2 → ... wait, t2 is p2→a2
  ...
```

Each adjacency entry is 9 bytes: edge_id(4) + other_node(4) + kind(1).
Kind: 0=outgoing, 1=incoming, 2=undirected.

### 3.5 Graph-Type Catalog

The catalog stores named GRAPH TYPE schemas (`CREATE GRAPH TYPE` /
`USE GRAPH TYPE` / `DROP GRAPH TYPE`) plus the currently-active pointer.
It survives close/reopen, so the typechecker can honor an active type
across sessions.

**Two anchor points**: the catalog root page lives in the file header
(`catalog_root`, bytes 60–63 of page 0), and the active type name is
mirrored inline in the same header (bytes 64–95). The header copy lets
the REPL print the active type before reading the chain. The chain
itself remains the source of truth.

**Page chain**. The catalog is serialized as JSON (via `serde_json`) and
written across one or more pages of `PageType::Catalog (= 8)`. Each
page after the standard 8-byte page header carries:

```
first page:        [next_page u32 LE] [total_len u32 LE] [payload..]
continuation page:  [next_page u32 LE]                    [payload..]
```

`next_page = 0` ends the chain. `total_len` (in the first page only)
tells the reader how many payload bytes are valid, so trailing zeros in
the last page are ignored. Effective payload capacity: 4080 bytes in
the first page, 4084 bytes per continuation. Catalogs are small in
practice — single-page is the common case.

**Rewrite-and-free**. Each catalog change (CREATE/USE/DROP) calls
`save_catalog()`, which frees the entire old chain through the pager's
free list and allocates a fresh one. The new root replaces
`header.catalog_root`. Page numbers are reused; there is no leak when
the catalog stays roughly the same size.

**JSON shape** (abridged):

```json
{
  "version": 1,
  "types": {
    "DEFAULT": { "nodes": [...], "edges": [...] },
    "strict":  { "nodes": [...], "edges": [...] }
  },
  "active": "strict",
  "validations": {
    "strict": {
      "against_node_count": 171,
      "against_edge_count": 253,
      "violations": 38,
      "validated_at_unix": 1738000000
    }
  }
}
```

Each `Schema` is a list of `VariableType::Node` / `Edge` values. The
typing types — `Schema`, `VariableType`, `DescriptorType`, `LabelType`,
`PropertyType`, `SimpleType` — derive `Serialize` / `Deserialize`, so
the catalog round-trips through `serde_json` without bespoke code. Any
change to those types is automatically reflected in the on-disk shape;
add a `format_version` bump if a change becomes incompatible.

**Validation cache**. `validations` is a sidecar map keyed by graph
type name. Entries are populated by `VALIDATE GRAPH TYPE <name>` (see
`src/typing/validate.rs`), which walks every node and edge and checks
each instance against `schema.nodes` / `schema.edges` via
`VariableType::is_subtype`. The map uses `#[serde(default)]` so older
catalogs that predate validation deserialize cleanly. Cache invalidation
is automatic on `register` (re-CREATE), `drop`, and `install_default`
(refresh). Future graph mutations must clear affected entries.

**Backward compatibility**. Files written before the catalog landed
have `catalog_root = 0` and an empty `active_type_name`. Readers treat
this as "no catalog, no active type" and fall back to `Schema::star()`
(permissive). Older binaries opening a file with a populated catalog
ignore the new header bytes — the rest of the format is unchanged, so
queries still work.

**Inferred DEFAULT**. The reserved name `DEFAULT` is repopulated on
demand by `infer_simple_schema(&store)` (`src/typing/inference.rs`),
which groups nodes by sorted label sets and edges by
`(edge_labels, src_labels, tgt_labels, directed)`, intersecting
property types across instances. Optional props fall out of the
intersection and the surviving record stays open. The CSV/JSON import
pipeline runs this once after `graph.save` and persists the result, so
a freshly-imported `.gdb` opens with `DEFAULT` already active.

## 4. Storage Backends

gqlrust has three backends. All implement the same `GraphAccess` trait,
so the query engine doesn't know or care which one is being used.

### 4.1 In-Memory Graph (`Graph`)

Loads everything from JSON into memory. Best for small graphs.

```
What's in memory:
┌──────────────────────────────────────────────────────────┐
│  node_names: Vec<String>     ["a1", "a2", "p1", ...]    │
│  edge_names: Vec<String>     ["t1", "t2", ...]           │
│  node_labels: Vec<LabelType> [Account, Account, ...]     │
│  edge_labels: Vec<LabelType> [Transfer, Transfer, ...]   │
│  node_props: Vec<Props>      [{owner:"Aretha",...}, ...]  │
│  edge_props: Vec<Props>      [{amount:2500000}, ...]      │
│  edge_src: Vec<u32>          [2, 3, 1, 0, 0]             │
│  edge_tgt: Vec<u32>          [3, 1, 0, 2, 4]             │
│  edge_directed: Vec<bool>    [T, T, T, T, T]             │
│                                                          │
│  ── Indexes ──                                           │
│  label_to_nodes: HashMap     {"Account"→[0,1,2,3], ...}  │
│  outgoing: Vec<Vec<u32>>     [0]→[3,4], [1]→[1], ...    │
│  incoming: Vec<Vec<u32>>     [0]→[2], [1]→[1], ...      │
└──────────────────────────────────────────────────────────┘

Memory: O(everything) — all data in RAM
Source: JSON file
Query speed: fastest (direct array access)
```

### 4.2 LazyGraphStore

Loads only compact indexes into memory. Node/edge records (labels, props)
are read on-demand from the .gql file through an LRU page cache.

```
What's in memory:
┌──────────────────────────────────────────────────────────┐
│  node_count: u32             5                           │
│  edge_count: u32             5                           │
│  edge_src: Vec<u32>          [2, 3, 1, 0, 0]             │
│  edge_tgt: Vec<u32>          [3, 1, 0, 2, 4]             │
│  edge_directed: Vec<bool>    [T, T, T, T, T]             │
│  node_locs: Vec<RecordLoc>   [(pg1,0), (pg1,1), ...]     │
│  edge_locs: Vec<RecordLoc>   [(pg2,0), (pg2,1), ...]     │
│                                                          │
│  ── Indexes ──                                           │
│  label_to_nodes: HashMap     {"Account"→[0,1,2,3], ...}  │
│  outgoing: HashMap<u32,Vec>  {0→[3,4], 1→[1], ...}      │
│  incoming: HashMap<u32,Vec>  {0→[2], 1→[1], ...}        │
│                                                          │
│  ── NOT in memory ──                                     │
│  String names, labels, properties                        │
│  → read from disk via page cache when needed             │
│                                                          │
│  ── Page cache (LRU) ──                                  │
│  Up to 2000 pages (≈8 MB) of recently accessed pages     │
│                                                          │
│  ── Graph-type catalog ──                                │
│  Loaded eagerly from the chain at `header.catalog_root`. │
│  Mutations go through `catalog_mut()` + `save_catalog()` │
│  (rewrites the chain, updates the header).               │
└──────────────────────────────────────────────────────────┘

                    ┌─────────────────┐
     cache miss ──► │  .gql file      │ ◄── read page from disk
                    │  (4KB pages)    │
                    └─────────────────┘

Memory: O(edges) for topology + O(label index) + O(cache_size)
Source: .gql file
Query speed: fast for topology, slower when accessing labels/props
Best for: large graphs where labels/props are sparse in queries
```

### 4.3 DiskGraphStore

Minimal memory. Only the edge topology arrays and a small page cache.
Even indexes (label, adjacency) are read from disk on demand.

```
What's in memory:
┌──────────────────────────────────────────────────────────┐
│  node_count: u32             5                           │
│  edge_count: u32             5                           │
│  edge_src: Vec<u32>          [2, 3, 1, 0, 0]             │
│  edge_tgt: Vec<u32>          [3, 1, 0, 2, 4]             │
│  edge_directed: Vec<bool>    [T, T, T, T, T]             │
│  node_locs: Vec<(u32,u16)>   [(pg1,0), (pg1,1), ...]     │
│  edge_locs: Vec<(u32,u16)>   [(pg2,0), (pg2,1), ...]     │
│                                                          │
│  ── NOT in memory ──                                     │
│  String names, labels, properties, adjacency index,      │
│  label index → ALL read from disk via page cache         │
│                                                          │
│  ── Page cache (LRU) ──                                  │
│  Up to 2000 pages (≈8 MB)                                │
└──────────────────────────────────────────────────────────┘

Memory: O(edges) for topology only + O(cache_size)
Source: .gql file
Query speed: slowest (everything goes through cache/disk)
Best for: very large graphs that don't fit in memory
```


## 5. Comparison

```
                  ┌────────────┬──────────────┬───────────────┐
                  │  In-Memory │ LazyGraph    │ DiskGraph     │
                  │  (Graph)   │ Store        │ Store         │
┌─────────────────┼────────────┼──────────────┼───────────────┤
│ Source format    │ JSON       │ .gql file    │ .gql file     │
│ Labels in RAM   │ yes        │ no (cached)  │ no (cached)   │
│ Props in RAM    │ yes        │ no (cached)  │ no (cached)   │
│ Adjacency       │ Vec<Vec>   │ HashMap      │ disk (cached) │
│ Topology        │ Vec<u32>   │ Vec<u32>     │ Vec<u32>      │
│ String names    │ Vec<Str>   │ no (on disk) │ no (on disk)  │
│ Memory usage    │ highest    │ low          │ lowest        │
│ Query speed     │ fastest    │ fast         │ slowest       │
│ Startup time    │ slow(parse)│ medium       │ fast          │
└─────────────────┴────────────┴──────────────┴───────────────┘
```

Note: in all three backends, the **query engine** works exactly the same
way. It calls `outgoing_edges(node_id)`, `node_labels(id)`, etc. through
the `GraphAccess` trait. The backend decides whether that's an array
lookup, a hash map lookup, or a disk read.


## 6. TripleIndex (in-memory, built at query time)

When the query engine encounters a multi-way join or a long chain of
directed edges, it builds a temporary in-memory structure called the
TripleIndex to feed the Leapfrog Triejoin (LTJ) algorithm. This index
is not saved to disk — it's rebuilt each time from whatever backend
(`Graph`, `LazyGraphStore`, etc.) is in use.

For a full explanation of LTJ and why it matters, see
`JOIN_STRATEGY_NOTES.md`.

### 6.1 What it stores

Each directed edge becomes a triple `(source, label, target)`. For our
fraud graph:

```
Edge t1: p1 ──Transfer──► p2  →  triple (2, Transfer, 3)
Edge t2: p2 ──Transfer──► a2  →  triple (3, Transfer, 1)
Edge t3: a2 ──Transfer──► a1  →  triple (1, Transfer, 0)
Edge t4: a1 ──Transfer──► p1  →  triple (0, Transfer, 2)
Edge t5: a1 ──Foo──► d1       →  triple (0, Foo, 4)
```

(Using internal IDs: a1=0, a2=1, p1=2, p2=3, d1=4)

These 5 triples are stored in **6 different sorted copies**. Each copy
sorts the same data by a different permutation of the three components.

### 6.2 Why 6 copies?

Imagine looking up a phone number. If the phone book is sorted by last
name, finding "Smith" is fast (binary search). But if you only know the
phone number and want the name, the book is useless — you need a reverse
directory.

Same idea here. The SPO ordering (sorted by source, then label, then
target) is perfect for the question "who does node 0 send Transfer edges
to?" — just binary search for the prefix (0, Transfer, ?). But it can't
answer "who sends edges TO node 3?" efficiently. For that question, we
need OSP ordering (sorted by target first).

With 6 orderings, **every possible question** can be answered with a
binary search:

```
"Who does node 0 connect to?"       → SPO:  search (0, ?, ?)
"Who connects TO node 3?"           → OSP:  search (3, ?, ?)
"Which edges have label Transfer?"  → POS:  search (Transfer, ?, ?)
"Does node 0 connect to node 4?"    → SOP:  search (0, 4, ?)
"Who sends Transfer edges to node 0?" → POS: search (Transfer, 0, ?)
```

Here are two of the six orderings for our fraud graph:

```
SPO (sorted by source, label, target):
  (0, Foo,      4)    ← a1 ──Foo──► d1
  (0, Transfer, 2)    ← a1 ──Transfer──► p1
  (1, Transfer, 0)    ← a2 ──Transfer──► a1
  (2, Transfer, 3)    ← p1 ──Transfer──► p2
  (3, Transfer, 1)    ← p2 ──Transfer──► a2

OSP (sorted by target, source, label):
  (0, 1, Transfer)    ← a2 ──Transfer──► a1   (target a1 first)
  (1, 3, Transfer)    ← p2 ──Transfer──► a2   (target a2 first)
  (2, 0, Transfer)    ← a1 ──Transfer──► p1   (target p1 first)
  (3, 2, Transfer)    ← p1 ──Transfer──► p2   (target p2 first)
  (4, 0, Foo)         ← a1 ──Foo──► d1        (target d1 first)
```

### 6.3 How it's built

The construction is straightforward: read all directed edges from the
graph (via `GraphAccess`), create one triple per edge per label, then
sort 6 copies.

```
Graph (any backend)
  │
  │  edges_directed() → [0, 1, 2, 3, 4]
  │  For each edge: src(), tgt(), edge_labels()
  │
  ▼
Raw triples: [(0,"Transfer",2), (0,"Foo",4), (1,"Transfer",0), ...]
  │
  │  Assign numeric label IDs: Transfer=0, Foo=1
  │
  ▼
Numeric triples: [(0,0,2,edge0), (0,1,4,edge4), (1,0,0,edge2), ...]
  │               src lbl tgt eid
  │
  │  Copy 6 times, reorder each copy, sort
  │
  ▼
SPO: sorted by (src, label, tgt)
SOP: sorted by (src, tgt, label)
POS: sorted by (label, tgt, src)
PSO: sorted by (label, src, tgt)
OSP: sorted by (tgt, src, label)
OPS: sorted by (tgt, label, src)
```

Each entry is 4 × u32 = 16 bytes. For 100K edges, each ordering is
~1.6 MB; all 6 together are ~10 MB. Construction time is dominated by
sorting: O(E log E).

### 6.4 How queries use it

The LTJ algorithm navigates these orderings using three operations:

**leap(start, end, depth, key)** — binary search for the first value
≥ key at a given column within a range. This is the workhorse:

```
SPO ordering:
  (0, Foo,      4)    index 0
  (0, Transfer, 2)    index 1
  (1, Transfer, 0)    index 2
  (2, Transfer, 3)    index 3
  (3, Transfer, 1)    index 4

leap(0, 5, depth=0, key=2)
  → binary search column 0 (src) for value ≥ 2
  → finds index 3: (2, Transfer, 3)
  → returns value=2
```

**range_for_key** — finds the exact range of rows where a column equals
a given value (two binary searches):

```
range_for_key(0, 5, depth=0, key=0)
  → rows where src=0: indices [0, 1]
  → returns range (0, 2)
```

**all_values** — lists all distinct values in a column within a range
(linear scan):

```
all_values(0, 5, depth=0)
  → distinct sources: [0, 1, 2, 3]
```


## 7. Query-Time Data Flow

This section traces how different query shapes flow through the system,
from pattern to results.

### Example 1: `(x: Account) -[:Transfer]-> (y)` — simple traversal

A single edge with labels. The runtime uses adjacency-driven concat
(not LTJ), since a single edge doesn't need multi-way join.

```
1. SCAN: find nodes with label "Account"
         graph.nodes_with_label("Account") → [0, 1, 2, 3]
         This uses the label index:
         ├── In-Memory backend: HashMap lookup (instant)
         ├── Lazy backend: HashMap lookup (index is in RAM)
         └── Disk backend: read label index page from disk

2. EXPAND: for each Account node, get outgoing edges
         graph.outgoing_edges(0) → [3, 4]   (a1's outgoing edges)
         graph.outgoing_edges(1) → []        (a2 has no outgoing... wait)

3. FILTER: keep only edges with label "Transfer"
         graph.edge_labels(3) → Transfer ✓
         graph.edge_labels(4) → Foo ✗  (skip this edge)

4. TARGET: get where the edge leads
         graph.tgt(3) → 2 (node p1)

5. RESULT: x=0 (a1), y=2 (p1), path = [Node(0), Edge(3), Node(2)]
```

### Example 2: `(a)->(b)->(c)->(d)` — long chain via LTJ

A chain of 3 edges. Without LTJ, this requires materializing all 2-hop
paths before considering 3-hop paths, which can be slow on dense graphs.
With LTJ, it's evaluated incrementally.

```
1. FLATTEN the Concat tree:
     [Node(a), EdgeRight, Node(b), EdgeRight, Node(c), EdgeRight, Node(d)]

2. EXTRACT triples (one per edge):
     Triple 1: (a, _p0, b)    _p0 = any label, fresh variable
     Triple 2: (b, _p1, c)    _p1 = any label
     Triple 3: (c, _p2, d)    _p2 = any label

3. BUILD TripleIndex: 6 sorted arrays from all graph edges

4. CHOOSE variable order (VEO):
     Variables b and c appear in 2 triples each (non-lonely) → bind first
     Variables a and d appear in 1 triple each → bind after
     Variables _p0, _p1, _p2 appear in 1 triple each → bind last
     Order: [b, c, a, d, _p0, _p1, _p2]

5. SEARCH:
     for each b:                           ← leap across triples 1,2
       for each c in out(b):              ← leap across triples 2,3
         for each a such that a→b:        ← leap in triple 1 alone
           for each d in out(c):          ← leap in triple 3 alone
             _p0, _p1, _p2 are lonely → seek_all (get all matching labels)
             emit (a, b, c, d)

   No intermediate table. Each step only considers values that satisfy
   all constraints so far.

6. CONVERT: each result tuple {a=5, b=12, c=7, d=20} becomes:
     Assignment: {a→Node(5), b→Node(12), c→Node(7), d→Node(20)}
     Path: [Node(5), Edge(?), Node(12), Edge(?), Node(7), Edge(?), Node(20)]
     (Edge IDs are looked up in the graph's adjacency lists)
```

### Example 3: `(a)->(b), (b)->(c), (c)->(a)` — triangle via LTJ

The classic multi-way join. Three patterns share variables pairwise.

```
1. DECOMPOSE the Join tree:
     Join(Join((a)->(b), (b)->(c)), (c)->(a))
     → Triple 1: (a, _p0, b)
       Triple 2: (b, _p1, c)
       Triple 3: (c, _p2, a)

2. SEARCH with leapfrog intersection:
     for each a:
       Triples 1 and 3 both mention a (as source and target respectively).
       The iterator for triple 1 uses SPO (a is source).
       The iterator for triple 3 uses OSP (a is target).
       Leapfrog finds the first a that exists in both orderings.

       for each b in out(a):
         Now a is fixed. Triple 1 narrows to "edges from a".
         Triple 2 needs b as source.
         Leapfrog intersects: b must be a target of a AND a source of
         some edge. On dense graphs, this skips many non-matching b values.

         for each c in out(b) ∩ in(a):
           This is where the magic happens. Triple 2 says c must be
           reachable from b. Triple 3 says c must reach a. Instead of
           finding ALL neighbors of b and then checking which ones reach a,
           LTJ intersects both lists simultaneously.

           If out(b) = [3, 7, 12, 25] and in(a) = [5, 12, 18, 25, 40]:
           → intersection = [12, 25] (found in O(log n) per step, not O(n))
           → only 2 candidates for c, not 4

           emit (a, b, c) for each valid c
```

### Example 4: `(x: Person)(y: Student)` — node concat, no edge

Two node patterns without an edge between them. This is just a filter:
"find nodes that are both Person and Student". No LTJ, no adjacency.

```
1. SCAN: nodes_with_label("Person") → [0, 4, ...]

2. For each Person node, check if it also has label "Student"
   via filter_node(). If yes, bind x and y to the same node.

   No edge expansion, no join, no LTJ.
```

### Summary: how the runtime chooses a strategy

```
                    ┌──────────────────────────────┐
                    │     Incoming pattern          │
                    └──────────────┬───────────────┘
                                   │
                    ┌──────────────▼───────────────┐
                    │  Is it a Join or Concat of    │
                    │  directed edges only?          │
                    └──────┬──────────────┬─────────┘
                       yes │              │ no
                    ┌──────▼──────┐  ┌────▼─────────────────┐
                    │ Decompose   │  │ Use existing runtime: │
                    │ into triples│  │ adjacency-driven concat│
                    │ Build index │  │ or pairwise hash-join  │
                    │ Run LTJ     │  │ or node scan           │
                    └─────────────┘  └────────────────────────┘
```

### Key insight

**Topology (edge_src, edge_tgt) is always in memory** in all backends.
Labels and properties are the expensive part — they require decoding
from disk in the lazy/disk backends.

**LTJ adds a temporary index** (the TripleIndex) that reorganizes the
topology data into 6 sorted orderings. This index is O(E) in space and
O(E log E) to build, but enables multi-way joins without intermediate
blowup. For queries on unlabeled graphs (like LiveJournal), the label
component is a fresh variable that LTJ handles naturally.
