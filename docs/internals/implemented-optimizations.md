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


## 6. OPTIONAL MATCH bind-pushdown (correlated nested-loop)

**Before:** `run_match_chain` evaluated the inner pattern of an
`OPTIONAL MATCH` against the entire graph and then performed a
`left_outer_join` against the accumulated rows. For LDBC IS5-style
chains where the OPTIONAL pattern shared variables with the outer
(`OPTIONAL MATCH (otherPerson)<-[:hasCreator]-(post)<-[:containerOf]-(forum)`,
where `otherPerson` and `forum` are already bound by the outer chain),
the global enumeration produced millions of `(Person, Post, Forum)`
triples that the join then filtered down to a handful of valid rows —
all the global work wasted.

**After:** When the inner pattern shares Node-typed variables with the
outer, `optional_via_bind_pushdown` extracts those bindings as pin
values and runs `try_ltj_with_pins` once per outer row, restricting
the inner search to ids consistent with the outer's bindings. The
LTJ's pinning machinery (originally added for the btree-driven ORDER
BY path) substitutes `Term::Variable → Term::Constant` in every
triple position before the search, so the inner LTJ never enumerates
the pinned variables' domains.

This is SQLite's correlated nested-loop strategy mapped onto the
six-ordering TripleIndex: outer rows drive the loop, inner side runs
as a small LTJ per pin, no global pattern materialisation.

**Falls back to the original global path** when:
- the optimization is disabled (`GQLITE_DISABLE_OPTIONAL_PUSHDOWN=1`);
- there are no shared variables (no correlation to exploit);
- any outer row binds a shared variable to a non-Node value (edges,
  `Nothing` from a prior empty OPTIONAL, repetition `Group`s);
- the inner pattern is not LTJ-decomposable.

**Impact (LDBC SF0.1, IS5-shape with `~[:knows]~{1,2}` outer):**

| Variant | Before | After | Speedup |
|---|---|---|---|
| `RETURN forum.title, forum.id, COUNT(post) GROUP BY ... LIMIT 20` | 52.3 s | 0.56 s | ~93× |
| `RETURN COUNT(*)` (full chain enumeration, 20 400 rows) | 72.7 s | 0.58 s | ~126× |

**Files changed:** `runtime/engine.rs` (added `optional_via_bind_pushdown`
called from the `MatchStatement::Optional` branch in `run_match_chain`),
`runtime/ltj/pattern_extract.rs` (new public `try_ltj_with_pins` that
generalises `try_ltj_with_pin` to multiple pinned variables; the
internal `try_ltj_inner` now takes `&[(&str, u32)]`).


## 7. Repeat unrolling for fixed-bound `(P){lb, ub}`

**Before:** `Repeat` was always evaluated by the runtime's
`run_repetition_pattern` / `run_repetition_range`, a hash-join
cross-product over the inner pattern's first→last node map. That
fallback never invoked the LTJ on the chain that contained the
repetition, so predicate pushdown, btree-driven range filters, and
worst-case-optimal join evaluation were all disabled across the
repetition boundary. For LDBC IC2-style patterns
`(p:Person {id})~[:knows]~{1,2}(o:Person)<-[:hasCreator]-(m)`, the
post-repetition chain enumerated ~200 k rows even when the anchored
endpoints were point-lookups.

**After:** `optimizer::unroll_repeat` rewrites `(P){lb, ub}` into
`(P)^lb | (P)^{lb+1} | ... | (P)^ub`. Each arm is a flat concat that
the LTJ extractor decomposes to triples and runs as a multi-way join.
The pass distributes Concat over Union
(`Concat(prefix, Union(a, b)) ≡ Union(Concat(prefix, a), Concat(prefix, b))`)
so each arm becomes a complete chain rather than leaving Union
embedded inside an outer Concat — Union inside Concat is one of the
LTJ decomposer's carve-outs and would force the whole chain back onto
hash-join.

**Activation guards:**
- `ub - lb + 1 ≤ MAX_UNROLL` (4) — bounds the AST blow-up;
- `P.freevars().is_empty()` — repetitions with named edge / node
  variables produce `Group` bindings whose length depends on the
  repetition count, which the pattern-level unroll can't express;
