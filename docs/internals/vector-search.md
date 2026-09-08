# Vector search

## What this is for

The question is: **which nodes satisfy a graph pattern and are also among
the `k` nearest to a query vector?**

The goal is not a vector-database feature. It is to measure several ways
of answering that question against each other, inside GQL, on RDF-shaped
queries — directed edges, no properties, no any-direction — which always
take the LTJ path. Everything here is built so the arms are
interchangeable and so a latency comparison between them means something.

## The algorithms being compared

Five. Four as originally specified, plus a second in-LTJ algorithm that
came out of review — the original in-LTJ turned out to describe one of
two genuinely different ways to combine the ranking with the join, and
both are worth measuring.

| # | algorithm | metric index? |
|---|---|---|
| 1 | **post-filter** — run the pattern, then rank what it produced | with or without |
| 2 | **interleave** — at VEO level `x`, hash that level's candidates and iterate the neighbour index nearest-first, descending into hits with backtracking | with or without |
| 3 | **global pre-sort** — rank the whole attribute once, then at level `x` walk that ranking testing membership in the candidates | exact by construction |
| 4 | **pre-filter** — for each neighbour the index yields, substitute it as a constant for `x` and re-run the query | with or without |
| 5 | **memo** — collect every prefix that reaches `x`, keyed by `x`; then walk the ranking **once**, globally, and resume the join below `x` only for what it accepts | with or without |

They are not five independent code paths. Two axes generate them:
`Strategy` (post / interleave / memo / pre) crossed with `VecSource`
(where the nearest-first ranking comes from). Algorithms 2 and 3 share
the same `search()` and differ only in the source, which is exactly why
the source is a first-class axis rather than an index/no-index flag.

### Why 2 and 5 are separate names and not a flag

Both bind `x` at a VEO level. They differ in **when the ranking is
consulted**, and that is the axis under study.

The search reaches level `x` once per binding of everything above it.
`interleave` walks the ranking inside each of those visits. Each visit is
internally sorted, but the concatenation of the visits is not: the
nearest surviving candidate overall may live under the last prefix
enumerated, so no visit can stop early on distance until a later one has
already contributed. The ranking therefore gets re-walked, once per
visit.

`memo` hoists the walk out. Phase 1 runs the join down to `x` and stops,
recording each surviving candidate against **all** the prefixes that
reach it — one node is reachable by many paths, so a key holds several
prefixes and phase 2 must resume every one. Phase 2 walks the ranking
once and resumes only what it accepts, so the distance cut becomes
global: it ends the search rather than trimming each visit.

| | `interleave` | `memo` |
|---|---|---|
| ranking walked | once per visit | once, globally |
| distance cut | per visit | global |
| prefix materialised | never | fully |
| a miss costs | a membership test per visit | one hash lookup, ever |

| # | `FROGQL_VEC_STRATEGY` | `FROGQL_VEC_SOURCE` | `stats.arm` |
|---|---|---|---|
| 1 without index | `post` | `localsort` | `post+localsort` |
| 1 with index | `post` | `hnsw` | `post+hnsw` |
| 2 without index | `interleave` | `localsort` | `interleave+localsort` |
| 2 with index | `interleave` | `hnsw` | `interleave+hnsw` |
| 3 | `interleave` | `globalsort` | `interleave+globalsort` |
| 4 without index | `pre` | `globalsort` | `pre+globalsort` |
| 4 with index | `pre` | `hnsw` | `pre+hnsw` |
| 5 without index | `memo` | `localsort` | `memo+localsort` |
| 5 with index | `memo` | `hnsw` | `memo+hnsw` |
| 5 global pre-sort | `memo` | `globalsort` | `memo+globalsort` |

`inltj` remains an accepted spelling of `interleave`: it was the only
in-LTJ algorithm when the flag was introduced, and recorded runs still
use it.

`vec_bench` sweeps all eleven itself and sets them programmatically; the
env vars are for the REPL and one-off runs. Read `stats.arm` (or the
`strategy` / `source` columns of the CSV) rather than the request — an
arm that could not be honoured degrades and says so.

