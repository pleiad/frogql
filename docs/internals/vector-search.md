# Vector search

## What this is for

The question is: **which nodes satisfy a graph pattern and are also among
the `k` nearest to a query vector?**

The goal is not a vector-database feature. It is to measure three ways of
answering that question against each other, inside GQL, on RDF-shaped
queries — directed edges, no properties, no any-direction — which always
take the LTJ path. Everything here is built so the three arms are
interchangeable and so a latency comparison between them means something.

## Surface

```
NEAREST <k> [ROWS] <var>.<attr> TO <expr> [AS <distvar>]
```

Sits between the MATCH chain and RETURN, so `<distvar>` is in scope for
projection, GROUP BY, and ORDER BY.

```
MATCH (tower)-[:P31]->(:Q12518), (tower)-[:P17]->(country),
      (tower)-[:P18]->(img), (country)-[:P361]->(:Q18)
NEAREST 10 img.emb TO VECTOR(151356, 'emb') AS dist
RETURN tower, img, dist
```

That is the direct translation of the SPARQL magic-predicate idiom
(`?img proc:hnswIterator ("idx" ?vector ?dist)`). It is a clause rather
than a pattern operand because position in a SPARQL basic graph pattern
does not fix evaluation order either, so nothing is lost, and a clause
avoids threading a new variant through every `PathPattern` traversal.

- `<expr>` is a literal list of numbers, or `VECTOR(<node id>, '<attr>')`
  reading a stored vector — "nearest to the embedding of this example".
- `NEAREST`, `ROWS`, and `TO` are soft keywords matched at the grammar
  level (the `TRAIL`/`SHORTEST` treatment), so `to` stays usable as a
  property name. `VECTOR` follows the `ELEMENTS`/`DATE` discipline: only
  the call form is special.

### The two k-modes

| Form | `k` counts |
|---|---|
| `NEAREST k x.a TO q` | distinct bindings of `x` that have at least one match |
| `NEAREST k ROWS x.a TO q` | result rows |

They differ whenever one binding yields several rows, which for a join is
the common case. Both exist because which one a study wants depends on
the question.

## Storage: sidecars

One file per vector attribute, `<db>.vec.<attr>`, outside the `.gdb`.

A node record has no extra area, so per-node vectors would otherwise have
to become ordinary properties — and then every `node_props()` call would
decode a 768-float blob it did not ask for. The vectors and their index
are also built offline and read-only at query time, so keeping them out
of the pager leaves the `.gdb` save path untouched.

Format in `src/vector/sidecar.rs`. The `ids` array is ascending and is
the only mapping from a row to a node, so `row()` is a binary search.

**The fingerprint is load-bearing.** Sidecar ids are graph-internal, and
`save()` renumbers every node when it compacts tombstones away, so a
sidecar built before a delete-then-save silently points at the wrong
nodes. The header carries a hash of the node and edge counts, and a
mismatch means the sidecar is not loaded at all. Second line of defence:
`LazyGraphStore::vectors()` returns `None` while the session holds an
unsaved node insert or delete, the same guard `lookup_node_eq` uses.
Property and label mutations deliberately do not trigger it — they cannot
move a node id, and a vector is not a property.

Build one with `vec_build`:

```bash
cargo run --release --bin vec_build -- movies.gdb --attr emb --input vecs.csv
cargo run --release --bin vec_build -- movies.gdb --attr emb --random 128
```

## The neighbour cursor

Every strategy consumes neighbours through one interface:

```rust
pub trait NnCursor {
    fn next(&mut self) -> Option<(Id, f32)>;
    fn expanded(&self) -> u64;
}
```

"Give me the next nearest", with no `k` fixed up front. That is the whole
reason for a cursor: the in-LTJ and pre-filter strategies cannot know in
advance how deep they must walk before enough candidates also satisfy the
pattern.

- `BruteForceCursor` — exact. The oracle every approximate arm is
  scored against.
- `HnswCursor` — approximate, an unbounded best-first traversal of layer
  0. Note it is an *iterator*, not a top-k call: `next()` takes no `k`
  and never stops, so a caller walks outward from `q` and decides for
  itself when it has enough.

**The HNSW cursor emits on a lookahead.** Before handing back the `i`-th
neighbour it has expanded at least `i + ef` rows, so what it emits is the
minimum over a frontier an ordinary `ef`-bounded search would also have
seen. Emitting straight off the frontier instead returns the
greedy-descent seed after a handful of expansions; on a 400×8 uniform set
that inverts the first two neighbours.