- `lb ≥ 1` — length-0 base case has different semantics (one row per
  graph node) and would require synthesising a Node descriptor;
- `ub` must be bounded — unbounded `{n,}` is a transitive closure and
  stays on the existing fallback;
- `P` must be a single edge pattern — the unroll inserts an anonymous
  `Node(None)` between consecutive copies and that boundary insertion
  is unambiguous only for edge inners.

**Disable** with `GQLITE_DISABLE_REPEAT_UNROLL=1`.

**Impact (LDBC SF0.1, IC2 with `~[:knows]~{1, 2}`):**

| Stage | Cost |
|---|---|
| Chain enumeration (no unroll) | 1.35 s |
| Chain enumeration (unroll + adjacent-edge LTJ fix) | 0.89 s (-34%) |

**Files changed:** `optimizer/unroll_repeat.rs` (new module),
`optimizer/mod.rs` (added to the per-pattern pipeline after `pushdown`).


## 8. LTJ decomposer accepts adjacent / trailing edges

**Before:** `decompose_flat_chain` required strict
`Node Edge Node Edge Node` alternation. Two consecutive edges
(`~[:knows]~~[:knows]~`, written by users or produced by the unroll
pass for `{1,2}` repetitions) failed at the second iteration of the
loop and the whole chain bailed to hash-join, even when an explicit
`()` between the edges would have made the same query LTJ-decomposable.

**After:** the loop consumes an Edge with an *optional* trailing
Node:
- if `elems[i + 1]` is a Node, use its descriptor and advance by 2
  (original behaviour);
- otherwise — adjacent edges, or a dangling trailing edge —
  synthesise a fresh internal variable and advance by 1.

The runtime `Concat` evaluator already handles back-to-back edges via
path-merge on `last_node_id` / `first_node_id`; this brings the LTJ
decomposer in line with that semantics. The earlier trailing-edge
special case folds into the same loop.

**Impact (LDBC SF0.1, isolated 2-hop):**

| Pattern | Before | After |
|---|---|---|
| `(a)~[:knows]~~[:knows]~(b)<-[:hasCreator]-(m:Comment\|Post) ... LIMIT 20` | 271 ms | **1 ms (~270×)** |
| `(a)~[:knows]~()~[:knows]~(b)...` (explicit boundary) | 1 ms | 1 ms (unchanged) |

**Files changed:** `runtime/ltj/pattern_extract.rs::decompose_flat_chain`.


## 9. ORDER BY alias resolution

**Before:** the parser stored `ORDER BY <alias>` as
`SortKey::Column(idx)` (an index into the projected row). The runtime
treated any `Column` sort key as a post-projection sort, which means
every row had to be fully projected — including expensive `COALESCE`
evaluations and multi-attr lookups — *before* the top-k truncate.
The btree-driven `try_btree_ltj_real` precondition also requires
`SortKey::Expr(AttrLookup)` and so was disabled by alias references.

**After:** `optimizer::order_by_alias` walks `Query.order_by` and
rewrites `Column(idx)` to `Expr(AttrLookup{var, attr})` when
`returns[idx]` is a non-aggregate `Expr::AttrLookup`. The runtime
then routes the sort through the pre-projection top-k heap (or
`try_btree_ltj_real`) and only the surviving k rows get projected.

**All-or-nothing rewrite:** `sort_projected_rows` and `sort_rows`
each require a uniform spec list. If any spec is non-resolvable
(alias of `COALESCE`, arithmetic, multi-var) the rewrite is skipped
for the entire ORDER BY. Aggregate / GROUP BY queries also bail —
the post-aggregation IR is `Vec<Vec<Value>>`, no surviving
binding-table to evaluate `Expr` against.

**Impact (LDBC SF0.1 IC2 with alias-keyed ORDER BY):**
2.31 s → 1.34 s (~42%; matches the raw-expression `ORDER BY` baseline).

**Files changed:** `optimizer/order_by_alias.rs` (new module),
`optimizer/mod.rs`, `lib.rs::optimize_query`.


## 10. Auto-activated btree-LTJ-real with multi-spec + Or-merge