Eleven runnable arms: `post` × 3 sources, `interleave` × 3, `memo` × 3,
and `pre` × 2 (pre-filter has no per-visit candidate set, so `localsort`
there is the same walk as `globalsort`). `post+globalsort` is not in the
original four; it is a control — post-filtering that reads a corpus-wide
ranking instead of ranking only what the pattern produced.

Algorithm 3 is the one predicted to be bad, and the prediction is about
level `x > 0`: see *Where the ranking comes from* below. Algorithm 5 is
the fix for that prediction; whether the fix pays is measured in
*Results*.

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

## Correlated `NEAREST`: one ranking per anchor

Everything above answers "which nodes satisfy this pattern and are among
the `k` nearest to **a** query vector" — one vector, fixed before the
search starts. That is why `resolve_spec` can evaluate `<expr>` against an
empty assignment.

A different question shows up in the IMGpedia workload, written in the
MillenniumDB benchmark dialect as `?v00 k50 ?v11`:

```
MATCH (v10:img)-[:P69]->(v00:img), (v10)-[:P6]->(:img {id: 100980834}),
      (v01:img)-[:P926]->(v21:img), (v01)-[:P69]->(v11:img)
NEAREST 50 v11.hog TO VECTOR(v00, 'hog') AS dist
RETURN DISTINCT v00.id, v11.id, dist ORDER BY v00.id, dist
```

"for **each** `v00` the pattern binds, which `v11` are among its 50
nearest". The query vector is a function of the row, so this is a
similarity **join**: as many rankings as there are distinct anchors.

The surface needs nothing new — `VECTOR(v00, 'hog')` was already legal
syntax and already typechecked, since `check_nearest` checks `<expr>`
against the environment the pattern binds. What was missing was the
evaluation. Until `runtime/vsearch/correlated.rs`, such a clause parsed,
typechecked, then returned **zero rows in silence**: the query vector
resolved against an empty assignment, `v00` was an unbound reference, and
an unresolvable query vector is (correctly, for the uncorrelated case) an
empty answer.

### The plan

`Runtime::run_match_chain_or_nearest` asks `correlated::anchor_vars`
whether `<expr>` names a pattern variable, minus the search variable
itself. A non-empty answer takes the correlated arm; an empty one takes
the four strategies exactly as before.

The arm runs the pattern **once**, partitions its rows by the anchor
binding, and ranks each partition against its own query vector.
Partitioning after a single run is the whole point: the anchors are not
known until the pattern has run, so re-running it pinned per anchor —
the shape `exists_body_pinned` uses — would pay for the pattern again for
every anchor.

Ranking inside a partition delegates to `post_filter::rank_buckets`, so
the correlated and uncorrelated arms cannot drift apart on what "the `k`
nearest of this candidate set" means. `FROGQL_VEC_SOURCE` selects the
per-partition stream with the same three values and the same cost story,
scaled down to a partition:

| source | per partition | when it wins |
|---|---|---|
| `localsort` | ranks that partition's candidates, exact | a selective pattern; never touches a node outside the match |
| `hnsw` | walks the corpus proximity graph, testing membership | the partition is a large fraction of the corpus |
| `globalsort` | sorts the **whole attribute**, exact | oracle only — it repeats that sort per partition |

`stats.arm` reports `correlated+<source>` and `stats.anchor_groups`
counts the partitions, so a benchmark row never claims an arm that did
not run. Pinned by `tests/correlated_nearest_test.rs`, which also asserts
the single pattern run and that a constant query vector still takes the
ordinary arm.

### Both in-LTJ arms have a correlated form; `pre` does not

The in-LTJ hook was built around **one** ranking, fixed before the search
runs. A correlated clause has one per anchor. What makes the in-LTJ arms
work anyway is an ordering constraint rather than a new algorithm: force
the anchor **above** the search variable in the variable elimination order
(`VeoOverride::pin_at_after`), and by the time a visit reaches the search
level the anchor is bound and its vector is readable.

Three things that were per-query then become per-anchor, and all three
reset together in `VecCtx::retarget`:

| | why |
|---|---|
| the query vector | it *is* the anchor's vector |
| the top-`k` threshold | `k` counts per anchor; the previous anchor's cut was measured from a different vector |
| the corpus stream | a stream walks *from* a query vector, so a new vector is a new walk — which is exactly why the corpus sources cost so much more here than the local one |

