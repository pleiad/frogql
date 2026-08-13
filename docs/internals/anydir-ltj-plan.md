# Any-direction edges in LTJ — design note

Status: **IMPLEMENTED** (Approach B — stored mirrored index), after the team
adopted ISO bag semantics (issue #71). The unblocking prerequisite — the LTJ
base-case per-eid fan-out — landed first; the mirrored index then produced
ISO-correct multiplicity and verifies against a hand-computed ground-truth
oracle (`tests/anydir_iso_test.rs`). Behind `FROGQL_DISABLE_ANYDIR_LTJ`.
This note is retained as the rationale record; the *Multiplicity* section
below documents the earlier reverted prototype and why the fan-out fix was
the missing piece.

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

Recommendation: **option 1, lazily built, behind `FROGQL_DISABLE_ANYDIR_LTJ`.**

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

**Resolution (issue #71).** The team adopted ISO bag semantics. Two steps
landed the feature:

1. **LTJ base-case fan-out** (`algorithm.rs`): the base case now emits one
   row per physical eid at the bound `(s, p, o)` unconditionally — not only
   when an edge variable is bound. This fixes the collapse for directed
   parallel edges too (a shipped inconsistency) and is the exact mechanism
   that makes the mirror correct: reciprocal `a→b`/`b→a` become two
   distinct eids at the fact `(a, L, b)` and fan out to two rows. Oracle:
   `tests/iso_multiplicity_test.rs`.
2. **Mirrored index** (`from_graph_anydir` + `try_ltj_anydir`): pure
   any-direction chains/joins run against the mirror. Oracle:
   `tests/anydir_iso_test.rs` (hand-computed ISO counts, not the fallback).

Follow-on: with the mirror in place, `unroll_repeat` no longer excludes
any-direction inners — a bounded, unused-edge `-[]-{n,m}` unrolls into a
Union whose flat any-direction arms each run through `try_ltj_anydir`
(ISO-correct, seeded by the boundary filter). So the issue-#57 shape
`(a)-[e]-{1,3}(b)` with `e` unprojected now takes the mirror-LTJ path
rather than the seeded adjacency traversal.

**Correctness is complete; performance is complete too.** A cross-path
differential (`tests/anydir_path_consistency_test.rs`) confirms all
any-direction paths agree on the ISO bag multiset across single-hop,
multi-hop, comma-join, unused- and used-edge repetition, and mixed
direction: the seeded traversal (`try_concat_with_edge_repetition`) and the
plain adjacency/hash-join fallback both iterate physical edge ids, so they
were never affected by the LTJ trie collapse and already produce ISO counts
— the fan-out fix simply brought LTJ into line with them. (An earlier note
here claimed the seeded/fallback paths were non-ISO; that was measured on
the pre-fan-out-fix prototype and is wrong.)

**Mixed directed + any-direction — DONE (per-triple index routing).**
`try_ltj_mixed` decomposes a mixed pattern with a per-triple
`EdgeKind::AnyDir` tag and builds each `LtjIterator` against the index its
edge kind selects — any-direction triples against the mirror, the rest
against the plain index. The leapfrog intersection joins across the two
transparently because node ids are global and both indexes assign label ids
in the same edge iteration order (so a label constant resolves identically
in either). `try_ltj` bails on any-direction via `has_any_direction`, so a
pure-directed workload never builds the mirror. Measured WCO win on the
fraud DB: `(t1)-[:USED_DEVICE]->(d)-[]-(t2)` runs ~4.7× faster / ~2× less
RSS than the fallback (which materializes the intermediate). Nothing
any-direction remains on the fallback except Unions and non-unrollable
repetitions, which are LTJ non-shapes for directed edges too.

## Acceptance criteria

Both implementations already exist (LTJ path + three-way hash-join
fallback), so the oracle is free: a **differential suite** asserting
merged-iterator LTJ ≡ current fallback on **row multisets** (not just
counts), over graphs with reciprocal edges, self-loops, and mixed
directed/undirected edges. Mirror the `shortest_bfs_test` pattern with a
`FROGQL_DISABLE_ANYDIR_LTJ` toggle serialized by a mutex. Plus an LDBC /
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