**Before:** `try_btree_ltj_real` was gated behind
`GQLITE_ORDERBY_FORCE=btree-ltj-real`, only fired for single-spec
ORDER BY, and bailed for descriptors with `Or` labels (`Comment|Post`)
because `LabelType::required_labels()` returns empty for `Or` and the
function picks the first label whose btree exists. The pdqsort
fallback enumerated every chain row and post-sorted, regardless of
how small the LIMIT was.

**After (three layered changes):**

1. **Auto-activation.** Drop the env-var gate; the function now runs
   on every ORDER BY query and fails closed (returns `None`) on any
   precondition miss. Force off with `GQLITE_ORDERBY_FORCE=pdqsort`
   or `topk` (still useful for A/B benchmarking).

2. **Multi-spec support with cohort early-exit.** When the primary
   spec drives the btree walk, snapshot the primary attr value of
   each visited id; once `out.len() ≥ cap` and the next id's primary
   value differs from the previously processed id's, every remaining
   id has a strictly worse primary key and can't enter the top-k —
   stop the walk and sort the cohort buffer by all specs in memory
   before truncating to `k`.

3. **K-way `Or`-merge.** `LabelType::or_candidate_labels()` returns
   the leaf labels of a pure `Or`-of-leaves expression;
   `or_candidate_labels_for_var` aggregates them across every
   descriptor declaration of the variable. When `required_labels()`
   is empty but the var carries an `Or`, the function builds one
   `BtreeMergeCursor` per candidate label and walks them via
   `pick_best_cursor` (linear scan picks the cursor whose head value
   is best per asc/desc; for the typical `Or` arity of 2-3 this
   beats a heap and avoids the allocation). Ids appearing in two
   cursors at once (a node carrying multiple Or leaves) are filtered
   via a `HashSet` so the LTJ pin runs only once per id.

   `pinned_run` (the per-pin dispatcher) also handles `Union`
   (introduced by the unroll pass) by recursing into each arm and
   concatenating, and unwraps `Filter` by post-filtering the inner
   run. Without it, a pattern like `Filter(Union(arm1, arm2), Ne(...))`
   would `decompose` to None and the merge would silently fall back
   per pin.

   Coverage relaxation: `ids.len() < label_total` is permitted — the
   auto-built btree omits null-valued nodes, and `NULLS LAST` (already
   a precondition) keeps them out of the top-k anyway.

   **Correctness fix bundled in:** the original pin retain dropped
   every filter for the pinned variable, including `NodeAttrCmp`.
   Pinning fixes the `NodeId` but not the property value;
   range / equality predicates need the LTJ to read the pinned
   binding's actual prop and compare. Keep `NodeAttrCmp` /
   `NodeInSet` in the filter list so the LTJ evaluates them at level
   0 against the pinned binding.

**Impact (LDBC SF0.1, IC2 shape with `~[:knows]~{1,2}` and
`(message:Comment|Post)`, alias-keyed `ORDER BY`):**

| Stage | Tiempo |
|---|---|
| pdqsort baseline (force off) | 3.96 s |
| Auto-activation only | 1.87 s (still bails on `Or`) |
| `Or`-merge + pinned_run + filter retain fix | **0.12 s** (~33×) |
| Single-label `Comment`-only IC2 (`required_labels()` non-empty) | **0.004 s** from 2.64 s baseline (~660×) |

**Files changed:** `runtime/engine.rs` (`try_btree_ltj_real`,
`pinned_run`, `BtreeMergeCursor`, `pick_best_cursor`,
`or_candidate_labels_for_var`), `typing/label_type.rs`
(`or_candidate_labels`), `runtime/ltj/pattern_extract.rs`
(filter retain on pinned vars).


## 11. DDL secondary indexes persisted in `.gdb`

**Before:** secondary indexes lived only in memory. Every
`LazyGraphStore::open` ran `build_auto_indexes_bulk` (which only
covers `(label, prop)` pairs whose values are unique within the
label) and discarded any DDL-declared indexes from previous sessions.
Users had to re-issue `CREATE INDEX` before every session — and the
sister btree-LTJ-real path silently fell back to pdqsort when the
manual DDL was missing.