Selection is per anchor too (`in_ltj::select_per_anchor`), applying the
same sink the other arms use inside each group.

**An anchor's visits are not generally contiguous**, and the cost of that
falls entirely on `interleave`. `pin_at_after` puts the anchor above the
search variable and says nothing about what sits above the *anchor*, so at
any level past 0 the join revisits an anchor once per binding of those
outer variables. `VecCtx::retarget` then fires once per visit rather than
once per anchor: a fresh vector, a fresh threshold and — for a corpus
source — a fresh stream, each time. Correctness survives (a reset
threshold only under-prunes; the surviving rows are discarded at
selection, which is why the equivalence tests pass at every level), but
the cost does not. `memo` sidesteps it by construction: phase 1 files by
anchor, so phase 2 retargets exactly once per anchor however the join
interleaved them.

`memo` keeps its two phases and scopes them by anchor. Phase 1 walks the
whole join and files each visit's candidates under the anchor that
reached them (`VecCtx::anchor_tables`, in first-seen order); phase 2 then
runs its single walk once per anchor, against that anchor's vector,
threshold and stream. "Once, globally" becomes "once per anchor", which
is still the thing the arm buys: at a deep level one anchor has many
visits, and `interleave` re-walks its ranking in every one of them.

The trade is memory. `interleave` holds one anchor at a time; `memo` holds
every anchor's candidates at once, because phase 2 cannot begin until
phase 1 has walked the whole join. Retargeting happens in phase 2 rather
than phase 1, since phase 1 consults no ranking and would only have built
a stream to leave it unread.

Phase 2's walk carries **three** cuts, and the last two bound it by the
candidate set instead of by the corpus:

| cut | when | worth |
|---|---|---|
| past the threshold | `k` held and the stream has moved beyond the worst of them, with `tau_eps` slack | the original cut |
| `k` held | the same fact one entry earlier: every accepted result is at least as near as this entry, and `TopK` refuses a tie once full. Only sound against an exactly sorted stream, so a caller asking for slack keeps walking | one pop per walk |
| every candidate seen | the table is empty, so no remaining stream entry can be in it, whatever the threshold says | caps the walk at the rank of the *last* candidate |

The third is the load-bearing one, and it is the discipline
`post_filter::walk_global` already applies with its `remaining` counter.
What it is worth depends entirely on where the last candidate sits in the
ranking: with the pattern's images interleaved through the corpus it trims
a few percent, and with them clustered ahead of it — the shape a real
corpus has, where the matched images are a sliver of the attribute — it
ends the walk at the eleventh entry instead of the two hundred and
eleventh. `FROGQL_DISABLE_MEMO_CUTS=1` is the kill switch the
differential test A/Bs both against, on the answer as well as the pops.

#### What the correlated arms measure

A synthetic sweep, since the IMGpedia dump is not on every machine.
60 450 nodes, 4 600 edges, a corpus of 60 420 sixteen-dimensional vectors
of which 60 000 are orphans — images with a descriptor and no triple, the
IMGpedia shape, spread pseudo-randomly so the corpus is genuinely hard
rather than lying on a line. `k = 50`, over

```
MATCH (a:Anchor)-[:R]->(m:Mid), (m)-[:R]->(v:Img)
NEAREST 50 v.emb TO VECTOR(a, 'emb') AS d
```

Twenty anchors and thirty mids, so the join reaches the search level 600
times — thirty visits per anchor, and (see above) not contiguously. All
400 candidates are 0.7% of the corpus. Every arm returns the same 10 000
rows.