Consequences to keep in mind:

- Distances are only **approximately** non-decreasing. A row closer than
  the one just emitted can sit behind an unexplored part of the graph.
  Threshold cuts therefore take slack (`FROGQL_VEC_TAU_EPS`).
- Rows in a layer-0 component unreachable from the entry point are never
  emitted, so a cursor can end before covering the attribute.
- Driving the cursor to exhaustion costs more than a brute-force scan. It
  pays off only because every strategy stops early.

`NnStream` wraps a cursor in a monotonically growing prefix cache. The
in-LTJ strategy reaches its level once per binding above it and re-walks
the stream each time; rebuilding a cursor per visit would dominate every
other cost. `replays` / `extends` are the counters that prove the cache
is working.

## Where the ranking comes from: `VecSource`

Orthogonal to the strategy. Three values, not an index/no-index boolean,
because "no index" hid two genuinely different algorithms:

| source | how the ranking is produced | exact? |
|---|---|---|
| `Hnsw` | lazily, expanding the proximity graph on demand | no |
| `GlobalSort` | sort the whole attribute once, up front | yes |
| `LocalSort` | sort only the current visit's candidates | yes |

**`Hnsw` and `GlobalSort` share their walk exactly.** Both hand the
in-LTJ level a corpus-wide ranking, and both make every visit re-scan it
from rank 0 testing membership in that visit's candidate set. They differ
only in what it costs to *build* the ranking — and in exactness.

The benchmark shows this directly: at a fixed level, `nn_pops` (the
membership tests) is *identical* between them, while `nn_expanded` (the
cost of producing the stream) is not. On 3 000 items, dim 16, k = 10:

| source | level | `nn_pops` | `nn_expanded` |
|---|---|---|---|
| GlobalSort | 0 | 14 | 3000 |
| Hnsw | 0 | 14 | **77** |
| GlobalSort | 1 | 11804 | 3000 |
| Hnsw | 1 | 11804 | **1306** |

Watch the constant factor, though: each HNSW expansion evaluates ~`m0`
(32) neighbour distances, so it only wins while the prefix it must
materialise stays under roughly `n / m0`. At level 1 above, 1306
expansions is ~42 k distance evaluations against a flat 3 000 — HNSW is
doing *more* work. The crossover moves far out as `n` grows, but it is
real and the benchmark should chart it rather than assume.

**`LocalSort` is the one that walks differently.** It ranks only the
candidates of the visit it is in, so it never touches a node outside the
level and never re-scans anything. `O(|C| log |C|)` per visit, no shared
prefix, no global structure. `tests/vector_strategy_equiv_test.rs` pins
this: `local.nn_pops <= local.candidates_hashed`, while
`global.nn_pops > local.nn_pops`.

Not every strategy can honour every source. Pre-filter has no per-visit
candidate set — its candidates are the whole corpus — so `LocalSort`
there is the same walk as `GlobalSort`, and `stats.arm` reports
`pre+globalsort` so a benchmark row cannot claim otherwise.

## The three strategies

All three enter through `vsearch::run_nearest` and leave as an
`IntermediateResult`, so projection, DISTINCT, ORDER BY, and LIMIT
downstream are identical across arms.

### 1. post-filter (`vsearch/post_filter.rs`)

Run the pattern, then rank what it produced. Under `LocalSort` that is a
distance to every binding the pattern produced — linear in *candidates*,
not in the corpus. Under the two corpus-wide sources it hashes the
candidates and walks the global ranking until `k` are hit, which costs
whatever it takes to reach the `k`-th surviving candidate: small when the
pattern is unselective, and deep when it is selective.

Answers every query shape, so it is also the universal fallback.

### 2. in-LTJ (`vsearch/in_ltj.rs`)

Place the search variable at a chosen VEO level. Each time the search
reaches it, materialise the candidate set — already narrowed by the
partial binding above — hash it, walk the neighbour stream nearest-first,
and descend only into hits.

Enumerating candidates up front and descending in distance order is legal
because `leap` is a pure query against state only `down`/`up` mutate:
draining leaves the iterators exactly as it found them.

