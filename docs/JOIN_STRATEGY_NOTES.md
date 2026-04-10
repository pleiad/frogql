# Join Strategy: Current State & Future Directions

## Current Implementation (pairwise hash-join)

The comma-join `Q1, Q2` evaluates each sub-query fully, then joins:

```rust
// engine.rs run_join()
let ir1 = self.run_path_pattern(q1, 0);  // materialize ALL of Q1
let ir2 = self.run_path_pattern(q2, 0);  // materialize ALL of Q2
// then hash-join on first shared variable
```

For a 4-clique `(a)-[]->(b), (a)-[]->(c), (a)-[]->(d), (b)-[]->(c), (b)-[]->(d), (c)-[]->(d)`:
- Parser builds left-associative chain: J1 = join(Q1,Q2), J2 = join(J1,Q3), ...
- Each join materializes its inputs fully before joining
- Intermediate blowup: a node with degree 100 → J1 produces 10K rows, J2 produces 1M, etc.
- Most intermediate rows are filtered out by later joins (e.g., J3 checks b→c)

## Benchmark Results (100K edges, limit=1000)

| Shape | Time | Issue |
|-------|------|-------|
| 1-tree | 0.28s | OK — single join, small |
| 2-comb | 0.40s | OK — linear concat, no join |
| 3-clique | 0.57s | OK — 3-way join, manageable |
| 3-cycle | 0.57s | OK |
| 2-3-lollipop | 1.56s | Acceptable |
| 4-cycle | 4.15s | Slow — 4-way join |
| 3-path | 6.33s | Slow — long chain without anchoring |
| 3-4-lollipop | 23.1s | Very slow |
| 2-tree | 101.8s | TIMEOUT — 3-way join with high-degree nodes |
| 4-path | 159.8s | TIMEOUT — 5-node chain |
| 4-clique | hung | 6-way join, completely explodes |

## Why Leapfrog Triejoin (LTJ) Would Help

LTJ doesn't do pairwise joins. It binds variables one at a time, intersecting candidate lists across ALL patterns simultaneously.

For 4-clique, instead of:
```
J1: all (a,b) edges × all (a,c) edges → huge intermediate
J2: J1 × all (a,d) edges → even bigger
J3: J2 filtered by (b,c) edges → most of J2 was wasted
```

LTJ does:
```
for each 'a':
  for each 'b' in out(a):
    for each 'c' in out(a) ∩ out(b):          ← intersection NOW
      for each 'd' in out(a) ∩ out(b) ∩ out(c):  ← intersection NOW
        emit (a,b,c,d)
```

Key insight: when binding variable 'c', LTJ already knows c must be in out(a) AND out(b), so it intersects both adjacency lists immediately. Never materializes (a,b,c) combinations where b→c doesn't exist.

The `leap()` operation does this intersection efficiently via exponential search on sorted lists: O(log ℓ) per step where ℓ is the gap between consecutive results.

## The 6 Tries Question

In RDF, a triple (s,p,o) can be queried by any prefix. LTJ needs 6 tries (SPO, SOP, POS, PSO, OSP, OPS) to support any variable elimination order.

In our property graph model, an edge is (src, label, tgt). Our current indexes only cover:
- `outgoing[src]` → edge IDs (like S?? in RDF)
- `incoming[tgt]` → edge IDs (like ??O in RDF)
- `label_to_edges[label]` → edge IDs (like ?P? in RDF)

We're missing combined indexes like:
- Given (src, label), find targets → SP? 
- Given (label, tgt), find sources → ?PO
- Given (tgt, src), find labels → OS?

For the unlabeled LiveJournal benchmark this doesn't matter (only one "label"), but for real GQL queries with labels it would.

## The Property Graph Complication

In RDF, everything is a triple. Labels and properties are just more triples:
```
(x, rdf:type, Person)
(x, name, "Alice")
```

In our property graph, labels and properties are **node/edge attributes**, not edges. A query like `(x: Person {a: str})` filters by label AND property type — this isn't expressible as edge traversal.

This means LTJ's trie-based approach doesn't directly apply to property constraints. We'd need either:
1. **Model properties as triples** (convert to RDF-like model) — then LTJ works natively
2. **Keep property graph model** and improve the join strategy separately from property filtering
3. **Hybrid**: use LTJ for the join/intersection part, with property predicates as post-filters during variable binding

## Implementation: LTJ (done)

Leapfrog Triejoin is now implemented in `src/runtime/ltj/`. See `CLAUDE.md` for full documentation.

The approach taken was hybrid: model edges as (src, label, tgt) triples stored in 6 sorted orderings (like CLTJ), but use sorted `Vec<(u32,u32,u32,u32)>` with binary search instead of compact tries with LOUDS. Node label and property constraints are handled as filters evaluated during the LTJ search, placed at the VEO level where all required variables are bound.

### Results (soc-LiveJournal1-100k, limit=1000)

| Shape | Hash-join (before) | LTJ (after) | Speedup |
|-------|-------------------|-------------|---------|
| 1-tree | 0.28s | 0.039s | 7x |
| 2-comb | 0.40s | 0.039s | 10x |
| 3-clique | 0.57s | 0.041s | 14x |
| 3-cycle | 0.57s | 0.041s | 14x |
| 4-cycle | 4.15s | 0.041s | 101x |
| 3-path | 6.33s | 0.040s | 158x |
| 2-tree | 101.8s | 0.039s | 2610x |
| 4-path | 159.8s | 0.039s | 4097x |
| 3-4-lollipop | 23.1s | 0.040s | 577x |
| 4-clique | hung | 0.043s | ∞ |

## Remaining Improvements

### 1. Adaptive VEO (medium)
Current VEO is static. An adaptive VEO re-estimates cardinalities at each step during the search, choosing the variable with the smallest candidate set. The CLTJ paper shows this reduces average query times by an order of magnitude.

### 2. Cache TripleIndex (easy)
Currently rebuilt per query. Should be cached on Runtime via `RefCell<Option<TripleIndex>>` and reused across queries on the same graph.

### 3. Unroll repetitions into LTJ (medium)
`{1,m}` with fixed bounds could be unrolled into m separate LTJ executions, each with k copies of the sub-pattern's triples. Results are unioned, and variables are collected into `PathValue::List`.

### 4. Handle undirected/left edges (medium)
Model undirected edges as two directed triples `(a,L,b)` + `(b,L,a)`, or add separate index.

### 5. Compact trie representation (hard)
Replace sorted Vec with LOUDS bitvectors for space efficiency on billion-triple graphs. Only matters for very large datasets.
