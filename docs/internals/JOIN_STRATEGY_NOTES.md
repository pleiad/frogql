# Join Strategy: From Hash-Join to Leapfrog Triejoin

This document explains how GQLite evaluates queries that involve multiple
patterns joined together, why the naive approach doesn't scale, and how
Leapfrog Triejoin (LTJ) solves the problem. It is based on the paper
CompactLTJ (Arroyuelo et al., VLDBJ 2025), with a reference C++
implementation in `../cltj/`.


## 1. What is a join in GQL?

A join happens when a query mentions the same variable in two or more
patterns. The runtime must find values for that variable that satisfy
all patterns simultaneously.

Consider this graph (a social network where people follow each other):

```
  Alice ──follows──► Bob ──follows──► Carol
    ▲                                   │
    └────────── follows ────────────────┘
```

The query "find triangles" asks for three people where each follows the next:

```
(a) -[:follows]-> (b), (b) -[:follows]-> (c), (c) -[:follows]-> (a)
```

Here, variable `b` appears in the first and second pattern, and `a` appears
in the first and third. The runtime must find values of `a`, `b`, `c` that
satisfy all three constraints at once. That's a multi-way join.


## 2. The naive approach: pairwise hash-join

The simplest strategy is to evaluate two patterns at a time:

```
Step 1: Find all (a,b) pairs where a follows b
        → produces a big table of pairs

Step 2: Find all (b,c) pairs where b follows c
        → produces another big table

Step 3: Join the two tables on shared variable b
        → for each (a,b) in step 1, find matching (b,c) in step 2
        → produces triples (a,b,c)

Step 4: Filter: keep only triples where c follows a
```

This works, but has a fatal flaw: **intermediate blowup**.

### The blowup problem, illustrated

Suppose each person follows 100 others (degree 100). Let's count:

```
Step 1: (a,b) pairs = 100 per person × N persons = 100N rows
Step 2: (b,c) pairs = 100 per person × N persons = 100N rows
Step 3: Join on b = for each (a,b), match with all (b,c)
        = up to 100 × 100 = 10,000 triples per value of b
        = 10,000N rows in the intermediate result
Step 4: Filter c→a: most of these 10,000N rows get thrown away
```

We built a table with 10,000N rows only to discard most of it. For a
4-clique (6 sub-queries), the intermediate tables grow to millions or
billions of rows. In our benchmarks, 4-clique simply hangs.

### Benchmark results with pairwise hash-join (before LTJ)

| Query shape | Time (100K edges, limit=1000) |
|-------------|-------------------------------|
| 3-clique    | 0.57s                         |
| 4-cycle     | 4.15s                         |
| 3-path      | 6.33s                         |
| 2-tree      | 101.8s (timeout)              |
| 4-path      | 159.8s (timeout)              |
| 4-clique    | never finishes                |


## 3. Leapfrog Triejoin: the idea

LTJ avoids intermediate blowup by binding variables **one at a time**
and checking all constraints immediately. Instead of "find all pairs,
then filter", it does "pick a candidate for `a`, then for `b`, then
check right away if it works".

### Triangle example, step by step

Query: "Find all triangles `(a)->(b), (b)->(c), (c)->(a)`"

```
for each candidate a:
  for each b such that a→b exists:
    for each c such that b→c AND c→a both exist:
      ← this is the key: we check c→a immediately, not later
      emit (a, b, c)
```

The critical difference: when considering candidates for `c`, LTJ already
knows that `c` must appear in both `out(b)` (from pattern 2) and `in(a)`
(from pattern 3). It **intersects** these two lists right now, instead of
building a huge table and filtering later.

If `b` has 100 outgoing edges and `a` has 5 incoming edges, the intersection
produces at most 5 candidates for `c`, not 100. No blowup.

### How the intersection works: leapfrog

Suppose we need to find values of `c` that appear in both list A (sorted)
and list B (sorted):

