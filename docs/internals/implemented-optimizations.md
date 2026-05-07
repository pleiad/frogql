# Implemented Optimizations

## 1. u32 internal IDs (replacing String IDs)

**Before:** All node/edge IDs were `String` throughout the system — in
`PathValue`, `GraphAccess`, `Assignment`, adjacency maps, and hash joins.
Every operation involved string hashing (O(n)), string comparison (O(n)),
and string cloning (heap allocation).

**After:** All IDs are `u32` (`pub type Id = u32`). `PathValue` is now
`Copy` (no heap allocation). HashMap keys are u32 (O(1) hash). Assignment
unification compares integers instead of strings.

**Impact:**
- `PathValue::Node(u32)` is 5 bytes vs ~32 bytes for `PathValue::Node(String)`
- Hash lookups: O(1) for u32 vs O(len) for String
- No cloning: u32 is Copy, no heap allocation per result row
- Benchmark: 2-comb query on 100K edges went from 1.13s to 0.40s (~3x faster)

**Files changed:** `model/value.rs`, `model/graph_access.rs`, `model/graph.rs`,
`runtime/engine.rs`, `runtime/assignment.rs`, `store/lazy.rs`, `store/disk.rs`,
`store/io.rs`


## 2. Label index for compound labels (A&B)

**Before:** The label index was only used for simple single-label queries
like `(x: Account)`. For compound labels like `(x: Person & Teacher)`,
the engine fell back to a **full scan** of all nodes, then checked each
node's labels against the compound type.

```rust
// Old: only worked for Label("X"), returned None for And(A,B)
fn extract_simple_label(desc) -> Option<&str> {
    desc?.dtype.label.as_simple_label()
}
```

**After:** The engine extracts all **required labels** from the label type
(labels that MUST be present, i.e., from `And` branches and bare `Label`),
looks up each in the label index, and picks the **smallest set** as the
candidate list. Then `filter_node` checks the full subtype on that
reduced set.

```rust
// New: extracts all required labels, picks smallest indexed set
fn smallest_label_set(&self, label, lookup) -> Option<Vec<Id>> {
    let required = label.required_labels();  // e.g., ["Person", "Teacher"]
    required.iter()
        .filter_map(|l| lookup(l))           // look up each in index
        .min_by_key(|v| v.len())             // pick smallest
}
```

**Example:** For `(x: Person & Teacher)` on a graph with 10,000 Person
nodes and 50 Teacher nodes:
- Before: scan all N nodes
- After: scan 50 Teacher nodes, filter for Person → up to 200x faster

**Handles correctly:**
- `A & B`: picks smaller of index(A), index(B)
- `A & B & C`: picks smallest of all three
- `A | B`: falls back to full scan (can't narrow with disjunction)
- `!A`: falls back to full scan (can't narrow with negation)
- `*`: falls back to full scan (matches everything)

**Files changed:** `typing/label_type.rs` (added `required_labels()`),
`runtime/engine.rs` (replaced `extract_simple_label` with `smallest_label_set`)


## 3. Lazy name resolution (no string names in memory)

**Before:** `LazyGraphStore` loaded all node and edge string names into
memory at startup (`node_ids: Vec<String>`, `edge_ids: Vec<String>`).
For LiveJournal (4.8M nodes, 69M edges), this consumed ~500MB+ of RAM
for strings that were never used during query execution.

**After:** String names are not loaded at startup. The store only keeps
counts (`node_count: u32`, `edge_count: u32`). When `node_name(id)` or
`edge_name(id)` is called (only for display), it reads the record from
disk on demand.

**Impact:**
- Startup RAM for LiveJournal: ~500MB less
- Load time: faster (no string allocation/hashing during page scan)
- Query performance: unchanged (names were never used in queries)
- Display: slightly slower (disk read per name), but only called for
  printing results, not during query execution

**Files changed:** `store/lazy.rs` (removed `node_ids`, `edge_ids`,
`node_id_map`, `edge_id_map`; added `node_count`, `edge_count`;
`node_name()`/`edge_name()` now read from disk)


## 4. Hash-join on shared variables

**Before (naive cross-product):** To join two result sets, check every
pair (r1, r2) for compatible assignments. If ir1 has N rows and ir2 has
M rows, this is O(N × M).

```
for r1 in ir1:          // N rows
  for r2 in ir2:        // M rows
    if r1.assignment.can_unify(r2.assignment):
      emit (r1, r2)     // O(N × M) total comparisons
```

**After (hash-join):** When the two sides share a variable (e.g., both
bind `x`), we build a hash index on one side keyed by the shared
variable's value, then probe it from the other side.

```
// Build phase: index ir2 by the value of shared variable
let ir2_by_val: HashMap<u32, Vec<index>> = ...;
for (i, r2) in ir2:
  ir2_by_val[r2.assignment["x"]].push(i)

// Probe phase: for each r1, look up matching r2s in O(1)
for r1 in ir1:
  for idx in ir2_by_val[r1.assignment["x"]]:
    if r1.can_unify(ir2[idx]):
      emit (r1, ir2[idx])
```

**Impact:** For a join like `(x)-[]->(y), (x)-[]->(z)` on a graph with
100K edges, the naive approach would do 100K × 100K = 10 billion
comparisons. The hash-join groups by `x` (the shared variable), so a
node with degree 50 only produces 50 × 50 = 2500 comparisons for that
node. Total work is proportional to the actual output size, not the
input size squared.

**Used in two places:**
- `run_join` (comma-join `Q1, Q2`): hashes on the first shared variable
  between Q1 and Q2
- `hash_join` (concat fallback): hashes on the path join point
  (last node of left = first node of right)

**Limitation:** Only indexes on the **first** shared variable. If Q1 and
Q2 share variables `x`, `y`, and `z`, we hash on `x` and then check
`y` and `z` via `can_unify`. A multi-key hash index would help for
queries with many shared variables.

**Files:** `runtime/engine.rs` (`run_join`, `hash_join`)


## 5. Early termination with result limit

**Before:** The runtime always materialized ALL results before returning.
Even with `--limit 1000`, the engine computed the complete result set
and then truncated.

**After:** `run_with_limit(pattern, limit)` propagates the limit through
the evaluation. Each combinator (concat, join, union, filter) checks
the limit and stops early once enough results are collected.

**Impact:**
- Queries that produce millions of results but only need 1000 return
  in milliseconds instead of seconds/minutes
- The limit is enforced at every level: concat loops, join loops,
  filter iterations, union merges

**Limitation:** The limit doesn't propagate into join sub-queries
(they still evaluate fully with limit=0). This is because the join
can't predict how many pre-join rows it needs. See
`possible-optimizations.md` item 5 for future work.

**Files changed:** `runtime/engine.rs` (added `limit` parameter to
`run_path_pattern`, `run_concat_pattern`, `run_join`, `concat_with_*`,
`hash_join`, `apply_filter`)
