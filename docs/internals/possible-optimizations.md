# Possible Optimizations

## 1. Fixed-length cells instead of slotted pages

**Current:** Node/edge records are variable-length (different numbers of
labels and properties), stored in slotted pages with a pointer array.

**Problem:** The slotted-page machinery (pointer array, free space tracking,
cell area growing from the bottom) adds complexity and overhead. For the
common case of graphs loaded once and queried many times (no in-place
updates), this complexity is unnecessary.

**Proposed:** Use fixed-length cells for topology (the part needed at
query time) and store labels/properties in separate structures:

```
Fixed-length edge cell (9 bytes):
┌──────────────┐
│ src: u32     │
│ tgt: u32     │
│ directed: u8 │
└──────────────┘

Separate label store:  label_string → Vec<node/edge IDs>  (already exists)
Separate prop store:   (entity_id, prop_name) → value
```

Benefits:
- Cell #N is at `offset + N * cell_size` — no pointer array, pure arithmetic
- No fragmentation or wasted space
- Cache-friendly sequential scans
- Simpler code (no slotted-page logic)
- For unlabeled graphs (like LiveJournal), node records become zero-size

Trade-off: reading all data for a single node (labels + props + topology)
requires multiple seeks. But LazyGraphStore already does this anyway.


## ~~2. Label index for compound labels (A&B)~~ (DONE)

Implemented. See `implemented-optimizations.md`.


## 3. Sorted adjacency lists for intersection

**Current:** Adjacency lists are `Vec<u32>` in insertion order (not sorted).
Finding the intersection of two adjacency lists requires hash set
construction or O(n*m) nested loops.

**Proposed:** Keep adjacency lists sorted by target node ID. Then
intersection can be done in O(n+m) with a merge-join, or O(n*log(m))
with binary/exponential search (this is what LTJ's `leap()` does).

This is a prerequisite for implementing any leapfrog-style join.


## ~~4. Remove string names from topology~~ (DONE)

Implemented. See `implemented-optimizations.md`.


## 5. Propagate limit into join sub-queries

**Current:** `run_join` evaluates both sub-queries with `limit=0`
(unlimited) before joining:

```rust
let ir1 = self.run_path_pattern(q1, 0);  // all results
let ir2 = self.run_path_pattern(q2, 0);  // all results
// then hash-join
```

**Problem:** For a 6-way join (4-clique), intermediate results explode
even though the final result may be limited to 1000 rows.

**Proposed (incremental):** Estimate how many rows to request from each
side based on selectivity heuristics. Or use semijoin reduction: before
computing ir1 fully, probe ir2 to find which values of the shared
variable actually exist, and only compute ir1 for those values.


## 6. Index-nested-loop join for single-edge patterns

**Current:** `(a)-[]->(b), (a)-[]->(c)` evaluates both sides as full
edge scans (100K edges each), then hash-joins on `a`.

**Proposed:** Evaluate `(a)-[]->(b)` as the outer, and for each row's
value of `a`, probe the adjacency index for `(a)-[]->(c)` directly:

```
for each edge (a→b):
  for each c in outgoing(a):   ← probe index, not scan
    emit (a, b, c)
```

This is essentially converting the hash-join into an index-nested-loop
join. Much cheaper when one side is a simple edge pattern with a shared
node variable.


## 7. Leapfrog Triejoin (LTJ)

See `JOIN_STRATEGY_NOTES.md` for the full analysis. LTJ would
replace the pairwise join strategy with a multi-way join that binds
variables one at a time, intersecting candidate lists across all
patterns simultaneously. This avoids intermediate result blowup entirely.

Requires: sorted adjacency lists (#3), and possibly trie indexes for
different access patterns (src→edges, tgt→edges, label→edges, etc).