```
List A (out(b)):  [3, 7, 12, 15, 20, 25, 30]
List B (in(a)):   [5, 12, 18, 25, 40]
```

The leapfrog algorithm alternates between the two lists:

```
1. Start: A seeks ≥ 0  → finds 3.   B seeks ≥ 3  → finds 5.   (3 ≠ 5)
2. Rotate: A seeks ≥ 5  → finds 7.   B seeks ≥ 7  → finds 12.  (7 ≠ 12)
3. Rotate: A seeks ≥ 12 → finds 12.  B seeks ≥ 12 → finds 12.  (12 = 12) ← match!
4. Next:   A seeks ≥ 13 → finds 15.  B seeks ≥ 15 → finds 18.  (15 ≠ 18)
5. Rotate: A seeks ≥ 18 → finds 20.  B seeks ≥ 20 → finds 25.  (20 ≠ 25)
6. Rotate: A seeks ≥ 25 → finds 25.  B seeks ≥ 25 → finds 25.  (25 = 25) ← match!
7. Next:   A seeks ≥ 26 → finds 30.  B seeks ≥ 30 → finds 40.  (30 ≠ 40)
8. Rotate: A seeks ≥ 40 → end of A.  Done.
```

Result: `c ∈ {12, 25}`. We found the intersection without scanning
every element. Each "seek" is a binary search: O(log n).

With 3 or more lists, the algorithm rotates through all of them, always
seeking forward to at least the current maximum. It converges quickly
because each seek jumps ahead, skipping large portions of each list.


## 4. From graph queries to triples

LTJ was originally designed for RDF databases, where data is stored as
`(subject, predicate, object)` triples. We adapt it to property graphs
by treating each directed edge as a triple:

```
Edge: Alice ──follows──► Bob
Triple: (Alice, follows, Bob)

Edge: p1 ──Transfer──► p2
Triple: (p1, Transfer, p2)
```

In general: `(source_node, edge_label, target_node)`.

### Query transformation

The runtime transforms GQL patterns into triple patterns. Each variable
in the query becomes a variable in a triple, and each concrete label
becomes a constant.

**Example: `(a)-[:Transfer]->(b), (b)-[:Transfer]->(c)`**

```
Triple 1: (a, Transfer, b)    ← "a" sends a Transfer to "b"
Triple 2: (b, Transfer, c)    ← "b" sends a Transfer to "c"
```

Variable `b` is shared between both triples. LTJ will bind `b` to a
value that satisfies both.

**Example: `(x)->(y)->(z)` (no label, concat chain)**

A concatenation of edges is also a join. The middle node `y` must be
the target of the first edge and the source of the second. This is the
same as a comma-join on `y`:

```
Triple 1: (x, _p0, y)    ← some edge from x to y (any label)
Triple 2: (y, _p1, z)    ← some edge from y to z (any label)
```

Variables `_p0` and `_p1` are fresh internal variables for the unknown
labels. LTJ treats them as additional variables to bind, which works
naturally: it just means "find any label that fits".

**Example: anonymous nodes `()-[]->()-[]->()`**

Nodes without variable names get fresh internal names (`_ltj_0`, etc.):

```
Triple 1: (_ltj_0, _ltj_2, _ltj_1)
Triple 2: (_ltj_1, _ltj_3, _ltj_4)
```

The internal names are excluded from the final result.


## 5. The 6 sorted orderings

LTJ needs to search triples efficiently by any combination of components.
Sometimes it knows the source and wants the target; sometimes it knows
the target and wants the source; sometimes it knows the label and wants
both endpoints.

To support this, we store 6 copies of all triples, each sorted by a
different permutation:

```
Name   Sorted by           Used when you know...
────   ─────────           ──────────────────────
SPO    (src, label, tgt)   src, then label → find targets
SOP    (src, tgt, label)   src, then tgt → find labels
POS    (label, tgt, src)   label, then tgt → find sources
PSO    (label, src, tgt)   label, then src → find targets
OSP    (tgt, src, label)   tgt, then src → find labels
OPS    (tgt, label, src)   tgt, then label → find sources
```

