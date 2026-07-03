# Any-direction edges in LTJ — design note

Status: **proposed, not implemented.** The issue-#57 OOM is already resolved
without this (see *Relationship to issue #57* below); this note scopes the
*performance* follow-up so it lands as its own reviewable, benchmarked PR
rather than riding along with the OOM fix.

## Problem

LTJ decomposes a pattern into `(src, label, tgt)` triples and binds
variables by leapfrog-intersecting the six sorted orderings. An
any-direction edge `(a)-[e]-(b)` matches a physical edge in *either*
orientation — `edge(a,e,b) ∨ edge(b,e,a)`. That disjunction is local to a
single atom, but a single triple pattern `(a, e, b)` is a *conjunctive*
constraint that checks one sense only. So `flatten_concat`
(`src/runtime/ltj/pattern_extract.rs`) returns `false` for
`EdgeAnyDirection`, and any pattern containing one falls off LTJ onto the
pairwise hash-join fallback.

Contrast the two edge forms the index *does* handle:

- **Directed `-[L]->` / `<-[L]-`**: one physical triple `(src, L, tgt)`.
  Leftward reuses the same triple with src/tgt swapped in the *pattern*
  (`decompose_flat_chain`: `EdgeKind::Left ⇒ (next, L, current)`), so no
  extra index data is needed.
- **Undirected `~[L]~`**: `TripleIndex::from_graph` emits it in **both**
  senses (`push_both`, `triple_index.rs:69`) — `(src,L,tgt)` *and*
  `(tgt,L,src)` sharing one eid. A single forward triple pattern then
  matches either endpoint binding. This is the "reverse triple" trick.

Any-direction is neither: the edges are physically directed (so not
mirrored like `~`), but the query wants both orientations (so one directed
triple is insufficient).

## Why the merged-iterator (approach B) is the right target

`(a)-[e]-(b)` is `edge(a,e,b) ∨ edge(b,e,a)` — a union over the extension
of *one* relation, not a union across the query. The clean fix pushes the
OR **below** the join, into the iterator, so leapfrog stays a pure
intersection and never learns the disjunction existed. This keeps
worst-case-optimality. It is the standard treatment:

- Veldhuizen, *Leapfrog Triejoin* (ICDT 2014, arXiv:1210.0481) §∃₁ — the
  trie-iterator `seek/next/atEnd` interface is designed to accept
  non-materialized predicate views; a merged forward+reverse view is
  exactly such a view.
- Arroyuelo et al., *The Ring* (SIGMOD 2021) — **caveat to record in
  `JOIN_STRATEGY_NOTES.md`**: the ring's "traverse attributes in either
  direction" is about which of s/p/o you bind first in the variable
  order, **not** the semantic orientation of the edge. It buys cheap
  reverse *access*, not undirected *matching*. Conflating the two is the
  standard slip.
- MillenniumDB (IMFD) implements GQL undirected edges over an LTJ/ring
  engine — closest concrete reference; read its edge-kind handling.

Approach C (UCQ: rewrite `k` any-direction edges into up to `2^k`
conjunctive queries, run LTJ per branch, union + dedup) is the fallback,
not the target: exponential in `k` (froGQL chains 2–3 any-direction edges
routinely — the issue-#57 shape after unroll is 3), and it forces bag-vs-set
dedup for self-loops and reciprocal pairs. Keep the existing hash-join as
that fallback; do not build UCQ.

## The storage decision (the crux, and why this is its own PR)

Approach B needs both senses of a *directed* edge reachable in the order
the current variable-elimination step needs. Today they are not: the
reverse `(tgt, L, src)` of a directed edge is absent from the index.
Options:

1. **Mirror directed edges into a separate any-direction index.**
   A second `TripleIndex`-like structure (or a `both_senses` triple set)
   built by emitting every directed edge in both senses, consulted *only*
   when a pattern edge is `EdgeAnyDirection`. Directed and `~` lookups keep
   using the current index unchanged (mirroring the main index would
   corrupt directed queries — they'd match reversed edges).
   - Cost: ~2× the directed-triple count for the any-direction index. The
     current TripleIndex is already ~670 ms to build and ~12 % of file
     size if persisted (see `storage-architecture.md`); an any-direction
     mirror is a comparable increment. **Must be measured**, and probably
     built lazily on first any-direction query, not at open.

2. **Tag each index entry with a sense bit and let any-direction patterns
   query both.** Avoids a second structure but complicates every
   ordering's comparator and the `LtjIterator` range logic, and still
   needs the reverse tuples present. More invasive to the hot path than
   option 1.

Recommendation: **option 1, lazily built, behind `GQLITE_DISABLE_ANYDIR_LTJ`.**

## Interfaces this touches

- `pattern_extract.rs`: new `EdgeKind::AnyDir`; `flatten_concat` accepts
  `EdgeAnyDirection`; `decompose_flat_chain` emits an any-direction triple
  whose iterator is bound to the merged view rather than the main index.
- `iterator.rs`: `LtjIterator` currently navigates a single `TripleIndex`
  ordering. The merged view means either (a) the iterator holds a second
  index reference for any-direction triples, or (b) a small
  `MergedIterator` wrapping two `LtjIterator`s with a sorted-merge
  `leap`/`seek_all`/`children_count`. (b) is cleaner but must preserve the
  `down`/`up` stack contract.
- `algorithm.rs`: base-case eid fan-out (`current_eids_all`) must expand
  **per (edge, orientation)**, not per eid — otherwise the row multiplicity
  is wrong (see below).

## Multiplicity — the correctness trap, now CONFIRMED by a prototype

This was prototyped (mirrored index via `from_graph_anydir`, pure-any-
direction patterns rewritten `EdgeAnyDirection → EdgeRight` and run against
it) and a differential suite compared it to the hash-join fallback. **The
prototype was reverted** because the differential suite proved the trap is
real and, worse, that the baseline itself is inconsistent.

On a 5-node graph with a reciprocal pair (`a→b`, `b→a`), a directed
self-loop (`c→c`), parallel edges (`a→d` twice), and an undirected edge,
the single-hop `MATCH (x)-[]-(y) RETURN x.id, y.id` produced **three
different row multisets**:

| path | rows | why |
|---|---|---|
| mirrored-index LTJ | 9 | the trie collapses distinct edges sharing `(s,L,o)` when no edge var is bound — after mirroring, `a→b` and `b→a` both become the fact `(a,L,b)` and count **once** |
| hash-join fallback | 14 | fans out `right ∪ left ∪ undirected`, but still under-counts some label/parallel cases |
| strict GQL bag semantics (hand-computed) | 16 | one row per (edge, traversable orientation) |

So the question is not "does the mirrored index work" — it does, and it is
fast — but "**what is the correct any-direction bag multiplicity over a
multigraph**," and today *none* of {LTJ, fallback, ISO bag semantics}
agree. The LTJ collapse is intrinsic (`algorithm.rs`: the base case fans
out per parallel eid only when `triple_has_edge_var`; without a bound edge
variable it emits one canonical eid — the documented "right behavior for
vertex joins"). Making the mirrored path match the fallback would require
forcing per-eid fan-out for any-direction triples; making *everything*
match ISO would require aligning the fallback too.

**Consequence for sequencing:** enabling any-direction LTJ is blocked on a
prior decision about multiplicity semantics, applied uniformly across the
fallback, the directed LTJ (which has the same collapse), and this path.
That is a correctness project in its own right — it must not ride in on a
performance PR. The reverted prototype lives in git history; resurrect it
only alongside a settled bag-semantics spec and a differential oracle that
is *ISO ground truth*, not the current fallback.

## Acceptance criteria

Both implementations already exist (LTJ path + three-way hash-join
fallback), so the oracle is free: a **differential suite** asserting
merged-iterator LTJ ≡ current fallback on **row multisets** (not just
counts), over graphs with reciprocal edges, self-loops, and mixed
directed/undirected edges. Mirror the `shortest_bfs_test` pattern with a
`GQLITE_DISABLE_ANYDIR_LTJ` toggle serialized by a mutex. Plus an LDBC /
soc-LiveJournal1 latency run to justify the index-size increment.

## Relationship to issue #57

Issue #57 (`(a)-[e]-{1,3}(b)` OOM) is **already fixed** without any of the
above, by two changes that shipped together:

1. `unroll_repeat` no longer unrolls any-direction inners
   (`can_unroll` rejects `EdgeAnyDirection`) — unrolling produced a union
   of non-decomposable arms that fell to the global hash-join.
2. `run_concat_pattern` gained a seeded repetition traversal
   (`try_concat_with_edge_repetition`) that expands level-by-level from the
   already-filtered left rows.

Measured: `-[e]-{1,3}` with a selective left filter dropped from
126 s / 5.6 GB to **0.014 s / 54 MB**, same 5 rows.

This note's work is a **separate performance goal**: making any-direction
edges LTJ-eligible in *comma-joins and chains* (not just single-edge
repetitions), where they currently take the correct-but-not-WCO hash-join.
Once landed, revisit whether the unroll gate (change 1) can narrow — an
unrolled any-direction chain would then be LTJ-decomposable, so unrolling
it becomes profitable again rather than an OOM.