**After:** the `.gdb` header reserves `secondary_index_root: u32`
(bytes 100-103, previously zeroed and reserved); a new chain of
`PageType::SecondaryIndex` pages stores a JSON-encoded
`Vec<PersistedSpec>` of DDL-declared specs. The save side
(`save_graph_with_catalog_and_indexes_atomic` in `store/io.rs`)
writes the chain after the catalog and updates the header in the
same `.tmp` open. The load side
(`secondary_index_io::read_specs` + `LazyGraphStore::open`) replays
each persisted entry via `build_declared` after the auto-build runs.
Auto entries are not persisted — they're reproduced deterministically
on every open.

**Backward compat:** legacy `.gdb` files written before the slot
existed have `secondary_index_root == 0`, which `read_specs` reports
as an empty list — no replay, identical behaviour to the
pre-persistence path. A doc-comment TODO on `secondary_index_root`
records that the legacy interpretation can be dropped once every
stored database has been re-saved.

**Round-trip:** `CREATE BTREE INDEX my_idx ON :Foo(bar); .save;
.quit`, reopen → `.indexes` lists `my_idx` with no manual
re-declaration. For LDBC SF0.1 IC2 with the manual
`CREATE BTREE INDEX ON :Post(creationDate)` saved into the file,
reopen + run goes from 1.11 s (auto-only fallback) to 0.086 s (the
`Or`-merge fires from the replayed DDL index).

**Files changed:** `pager/page.rs` (new `PageType::SecondaryIndex`),
`pager/header.rs` (new `secondary_index_root` slot),
`store/secondary_index_io.rs` (new chain reader / writer),
`store/io.rs::save_graph_with_catalog_and_indexes_atomic`,
`store/lazy.rs` (load on open, pass spec list on save),
`store/mod.rs`.

## 12. Boundary-predicate pushdown into selected paths (ISO §16.6)

**Before:** the pushdown pass treated `PathPattern::Selected` (a
path with a §16.6 prefix) as an opaque boundary — it optimized
inside via `rewrite` but pushed no outer WHERE constraint through it.
So `MATCH ANY SHORTEST (s)-[]->+(t) WHERE s.name = 'A' AND t.name =
'D'` ran the shortest search over *all* `(s, t)` pairs and only then
filtered, even though the endpoint predicates could prune the search.

**After:** `merge_constraints` pushes endpoint constraints into a
`Selected`. The rule depends on the search policy:

- **Mode-only prefix** (`PathSearch::All`, e.g. `ACYCLIC` / `TRAIL` /
  `SIMPLE` with no selection): every matching path is kept, so
  filtering before vs. after is equivalent — push *all* constraints
  in, interior nodes included.
- **Selective prefix** (`ANY` / `SHORTEST`): push **only boundary
  (first/last node) variable** constraints; interior-node predicates
  stay as a post-selection `Filter`.

**Why the split is sound.** A selective search picks paths per
`(source, target)` partition. Restricting the endpoints *before* the
search yields the same per-partition result as filtering *after*
(partitions are independent). But filtering an *interior* node before
the search would change which paths exist, hence which are shortest —
e.g. the shortest `A→D` path might pass through `m ≠ B`, so pushing
`m.name = 'B'` ahead of the search would return a length-3 path that
post-filtering correctly drops. Interior predicates must therefore run
after selection.

**Implementation:** `Constraints` gains `Clone` + `retain_vars(&HashSet)`;
`boundary_node_vars` returns the first/last top-level node vars;
`walk_kinds` registers only boundary vars as pushable for a selective
`Selected`; `merge_constraints` clones the full constraint set for
mode-only prefixes and `retain_vars(boundary)` for selective ones.

**Files changed:** `optimizer/pushdown.rs` (the `Selected` arms of
`walk_kinds` / `merge_constraints`, `retain_vars`, `boundary_node_vars`).
Tests: `selected_shortest_pushes_boundary_value_preds`,
`selected_shortest_keeps_interior_value_pred_as_filter`,
`selected_mode_all_pushes_interior_value_preds` (in `pushdown.rs`).


## 13. Pinned correlated subquery probes (EXISTS / VALUE)