Think of each ordering as a phone book sorted differently. If you know
the last name, the standard phone book works. If you know the city first,
you need a different sorting. With 6 orderings, any prefix query is a
simple binary search.

### Concrete example (fraud graph, 5 edges)

```
Edges:  p1──Transfer──►p2,  p2──Transfer──►a2,  a2──Transfer──►a1,
        a1──Transfer──►p1,  a1──Foo──►d1

Using internal IDs: p1=2, p2=3, a2=1, a1=0, d1=4
                    Transfer=0, Foo=1

SPO ordering (sorted by src, then label, then tgt):
  (0, 0, 2)   ← a1 ──Transfer──► p1
  (0, 1, 4)   ← a1 ──Foo──► d1
  (1, 0, 0)   ← a2 ──Transfer──► a1
  (2, 0, 3)   ← p1 ──Transfer──► p2
  (3, 0, 1)   ← p2 ──Transfer──► a2
```

To find "all targets of Transfer edges from node 0 (a1)":
binary search for prefix (0, 0, ?) → finds (0, 0, 2) → target is p1.

To find "all sources that reach node 0 (a1)", we need the OSP ordering
(sorted by tgt first): binary search for prefix (0, ?, ?) → finds all
edges ending at a1.


## 6. The algorithm in action: full worked example

Let's trace LTJ on a triangle query over a small graph.

**Graph** (directed edges only):
```
0 → 1,  0 → 2,  1 → 2,  1 → 0,  2 → 0,  2 → 1
```

**Query**: `(a)->(b), (b)->(c), (c)->(a)` — find all triangles.

**Triples** (labels omitted for simplicity):
```
Triple 1: (a, *, b)    ← a→b
Triple 2: (b, *, c)    ← b→c
Triple 3: (c, *, a)    ← c→a
```

**Variable order (VEO)**: a, b, c (all non-lonely).

**Search**:

```
seek(a, ≥0):
  Triple 1 needs a at position S → leap in SPO: first src ≥ 0 → a=0  ✓
  Triple 3 needs a at position O → leap in OSP: first tgt ≥ 0 → a=0  ✓
  Both agree: a=0

  down(a=0) in all iterators containing a

  seek(b, ≥0):
    Triple 1 needs b at position O, with a=0 fixed at S
      → use SOP ordering, range narrowed to src=0: targets are {1, 2}
      → leap ≥ 0 → b=1  ✓
    Triple 2 needs b at position S
      → use SPO ordering: first src ≥ 1 → b=1  ✓
    Both agree: b=1

    down(b=1) in all iterators containing b

    seek(c, ≥0):
      Triple 2 needs c at position O, with b=1 fixed at S
        → targets of node 1: {0, 2}
        → leap ≥ 0 → c=0  ✓
      Triple 3 needs c at position S, with a=0 fixed at O
        → sources reaching node 0: {1, 2}
        → leap ≥ 0 → c=1  (0 is not in {1,2})
      Mismatch! c=0 vs c=1. Rotate:
        Triple 2 seeks ≥ 1 → c=2
        Triple 3 seeks ≥ 2 → c=2  ✓
      Both agree: c=2

      EMIT (a=0, b=1, c=2) ← triangle found!

      seek(c, ≥3): both lists exhausted → no more c

    up(b)

    seek(b, ≥2):
      Triple 1: targets of 0 ≥ 2 → b=2
      Triple 2: sources ≥ 2 → b=2
      Both agree: b=2

      down(b=2)

      seek(c, ≥0):
        Triple 2: targets of 2 → {0, 1}. leap ≥ 0 → c=0
        Triple 3: sources to 0 → {1, 2}. leap ≥ 0 → c=1
        Mismatch. Rotate:
          Triple 2 seeks ≥ 1 → c=1.
          Triple 3 seeks ≥ 1 → c=1.  ✓
        Both agree: c=1

        EMIT (a=0, b=2, c=1) ← another triangle!

      up(b)

    seek(b, ≥3): exhausted

  up(a)

  seek(a, ≥1):
    ... continues for a=1, a=2, finding all triangles
```