| arm | level | ms | `nn_pops` | `nn_expanded` |
|---|---|---|---|---|
| `correlated+localsort` | — | 106.0 | 1 000 | 8 000 |
| `correlated+globalsort` | — | 128.0 | 142 688 | 1 208 400 |
| `correlated+hnsw` | — | 185.5 | 142 688 | 143 948 |
| `interleave+localsort` | 0 | 27.6 | 1 020 | 0 |
| `memo+localsort` | 0 | 29.5 | 1 000 | 0 |
| `interleave+globalsort` | 0 | 58.3 | 142 708 | 1 208 400 |
| `memo+globalsort` | 0 | 60.2 | 142 688 | 1 208 400 |
| `interleave+hnsw` | 0 | 110.2 | 142 708 | 143 968 |
| `memo+hnsw` | 0 | 109.7 | 142 688 | 143 948 |
| `interleave+localsort` | 1 | 40.3 | 30 600 | 0 |
| `memo+localsort` | 1 | **36.5** | **1 000** | 0 |
| `interleave+globalsort` | 1 | 965.2 | 13 534 770 | 36 252 000 |
| `memo+globalsort` | 1 | **67.7** | **142 688** | **1 208 400** |
| `interleave+hnsw` | 1 | 5 614.8 | 13 534 770 | 13 572 570 |
| `memo+hnsw` | 1 | **116.5** | **142 688** | **143 948** |

**At a level past 0 the corpus sources are where `memo` earns its
keep: 48× on hnsw, 14× on globalsort.** Two effects compound. The
ranking is walked once per anchor instead of once per visit (95× fewer
pops), and — the one that was not designed in — retargeting happens once
per anchor instead of once per visit, so `interleave` rebuilt its stream
600 times against `memo`'s 20 (30× fewer expansions). This is the case
that motivated the arm: on the IMGpedia dump `interleave+hnsw` at level 4
never finished.

**At level 0 there is one visit per anchor, nothing to hoist, and the two
are equal.** Same shape as the uncorrelated results.

**With a local source the clock still barely moves** — 36.5 ms against
40.3 ms, on 31× fewer pops. The join dominates when the ranking is
already cheap, which is the uncorrelated finding reproduced. The pop
count is a real quantity and a poor proxy for time; both are reported for
that reason.

**Partitioning loses to both**, 106 ms against 30–40 ms, since it runs
the whole pattern and ranks 8 000 buckets afterwards.

#### Why `globalsort` is worth keeping when there is an index

Read the two corpus sources against each other in the table above. At
level 1, `memo+hnsw` expands 143 948 vectors where `memo+globalsort`
expands 1 208 400 — the index touches **8.4× fewer** — and it is
**1.7× slower**, 116.5 ms against 67.7 ms. Both walk the same 142 688
stream positions; a best-first graph traversal simply costs far more per
position than indexing into a sorted array. The index pays only when the
walk stops early enough for 8× fewer expansions to beat a much cheaper
per-pop cost, and a selective pattern is exactly the case where it does
not stop early. `localsort` then beats both by another 1.9×, by never
leaving the candidate set at all.

So `globalsort` is not a naive baseline that the index supersedes. It is
there for three reasons: it is **exact**, so it is the oracle the
approximate arm's recall is scored against; it shares `hnsw`'s walk line
for line, so the pair isolates what the *index* buys with nothing else
varying; and on this shape it is simply the faster of the two.

`pre` has no correlated form. Pre-filter's defining property is that it
never runs the pattern first, and the anchors are not known until it has;
the honest correlated version needs the *minimal sub-pattern that binds
the anchor*, which is query planning this engine does not do. It
partitions, with the reason in `fallback_reason`, and
`tests/correlated_nearest_test.rs` pins that it says so rather than
report an arm that did not run.

### The in-LTJ arms decline a residual `WHERE`

`decompose_pattern` drops the predicate of a `PathPattern::Filter` and
decomposes only the inner pattern. Sound for every caller that reaches
LTJ through `run_path_pattern`, whose `Filter` arm re-applies it; unsound
for `in_ltj`, which invokes the decomposition directly and, until this
guard, returned rows the `WHERE` excluded:

```text
MATCH (hub:Img)-[:P69]->(v11:Img) WHERE hub.idx = -1 NEAREST 2 v11.emb TO ...
post       : v11 = 0, 1      correct
interleave : v11 = 0, 100    100 is reachable only from a node the WHERE excludes
```

Filtering the output afterwards would not repair it. The search prunes
with a running top-`k` threshold, so a row the predicate rejects has
already tightened the cut and excluded neighbours that belonged in the
answer: the right rows out of a wrong candidate set. `in_ltj` therefore
declines a pattern with `has_residual_filter()` and degrades to
post-filtering, which evaluates the whole query. Widening the optimizer's
value-predicate pushdown is what would re-admit these shapes; until then
the guard is what keeps the arm honest. Pinned by
`vector_strategy_equiv_test::a_residual_where_is_not_dropped`.

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

