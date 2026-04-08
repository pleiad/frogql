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

## Possible Improvements (ordered by effort)

### 1. Propagate limit to sub-queries in joins (easy)
Currently `run_join` passes `limit=0` to sub-queries. For queries with shared variables, we could estimate how many rows we need from each side. Won't help with the fundamental blowup but would help when limit is small.

### 2. Join reordering (medium)
Currently left-to-right. Reorder joins so that the most selective joins happen first. E.g., for 4-clique, do triangle first then extend. This is what CLTJ calls "adaptive VEO" (Variable Elimination Order).

### 3. Semijoin reduction (medium)  
Before joining, reduce each side by checking which values of shared variables actually appear in the other side. This avoids producing rows that will be filtered out.

### 4. Index-nested-loop join for edge patterns (medium)
Instead of materializing `(a)-[]->(b)` as all edges, iterate over edges lazily and probe the index for the next pattern. This is closer to LTJ's behavior.

### 5. Implement LTJ over adjacency lists (hard)
Build sorted adjacency lists, implement `leap()` (exponential search), and a generic multi-way join that binds variables one at a time with leapfrog intersection. Would need sorted adjacency lists (currently HashMap-based).

### 6. Full trie index (hard)
Build proper tries for (src, label, tgt) in all orderings, implement full LTJ. Would also need to decide how to handle property constraints.

## Decision to Make

The core question: **RDF triples vs. property graph model?**

- If we go RDF: reuse CLTJ's approach directly, compare apples-to-apples with the paper
- If we stay property graph: need a custom join strategy that handles labels/properties natively
- Hybrid is possible but complex

For the benchmark comparison with CLTJ, option 1 (RDF) makes the most sense since we're comparing on the same datasets and queries. For the broader GQL project, option 3 (hybrid) is more interesting but much more work.
