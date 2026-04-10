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
│                node ID index, edge ID index           │
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
│ Pages K+1...: Indexes                                │
│   Label index (which nodes/edges have each label)    │
│   Adjacency index (outgoing/incoming per node)       │
└──────────────────────────────────────────────────────┘
```

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


## 6. TripleIndex (in-memory, query-time)

El `TripleIndex` es una estructura in-memory construida en tiempo de query
para alimentar el algoritmo Leapfrog Triejoin (LTJ). No se persiste a disco:
se construye a partir de cualquier backend `GraphAccess`.

### 6.1 Qué es

Cada arista dirigida del grafo se modela como un triple `(src, label_id, tgt)`,
similar a RDF. Los labels se mapean a IDs numéricos con un diccionario local.
El triple se almacena en 6 copias, cada una ordenada lexicográficamente por
una permutación distinta de las 3 componentes:

```
Orderings para el grafo de ejemplo (5 aristas dirigidas):

SPO (src, label, tgt):     SOP (src, tgt, label):
  (0, Transfer, 2)           (0, 2, Transfer)
  (0, Foo,      4)           (0, 4, Foo)
  (1, Transfer, 0)           (1, 0, Transfer)
  (2, Transfer, 3)           (2, 3, Transfer)
  (3, Transfer, 1)           (3, 1, Transfer)

POS (label, tgt, src):     PSO (label, src, tgt):
  (Foo,      4, 0)           (Foo,      0, 4)
  (Transfer, 0, 1)           (Transfer, 0, 2)
  (Transfer, 1, 3)           (Transfer, 1, 0)
  (Transfer, 2, 0)           (Transfer, 2, 3)
  (Transfer, 3, 2)           (Transfer, 3, 1)

OSP (tgt, src, label):     OPS (tgt, label, src):
  (0, 1, Transfer)           (0, Transfer, 1)
  (1, 3, Transfer)           (1, Transfer, 3)
  (2, 0, Transfer)           (2, Transfer, 0)
  (3, 2, Transfer)           (3, Transfer, 2)
  (4, 0, Foo)                (4, Foo,      0)
```

Cada entrada es `(u32, u32, u32, u32)`: las 3 componentes en el orden
del ordering + el edge_id original (para reconstruir los resultados).

### 6.2 Por qué 6 orderings

LTJ necesita buscar eficientemente por cualquier prefijo de un triple.
Si ya sabemos que `src=0` y `label=Transfer`, necesitamos encontrar
rápidamente todos los `tgt` posibles. El ordering SPO resuelve esto:
buscamos el rango donde `(0, Transfer, ?)` y leemos los targets.

Pero si sabemos `tgt=3` y queremos buscar `src`, necesitamos un
ordering que tenga `tgt` antes que `src`: OSP o OPS. Con 6 orderings,
cualquier combinación de 0, 1 o 2 componentes fijas tiene un ordering
donde las fijas están al principio y la variable buscada al final.

```
Variables fijadas → Ordering usado → Variable al depth correcto
────────────────────────────────────────────────────────────────
(nada fijo, buscar S) → SPO   → S está en depth 0
(S fijo, buscar P)    → SPO   → P está en depth 1
(S fijo, buscar O)    → SOP   → O está en depth 1
(S y P fijos, buscar O) → SPO → O está en depth 2
(P fijo, buscar S)    → PSO   → S está en depth 1
(O fijo, buscar S)    → OSP   → S está en depth 1
...etc.
```

### 6.3 Cómo se construye

`TripleIndex::from_graph(graph)` itera `edges_directed()`, extrae los
labels de cada arista, asigna IDs numéricos a los labels, y construye
los 6 arrays sorted. Complejidad: O(E log E) por el sort.

```rust
// Simplificado:
for eid in graph.edges_directed() {
    let src = graph.src(eid);
    let tgt = graph.tgt(eid);
    for label in graph.edge_labels(eid).required_labels() {
        let lid = label_to_id[label];
        raw_triples.push((src, lid, tgt, eid));
    }
}
// Luego: 6 copias reordenadas y sorted
```

### 6.4 Operaciones sobre el índice

```
leap(slice, begin, end, depth, key) → Option<(value, pos)>
  Binary search en slice[begin..end] para el primer valor ≥ key
  en la componente `depth`. O(log n).

range_for_key(slice, begin, end, depth, key) → (lo, hi)
  Rango exacto de entries cuya componente `depth` == key.
  Dos binary searches. O(log n).

all_values(slice, begin, end, depth) → Vec<u32>
  Todos los valores distintos en la componente `depth`.
  Scan lineal sobre el rango. O(n).