## The strategies

Every arm enters through `vsearch::run_nearest` and leaves as an
`IntermediateResult`, so projection, DISTINCT, ORDER BY, and LIMIT
downstream are identical, which is what makes the latencies comparable.

Three modules, because algorithms 2 and 3 share one: they differ only in
`VecSource`.

### 1. post-filter (`vsearch/post_filter.rs`)

Run the pattern, then rank what it produced. Under `LocalSort` that is a
distance to every binding the pattern produced — linear in *candidates*,
not in the corpus. Under the two corpus-wide sources it hashes the
candidates and walks the global ranking until `k` are hit, which costs
whatever it takes to reach the `k`-th surviving candidate: small when the
pattern is unselective, and deep when it is selective.

Answers every query shape, so it is also the universal fallback.

### 2. interleave (`vsearch/in_ltj.rs`, `NnMode::Interleave`)

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

The cut is per visit, not global: it bounds how deep each visit scans,
not how many visits scan. That is the cost `memo` was written to remove.

### 5. memo (`vsearch/in_ltj.rs`, `NnMode::Memo`)

Same hook, same level, ranking consulted once instead of per visit.

**Phase 1** runs the ordinary search down to the search level and stops.
Every candidate that survives the levels above is recorded in
`VecCtx::table` against all the prefixes reaching it. Prefixes are stored
flat — one buffer per key, the `i`-th prefix a slice at `i * stride`
(`Prefixes`) — because the obvious `Vec<Vec<u32>>` costs one allocation
per prefix and a deep level has as many prefixes as the join has partial
rows. The filters at the level still run, so a candidate that cannot
survive never enters the table.

**Phase 2** walks the ranking once. A miss is a hash lookup. A hit is
resumed: replay the stored prefix with `down`, search the levels below,
undo with `up`. Replaying is sound because `down` needs no preceding
`seek` — which is why phase 1's own level could call it straight after
collecting candidates — so the iterators land exactly where the
collecting pass left them. The filters at and above the level are not
re-evaluated: a stored prefix is one that already passed them.

`LocalSort` here ranks the **table's keys** rather than a per-visit set:
they are the only nodes that can contribute, so nothing outside the
domain is touched and no membership test is needed.

**Level 0 is where `memo` cannot win.** With the search variable at level
0 there is exactly one visit, so there is nothing to re-walk and the
table is pure overhead. Measured below.

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
heuristic was worth. On the fixture below the clamp makes levels 1 and 2
the same position, which is why their rows are identical.

### 3. pre-filter (`vsearch/pre_filter.rs`)

Walk the neighbour stream; pin the search variable to each candidate and
re-run the whole pattern. Nearly free to build, since pinning is what the
LTJ already does for correlated EXISTS. Exactly one pattern evaluation per
neighbour examined, so it wins when the first few neighbours also match
and loses badly when the pattern is selective.

A special case of the in-LTJ arms with the search variable at level 0 —
but only at level 0. Placing it deeper is something only `interleave` and
`memo` can do. `memo` at level 0 is the same shape with the domain
memoised, which is why it beats `pre` by an order of magnitude below:
same single global walk, one join instead of one per neighbour.

## What to measure

**`nn_pops` per accepted result.** With a selective pattern the arms that
consult a corpus-wide ranking walk a proximity graph built over the
*whole* corpus, so reaching a candidate that also satisfies the pattern
can cost a large fraction of layer 0. This is the classic filtered-ANN
failure mode. Post-filter degrades gracefully exactly where those blow
up.

Read it **against wall clock**, never instead of it. The first result
below is that `nn_pops` and latency can move in opposite directions.

`VecStats` also records **which arm executed**, not which was requested: a
precondition miss falls back, and reporting the requested arm would lie.

```bash
cargo run --release --bin vec_bench -- --items 50000 --dim 128 --ks 1,10,100 --levels 0,1,2
```

CSV columns: `items,dim,k,mode,selectivity,strategy,source,level,median_ms,
recall,nn_pops,nn_expanded,pattern_runs,ltj_visits,candidates,resumes,rows`.

## Results