**Correctness off level 0.** The level is visited many times and each
visit is internally sorted, but the concatenation is not. `DistThreshold`
holds the `k` best distances accepted so far; a visit stops as soon as the
stream passes it. Because the threshold only ever tightens, a neighbour
rejected once can never be needed later. It is re-read every iteration
rather than hoisted — the recursive descent between two iterations can
accept matches and tighten it.

**The VEO override is applied before filters are placed.** Placement
resolves each filter to the level where its last dependency binds;
reordering afterwards can leave a filter reading a variable that is not
bound yet. That is silently wrong, not merely slow: `check_filters` finds
a binding by scanning the tuple for the var id, and the deeper slots still
hold the previous sibling branch's values.

The requested level is clamped to `VeoOverride::max_level` — just before
the first lonely variable — and the real position is read back, never
assumed. Note this deliberately overrides the lonely-last rule documented
in `veo.rs`; correctness is unaffected (leapfrog is order-agnostic), but
the level axis of the benchmark is partly measuring how much that
heuristic was worth.

### 3. pre-filter (`vsearch/pre_filter.rs`)

Walk the neighbour stream; pin the search variable to each candidate and
re-run the whole pattern. Nearly free to build, since pinning is what the
LTJ already does for correlated EXISTS. Exactly one pattern evaluation per
neighbour examined, so it wins when the first few neighbours also match
and loses badly when the pattern is selective.

A special case of in-LTJ with the search variable at level 0 — but only
at level 0. Placing it deeper is something only the in-LTJ arm can do.

## What to measure

**`nn_pops` per accepted result.** With a selective pattern the
interleaving arms walk a proximity graph built over the *whole* corpus, so
reaching a candidate that also satisfies the pattern can cost a large
fraction of layer 0. This is the classic filtered-ANN failure mode.
Post-filter degrades gracefully exactly where those two blow up.

`VecStats` also records **which arm executed**, not which was requested: a
precondition miss falls back, and reporting the requested arm would lie.

```bash
cargo run --release --bin vec_bench -- --items 50000 --dim 128 --ks 1,10,100 --levels 0,1,2
```

CSV columns: `items,dim,k,mode,selectivity,strategy,index,level,median_ms,
recall,nn_pops,nn_expanded,pattern_runs,ltj_visits,candidates,rows`.

## Equivalence

`tests/vector_strategy_equiv_test.rs` is what makes the benchmark
legitimate. Under the **exact** cursor all three strategies return
identical answers across VEO levels, `k` values, both k-modes, and four
query shapes. If they could disagree, comparing their latency would be
comparing three different queries.

Under HNSW recall genuinely differs by arm — that is a result, not a bug —
so what is asserted there is only that no arm invents a row the pattern
does not produce. Both exact sources are in the equivalence sweep, at
every level. The suite also checks the in-LTJ arm actually ran rather
than falling back, which would make the equivalence pass for the wrong
reason.

## Environment

| Var | Effect |
|---|---|
| `FROGQL_VEC_STRATEGY=post\|pre\|inltj` | which strategy to run (default `post`) |
| `FROGQL_VEC_SOURCE=hnsw\|localsort\|globalsort` | where the ranking comes from (default `hnsw`) |
| `FROGQL_VEC_LEVEL=<n>` | VEO position of the search variable; in-LTJ only, clamped |
| `FROGQL_VEC_TAU_EPS=<f>` | relative slack on the threshold cut (default 0) |
| `FROGQL_DISABLE_VECTORS` | ignore every sidecar; queries see no vector attribute |
| `FROGQL_DEBUG_VEC` | print the executed arm and its counters |

`vec_bench` sets these programmatically via `Runtime::set_vec_cfg`, so
its sweeps do not depend on process-global state.

## Known limits

- **Approximate arms disagree by design.** Only the exact cursor is
  pinned to equality.
- **A missing or suspended sidecar yields no rows**, not the unfiltered
  pattern. "Among the `k` nearest" cannot be satisfied by anything when
  there are no vectors, and returning the pattern would be silently wrong.
- **Vectors are plain float lists**, not a `SimpleType` terminal. A new
  terminal would ripple through the whole lattice to distinguish
  something no part of the language needs to distinguish.
- **The fingerprint is coarse**: it will not catch a delete plus an
  equal-sized insert followed by a save. The in-session DML guard covers
  that while the session lasts.
- **`k = 0`** is legal and produces nothing; the typechecker warns.