```

### 6.5 Relación con los backends de almacenamiento

El TripleIndex funciona con cualquier backend. Lee datos a través del
trait `GraphAccess`, así que la fuente puede ser JSON en RAM, .gql
con page cache, o cualquier implementación futura:

```
┌──────────────────────────────────────────────────────────┐
│ Query: (a)->(b), (b)->(c), (c)->(a)                     │
│                                                          │
│ engine.rs detecta Join → intenta LTJ                     │
│   ↓                                                      │
│ TripleIndex::from_graph(graph)                           │
│   ├── graph.edges_directed()  ← funciona con cualquier   │
│   ├── graph.src(eid)            backend GraphAccess       │
│   ├── graph.tgt(eid)                                     │
│   └── graph.edge_labels(eid)                             │
│   ↓                                                      │
│ 6 arrays sorted (in-memory, efímeros)                    │
│   ↓                                                      │
│ LTJ: iteradores + leapfrog seek                          │
│   ↓                                                      │
│ Resultados → IntermediateResult                          │
└──────────────────────────────────────────────────────────┘
```


## 7. Query-Time Data Flow

### Example 1: `(x: Account) -[:Transfer]-> (y)` (concat, single edge)

Una travesía con label. Para un solo edge, el runtime usa adjacency-driven
concat (no LTJ, porque un solo triple no se beneficia).

```
1. SCAN: engine calls graph.nodes_with_label("Account")
         Uses label index → returns [0, 1, 2, 3]
         ├── In-Memory: HashMap lookup (in RAM)
         ├── Lazy: HashMap lookup (index in RAM)
         └── Disk: read label index page from disk/cache

2. EXPAND: for each Account node, follow outgoing edges
           engine calls graph.outgoing_edges(node_id) → Vec<u32>
           ├── In-Memory: array index → outgoing[node_id]
           ├── Lazy: HashMap lookup → outgoing[node_id]  (index in RAM)
           └── Disk: read adjacency page from disk/cache

3. FILTER EDGE: check edge has label "Transfer"
                engine calls graph.edge_labels(edge_id)
                ├── In-Memory: array index → edge_labels[id]
                ├── Lazy: read edge record page → decode
                └── Disk: read edge record page → decode

4. RESOLVE TARGET: get target node
                   engine calls graph.tgt(edge_id) → u32
                   All three: array index → edge_tgt[id]  (always in RAM)

5. RESULT: PathValue::Node(target_id), bind to variable y
           All u32 — no string allocation
```

### Example 2: `(a)->(b)->(c)->(d)` (cadena larga, LTJ)

Una cadena de 3 aristas. El runtime usa LTJ porque la cadena se descompone
en 3 triples con variables compartidas.

```
1. DECOMPOSE: flatten Concat tree → [Node(a), Edge, Node(b), Edge, Node(c), Edge, Node(d)]
              Extract triples: (a,_p0,b), (b,_p1,c), (c,_p2,d)

2. BUILD INDEX: TripleIndex::from_graph → 6 sorted arrays

3. VEO: order variables by connectivity.
        a,b,c,d are non-lonely (shared between triples, except a and d).
        _p0,_p1,_p2 are lonely (each in one triple).
        Order chosen: b, c, a, d, _p0, _p1, _p2 (example)

4. SEARCH (recursive):
   para cada b (leap en SPO para triples 1 y 2):
     para cada c en out(b) (triples 2 y 3):
       para cada a en in(b) (triple 1):
         para cada d en out(c) (triple 3):
           // _p0, _p1, _p2 son lonely: seek_all directo
           emit (a, b, c, d)

   Nunca materializa todos los pares (a,b) antes de ver c.

5. RESULT: convert tuples → IntermediateResult con Assignment y Path
```

### Example 3: `(a)->(b), (b)->(c), (c)->(a)` (triangle, LTJ)

```
1. DECOMPOSE: Join(Join((a)->(b), (b)->(c)), (c)->(a))
              Triples: (a,_p0,b), (b,_p1,c), (c,_p2,a)

2. SEARCH:
   para cada a (triples 1 y 3):
     para cada b en out(a) ∩ ... (triples 1 y 2):
       para cada c en out(b) ∩ in(a) (triples 2 y 3):
         emit (a, b, c)

   La intersección de out(b) ∩ in(a) se hace con leapfrog seek:
   se busca alternadamente en los iteradores de triples 2 y 3
   hasta que ambos coinciden en el mismo valor de c.
```

### Example 4: `(x: Person)(y: Student)` (concat sin edge)

Two node patterns concatenated. No edge, no LTJ.

```
1. SCAN: engine calls nodes_with_label("Person") → [0, 4, ...]

2. CONCAT WITH NODE: for each Person node, check if it also
         matches "Student" (via filter_node). The result
         has path [node4] with bindings {x → 4, y → 4}.

   No adjacency expansion happens. This is just a filter.
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