**Before:** a *correlated* `EXISTS { ... }` / `NOT EXISTS { ... }` /
`VALUE { ... }` (one that shares a pattern variable with the outer
row) evaluated its body **once over the whole graph**
(`run_match_chain(body, 0)`), bucketed the rows by the correlation
tuple, and probed a `HashSet` / map per outer row. That amortises when
the outer side has *many* distinct correlation tuples, but the LDBC
anti-join / arg-max shape is the opposite — one person, a handful of
correlated values — so the global scan dominates and never amortises:

- **IC4** `NOT EXISTS { (friend)<-[:hasCreator]-(post2)-[:hasTag]->(tag)
  WHERE post2.creationDate < $start }` (`friend`, `tag` correlated):
  the body materialised every pre-`$start` `(friend, tag)` pair in the
  graph — a fixed ~3 s floor per param on a slow host, enough to blow
  past the cross-system bench's 600 s wall-cap.
- **IC7** `VALUE { (person)<-[:hasCreator]-(m)<-[l:likes]-(liker) ...
  ORDER BY ... LIMIT 1 }` (`person`, `liker` correlated): inside the
  subquery `person` carries no `{id}` filter, so uncorrelated the body
  computes the **entire** like relation — 109 440 triples / param —
  when the real correlated work for one person is ~111 rows.

**After:** when the body is LTJ-pinnable, run it **per outer row with
the correlation variables pinned to that row's node ids** (the same
`try_ltj_with_pins` machinery as the OPTIONAL bind-pushdown, §6, via a
new `pinned_run_multi` that also unwraps the `Filter` carrying the
body's WHERE so the predicate still runs), and **memoise the verdict
by correlation tuple**. The body is evaluated once per *distinct*
correlation tuple, never globally. `exists_body_pinned` returns a
bool; `value_subquery_pinned` projects the single RETURN item + body
ORDER BY + LIMIT 1 (the arg-max). New cache variants
`ExistsCache::CorrelatedPinned { memo }` and
`ValueSubqueryCache { map, pinned: true }` hold the lazily-filled
memo; the original full-materialisation path stays as the fallback
under `ExistsCache::Correlated` / `pinned: false`.

**Correctness — undirected edges.** Pinning **both** endpoints of an
undirected edge (`~[...]~`) does **not** constrain the LTJ to that
specific pair (it yields false positives), so a body containing one
must not take the pinned path. `PathPattern::has_undirected_edge`
gates both probes back to the global materialisation; IC7's
`isNew = NOT EXISTS { (liker)~[:knows]~(person) }` takes this fallback
while its `VALUE` body (all directed) stays pinned. *(This caveat was
found the hard way: the first EXISTS-pin cut shipped without the gate
and produced wrong `isNew` results — the cross-system oracle missed it
because it canonicalises row order. `ic7_test` is the regression
guard; it was absent from the standard sweep at the time.)*

**Falls back to the global materialisation** when: a correlation value
is not a `Node`; the body is OPTIONAL / `Selected` / not
LTJ-decomposable; the body contains an undirected edge; (VALUE only) a
parameter correlation or a grouping/aggregate body needing
`run_aggregated`. Force off with `GQLITE_DISABLE_EXISTS_PIN=1` /
`GQLITE_DISABLE_VALUE_SUBQUERY_PIN=1`.

**Impact (LDBC SF0.1, lazy, median per param; results byte-identical
to the materialise-once reference):**

| Query | Before | After | Speedup |
|---|---|---|---|
| IC4 (`NOT EXISTS` anti-join) | ~3 s floor (600 s-cap timeout on slow host) | ~85 ms | no longer times out |
| IC7 (`VALUE` arg-max subquery) | 1305 ms | ~9 ms | ~145× |

**Files changed:** `runtime/engine.rs` (`eval_exists_correlated`
split into pinned + `build_correlated_set` fallback, new
`exists_body_pinned`; `eval_value_subquery` split into pinned +
materialise fallback, new `value_subquery_pinned`; `pinned_run_multi`),
`syntax/path_pattern.rs` (`has_undirected_edge`). Tests:
`exists_runtime_test`, `value_subquery_test`, `correlated_subquery_test`,
`ic7_test` (the undirected-EXISTS guard).