Each "seek" is a binary search in a sorted array. No intermediate tables.
No blowup.


## 7. When LTJ activates and when it doesn't

The runtime tries LTJ automatically whenever it encounters a join or
concatenation of directed edges. If the pattern contains elements that
can't be decomposed into triples, it falls back to the original
pairwise hash-join.

**LTJ handles:**
- Comma-joins: `(a)->(b), (b)->(c), (c)->(a)`
- Long chains: `(a)->(b)->(c)->(d)->(e)`
- Combinations: `(a)->(b)->(c), (c)->(d), (a)->(d)`
- Labeled edges: `(a)-[:Transfer]->(b)` (label becomes a constant)
- Unlabeled edges: `(a)->(b)` (label becomes a free variable)
- Anonymous nodes: `()-[]->()-[]->()` (fresh internal variables)
- Left-direction edges: `(a)<-[e]-(b)` — the extractor swaps endpoints
  so the emitted triple is `(b, p, a)` (forward).
- Undirected edges: `(a)~[e]~(b)` — the `TripleIndex` stores each
  undirected edge as both `(s,p,t)` and `(t,p,s)` with the same
  `edge_id`, so a forward lookup catches it from either endpoint.
  Result reconstruction recovers the `EdgeUndirectional` variant
  via `graph.edge_path_value(eid)`.

**LTJ does NOT handle (falls back to hash-join):**
- Any-direction edges (no tilde): `(a)-[e]-(b)` — distinct from
  undirected; matches both directed senses without the schema
  guarantee of an undirected edge.
- Unions: `(a)->(b) | (a)->(c)`
- Repetitions: `(a)->(b){1,3}`
- WHERE expressions with property comparisons (labels ARE handled)

The fallback is safe: results are identical, only performance differs.

### Repetitions: a future opportunity

A pattern like `((a)-[:T]->(b)){1,2}` means "1 or 2 consecutive T-edges".
This could be unrolled into separate LTJ executions:

```
k=1: one triple  (a, T, b)
k=2: two triples (a, T, _m), (_m, T, b)
```

Each execution runs without intermediate blowup. The results are unioned,
and the edge variables are collected into `PathValue::List` as the runtime
expects. This is not implemented yet but would extend LTJ to handle fixed
repetitions.


## 8. Benchmark results: before and after

All measurements on soc-LiveJournal1 (100K edges), limit=1000 results.

| Query shape    | Hash-join (before) | LTJ (after) | Speedup |
|----------------|-------------------|-------------|---------|
| 1-tree         | 0.28s             | 0.039s      | 7x      |
| 2-comb         | 0.40s             | 0.039s      | 10x     |
| 3-clique       | 0.57s             | 0.041s      | 14x     |
| 3-cycle        | 0.57s             | 0.041s      | 14x     |
| 2-3-lollipop   | 1.56s             | 0.040s      | 39x     |
| 4-cycle        | 4.15s             | 0.041s      | 101x    |
| 3-path         | 6.33s             | 0.040s      | 158x    |
| 3-4-lollipop   | 23.1s             | 0.040s      | 577x    |
| 2-tree         | 101.8s            | 0.039s      | 2610x   |
| 4-path         | 159.8s            | 0.039s      | 4097x   |
| **4-clique**   | **never finished** | **0.043s** | **∞**   |

The speedup grows with query complexity because LTJ avoids exponential
intermediate blowup. Simple queries (1-tree, 2-comb) see modest gains
because the old approach didn't blow up on them.


## 9. Implementation overview

The implementation lives in `src/runtime/ltj/` with 6 files:

```
triple_index.rs    Build 6 sorted orderings from the graph.
                   Provides leap() and range_for_key() via binary search.

iterator.rs        One iterator per triple pattern. Navigates the right
                   ordering based on which variables are already bound.
                   Handles constant labels by pre-fixing them in the stack.

veo.rs             Decides the order to bind variables. Non-lonely variables
                   (shared between triples) go first. Two protocols: a
                   materialised order fixed before the search (VeoSimple,
                   the default), and an adaptive one that picks the next
                   variable per binding (AdaptiveVeo, FROGQL_VEO=adaptive).

algorithm.rs       The recursive search: bind variable j, seek intersection,
                   descend, recurse for j+1, backtrack. Evaluates filters
                   (node labels) at each level before descending.

pattern_extract.rs Transforms a PathPattern tree into flat triple patterns.
                   Flattens Concat chains, extracts Join children, creates
                   fresh variables for anonymous nodes. Entry point: try_ltj().

engine.rs          Integration: run_join() and run_concat_pattern() call
(modified)         try_ltj() first. If it returns None, fall back to the
                   original strategy.
```

## 10. Remaining improvements

1. **Adaptive VEO**: **DONE, behind `FROGQL_VEO=adaptive`** (issue #101) —
   see section 11 for what it costs and what it does not buy here.

2. **Cache the TripleIndex**: currently rebuilt for each query. Should be
   built once per graph and reused.

3. **Unroll fixed repetitions**: `{1,m}` can be decomposed into m separate
   LTJ runs (see section 7).

4. **Compact tries**: replace sorted Vec with LOUDS bitvectors for space
   efficiency on very large graphs (billions of edges).

5. **Handle any-direction edges (`-[e]-`)**: **DONE** — pure *and* mixed
   any-direction chains and comma-joins (issue #71). The team adopted ISO
   bag semantics; the LTJ base case now fans out one row per physical edge
   at the bound `(s, L, o)` unconditionally (fixing the directed
   parallel-edge collapse too), and a mirrored index (`from_graph_anydir`)
   stores every edge in both senses so a single forward triple matches
   either orientation in one LTJ pass. Verified against a hand-computed ISO
   oracle (`tests/{iso_multiplicity,anydir_iso}_test.rs`), behind
   `FROGQL_DISABLE_ANYDIR_LTJ`. Bounded unused-edge any-direction
   *repetitions* (`(a)-[]-{1,3}(b)`) also reach the mirror: `unroll_repeat`
   unrolls them into flat any-direction arms. Full rationale in
   `anydir-ltj-plan.md`. All any-direction paths (mixed LTJ, seeded
   repetition, adjacency fallback) are ISO bag-consistent — verified by
   `tests/anydir_path_consistency_test.rs`. **Mixed** directed+any-direction
   chains also run through LTJ now (`try_ltj_mixed`, per-triple index
   routing): each triple's iterator queries the plain or mirrored index by
   its `EdgeKind`, and the leapfrog joins across both (global node ids,
   shared label ids). Measured ~4.7× faster / ~2× less RSS than the fallback
   on a fraud-DB mixed query. Nothing any-direction is left on the fallback
   except Unions / non-unrollable repetitions.

### Caveat: the Ring's bidirectionality ≠ undirected edges

A recurring slip when reading the Ring / CLTJ papers: the ring lets you
traverse the *attributes* `s/p/o` in either order (which you bind first in
the variable elimination), so one compact structure serves all 6 orderings.
That is **access-order** bidirectionality — it is *not* the same as an
edge's semantic orientation. The stored triples stay directed: a directed
edge `a→b` is the single fact `(a, L, b)`, reachable via OSP by binding `b`
first, but the reverse *fact* `(b, L, a)` is never manufactured by re-
ordering. Only genuinely undirected edges are stored in both senses
(`triple_index.rs`, `push_both`). So the ring buys cheap reverse *access*,
never any-direction *matching*; conflating the two is how `-[e]-` looks
"already supported" when it is not.


## 11. Adaptive VEO (the default; `FROGQL_VEO=simple` opts out)

`VeoSimple` fixes the whole variable order before the search, from a
syntactic weight: an equality filter counts as one binding, a range as a
tenth of the index, a label as a quarter, nothing as the whole thing. It
is a guess about the query, made once, and never a measurement of what the
index holds.

`AdaptiveVeo` (a port of `cltj/include/veo/veo_adaptive.hpp`) decides one
variable at a time, from four hooks the search drives:

- **`next(j)`** — take the lightest still-unbound variable. Lonely
  variables are held back and returned last, exactly as `VeoSimple` sorts
  them last.
- **`down`** — a value was bound and the iterators descended: ask every
  iterator holding an unbound *neighbour* for the subtree it now reports,
  take the minimum, and lower the neighbour's weight if it improved.
  Overwritten weights are recorded in a version.
- **`up`** — pop the version and restore. This is what makes it correct
  under backtracking.
- **`done`** — the level is exhausted; return its variable to the pool.

The cardinality is `LtjIterator::subtree_size`, the reference's
`trait_size`: `usize::MAX` with nothing fixed, the real subtree at the
current node otherwise. Constants count as fixed, so a labelled edge
reports a genuine number before any variable binds. The array
representation answers with the width of its current range (index entries,
parallel edges included); the compact trie answers with the span between
its leftmost and rightmost leaf (distinct `(s, p, o)`). Both are O(1) and
both are estimates feeding a comparison.

Two things are kept from `VeoSimple` rather than ported:

- **Lonely-last.** A variable in a single triple is enumerated, not
  intersected; elevating it trades a structural intersection for a scan.
- **The syntactic weight as a ceiling** (`min` with the measured size).
  The index knows nothing about a pushed-down `x.id = K`, which really does
  leave one binding, and this engine works hard to push exactly those down.

### Combining the measured size with the syntactic weight

The measured size is a property of the **triple**, not of the variable.
With only the label fixed, both endpoints of `(a)-[:L]->(b)` report the
same subtree, so at the root it discriminates *across* triples and never
*within* one. The weight is therefore `min(syntactic, measured)` compared
**with the syntactic weight as the tiebreak**, and the tiebreak is
load-bearing rather than cosmetic.

Without it, LDBC IC5's outer join — the single triple
`(otherPerson)<-[:hasMember]-(forum:Forum)` — measures 123 268 on both
sides. `min` then overwrote two genuinely different syntactic weights
(1 492 038 and 373 009; `forum` is the side carrying the `:Forum` filter)
with the same number, turned a real ranking into a tie, and let the tie
fall to variable id. The unfiltered 123 268-node side bound first and the
whole query ran 1.36× slower. `tests/veo_adaptive_test.rs` and
`adaptive_breaks_a_measured_tie_on_the_syntactic_weight` pin it.

### A forced order skips the adaptive VEO entirely

`veo::order_is_forced` is true when the pattern's shape leaves one
possible order: at most one non-lonely variable (no pick to make) and at
most one lonely one (nothing to sort it against). `AdaptiveVeo` then
provably produces `VeoSimple`'s order, so `pattern_extract` builds the
cheap one instead.

This matters because the LTJ runs once per outer row inside a correlated
subquery or an OPTIONAL pushdown, so VEO construction is a per-*row* cost.
IC8 issues 148 323 runs of a `(connector, lonely)` pattern; both orders
visit the same 600 candidates, and building the adaptive VEO for them was
20% of the query for an order it could not change. IC5 issues 197 489 runs
of a single-variable pattern, same story.

### What it costs

LDBC IC medians on `bench/data/ldbc-sf0.1.gdb`, lazy backend, 15 params ×
5 iters, release build, two independent repetitions. Every IC returns the
**same row counts** under both orders — the strongest equivalence check
available outside the differential suite.

| IC | simple | adaptive | ratio (rep 1 / rep 2) |
|---|---|---|---|
| IC1 | 28.7 ms | 28.6 ms | 1.00 / 0.99 |
| IC2 | 11.7 ms | 11.6 ms | 0.99 / 1.00 |
| IC3 | 10.1 ms | 10.1 ms | 1.00 / 0.99 |
| **IC4** | **120.5 ms** | **0.7 ms** | **0.01 / 0.01** |
| IC5 | 185.4 ms | 181.5 ms | 0.98 / 1.01 |
| IC6 | 1.3 ms | 1.3 ms | 1.00 / 1.00 |
| IC7 | 21.8 ms | 22.3 ms | 1.02 / 1.01 |
| IC8 | 36.7 ms | 38.4 ms | 1.05 / 1.09 |
| IC9 | 1649.5 ms | 1830.6 ms | 1.11 / 1.10 |
| IC11 | 1.7 ms | 0.9 ms | 0.53 / 0.53 |
| IC12 | 96.9 ms | 90.6 ms | 0.93 / 1.02 |
| IC13 | 19.7 ms | 19.8 ms | 1.01 / 1.01 |
| IC14 | 500.2 ms | 481.2 ms | 0.96 / 0.96 |

**IC4 is ~170× faster**, and the visit counts say why: the same 386 LTJ
runs descend into 43 264 candidates under `simple` and 1 783 under
`adaptive`. IC4's cost is a correlated `NOT EXISTS` anti-join run once per
distinct correlation tuple with the outer variables pinned; the pins make
each triple's subtree genuinely different, which is exactly where the
measurement carries information. Its dominant pattern weighs
`friend(syn=373 009 meas=1)` against `post(syn=149 203 meas=286 744)`:
the syntactic weight ranks them backwards, and the measurement flips it.
IC11 is the same shape, smaller.

**IC9 is the honest regression.** 30 LTJ runs, so construction cost is not
in it, and the visits barely move (2 540 806 → 2 521 095). Its
3-variable pattern weighs `_ltj_0(syn=1 492 038 meas=10)` against
`otherPerson(syn=373 009 meas=28 146)`, so adaptive binds `_ltj_0` first.
Ten candidates against 28 146 looks like the better opening and is not:
the same number of descents come out costing more, because a descent is
not a unit of work — which iterators leapfrog at which level changes with
the order. Reporting visits alongside wall clock is what makes that
visible, and `FROGQL_DEBUG_VEO=1` prints both.

**IC8's residual 1.05–1.09× is the switch, not the order.** Both orders
are identical there (600 visits each) and the adaptive VEO is not even
built. The gap is the `std::env::var("FROGQL_VEO")` that
`adaptive_requested` reads once per LTJ run — 148 323 lookups, each
allocating a `String`. Running the *simple* order with `FROGQL_VEO=simple`
set reproduces it exactly (38.6 ms against 36.4 ms with the variable
unset), which is how it was identified. Every other kill switch on this
path is read the same way; caching it would need a mechanism the
in-process differential tests can still flip.

The default path pays nothing for the hooks. `next` / `down` / `up` /
`done` are trait calls with no-op bodies for a materialised order, and an
A/B against the commit before this one measures IC2 1.004×, IC5 1.034×,
IC9 1.000×.

The honest summary: **adaptive wins where the pins have already made the
triples' cardinalities differ, and is a coin flip everywhere else.**

It is the default on that balance — a 170× win on IC4 against a 10–15%
loss on IC9, with the full test sweep returning identical results under
either order (1602 tests, both ways). `FROGQL_VEO=simple` stays as the
escape hatch and as what the differential test A/Bs against.

What that decision rests on is one dataset. LDBC SF0.1 is 327 K nodes with
short patterns; the RDF corpus this engine also targets is 617 M edges
with six-join patterns whose variables are mostly lonely, and the
per-binding re-weighing is paid once per binding there too. That has not
been measured.

### Not wired to the vector-search arms

The in-LTJ vector strategies stay on the materialised order. They *set*
the level their search variable binds at (`VeoOverride`), which is the
axis those benchmarks measure, and `Memo`'s phase 2 replays a stored
prefix by reading `var_at` for levels the search has already left —
something an order decided during the search cannot answer.