20 000 items, dim 32, `k` = 10, distinct-binding mode, 5 query vectors ×
5 iterations, median. Same data, same seed, one binary.

| arm | level | ms | `nn_pops` | candidates | resumes |
|---|---|---|---|---|---|
| `interleave+localsort` | 0 | **2.95** | 11 | 8 659 | — |
| `interleave+globalsort` | 0 | 3.40 | 4 163 | 8 659 | — |
| `interleave+hnsw` | 0 | 4.28 | 4 163 | 8 659 | — |
| `memo+localsort` | 0 | 5.59 | 11 | 8 659 | 10 |
| `memo+globalsort` | 0 | 5.64 | 4 163 | 8 659 | 10 |
| `memo+hnsw` | 0 | 6.59 | 4 163 | 8 659 | 10 |
| `interleave+localsort` | 1 | **11.71** | 1 050 | 20 000 | — |
| `interleave+globalsort` | 1 | 12.87 | **198 964** | 20 000 | — |
| `interleave+hnsw` | 1 | 15.77 | **198 713** | 20 000 | — |
| `memo+globalsort` | 1 | 18.47 | **4 163** | 20 000 | 12 |
| `memo+hnsw` | 1 | 18.58 | **4 163** | 20 000 | 12 |
| `memo+localsort` | 1 | 18.67 | **11** | 20 000 | 12 |
| `post+globalsort` | 0 | 44.69 | 4 162 | 8 659 | — |
| `post+hnsw` | 0 | 45.59 | 4 162 | 8 659 | — |
| `pre+globalsort` | 0 | 55.87 | 4 163 | — | — |
| `pre+hnsw` | 0 | 59.02 | 4 163 | — | — |

**The re-walk is real, and fixing it does not pay here.** Off level 0,
`interleave` pops 198 964 neighbours where `memo` pops 4 163 — 48× fewer,
exactly the cost the two-phase shape was written to remove. `memo` is
still 1.4× slower in wall clock. The join dominates: phase 1 collects
20 000 candidates and the neighbour order then completes 12 of them, so
the ranking was never the bottleneck it looked like.

`interleave+localsort` at level 1 is the cleanest demonstration. It pops
1 050 against `memo+localsort`'s 11, a 95× gap on the headline metric,
and it is 1.6× *faster*.

**At level 0 `memo` cannot win, and does not.** One visit means nothing to
re-walk, so identical `nn_pops` and a table built for nothing: 3.40 ms
against 5.64 ms, same source, same 4 163 pops.

**Both in-LTJ arms beat both baselines by a wide margin**, which is the
result that was being looked for: 2.95 ms against 44.69 ms (post) and
55.87 ms (pre). And `memo` at level 0 beats `pre` roughly 10× on the same
single global walk, the difference being one join instead of one per
neighbour (`pattern_runs`).

Caveat: one fixture, one shape, one scale. The crossover where
materialising a prefix costs less than re-walking a ranking should move
with corpus size, selectivity, and how much join sits below the search
level. None of that is charted yet.

## Equivalence

`tests/vector_strategy_equiv_test.rs` is what makes the benchmark
legitimate. Under either **exact** source every strategy returns
identical answers across VEO levels, `k` values, both k-modes, and four
query shapes — algorithms 1, 2, 3, and 4 all agree. If they could disagree, comparing their latency would be
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
| `FROGQL_VEC_STRATEGY=post\|pre\|interleave\|memo` | which strategy to run (default `post`). `inltj` is an accepted alias for `interleave` |
| `FROGQL_VEC_SOURCE=hnsw\|localsort\|globalsort` | where the ranking comes from (default `hnsw`) |
| `FROGQL_VEC_LEVEL=<n>` | VEO position of the search variable; `interleave` / `memo` only, clamped |
| `FROGQL_VEC_TAU_EPS=<f>` | relative slack on the threshold cut (default 0) |
| `FROGQL_DISABLE_MEMO_CUTS` | drop `memo`'s two phase-2 walk cuts (`k` held, every candidate seen), leaving only the threshold cut. The kill switch the differential test A/Bs against |
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
- **`pre` has no correlated form.** It partitions and ranks; `post`,
  `interleave` and `memo` all have one. See *Correlated `NEAREST`* above.
