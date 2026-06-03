# froGQL vs Grafeo — head-to-head micro-benchmark

A reproducible comparison between **froGQL** and **[Grafeo](https://grafeo.dev/)**,
two embeddable Rust graph databases that both expose a Python binding with an
ISO-GQL `execute`. The harness loads the *identical* graph into each engine,
verifies the two return the *same rows* for every query, then times warm
executions with the same wall-clock method.

This is a runtime benchmark. It does **not** measure froGQL's distinguishing
feature — the static typechecker that rejects type-mismatched queries before they
run.

## Run it

```bash
python3 -m venv venv && . venv/bin/activate
pip install frogql "grafeo[cli]"
python bench.py
```

Versions used for the numbers below: **froGQL 0.2.3**, **Grafeo 0.5.34**,
Apple Silicon macOS, warm runs, median of 9 iterations (+1 warmup).

## Method, and how it stays fair

- **Same logical graph in both.** A seeded generator emits a directed
  `(:Person)-[:KNOWS]->(:Person)` graph; parallel edges and self-loops are
  removed so the two engines see the same graph. froGQL models edges as a set of
  `(src, label, tgt)` triples (no parallel edges); Grafeo is a multigraph. With
  parallel edges present the row counts legitimately differ, so we drop them.
- **Same queries, results checked.** Every timed query is run on both engines and
  the result sets are compared as multisets. A query is only reported in the
  speed table when both engines return identical results.
- **Same clock.** `time.perf_counter` around `execute`, GC disabled during the
  timed region, for both engines equally.

## Results (synthetic, 10 000 nodes / 79 965 edges)

| Query | froGQL ms | Grafeo ms | Faster |
|---|--:|--:|---|
| `label_scan` — `MATCH (p:Person) RETURN p.name` | 12.2 | 3.0 | **Grafeo 4.1×** |
| `filter_attr` — `WHERE p.age > 80` | 7.0 | 0.8 | **Grafeo 8.4×** |
| `count_nodes` — `RETURN count(p)` | 7.7 | 0.2 | **Grafeo 36×** |
| `one_hop_cnt` — 1-way join | 68.9 | 26.5 | **Grafeo 2.6×** |
| `two_hop_cnt` — 2-way join | 673.7 | 243.7 | **Grafeo 2.8×** |
| `three_path_cnt` — 3-way join | 6750.1 | 2141.3 | **Grafeo 3.2×** |
| `one_hop_rows` — 1-hop, projected (79 965 rows) | 136.9 | 64.4 | **Grafeo 2.1×** |

The ratios are stable across scales (movies / 2 k / 10 k). On this uniform-random
social graph **Grafeo is faster on every comparable query.**

### Honest reading of the gap

- **Cheap queries (scan / filter / count).** froGQL's Python binding parses,
  typechecks and optimizes on *every* `execute`; Grafeo caches the plan. When the
  query itself is sub-millisecond, that per-call compile dominates. froGQL also
  has no metadata `count` fast-path — it materializes rows then counts. Both are
  fixable engineering gaps, not fundamentals.
- **Joins.** The compile cost amortizes, but Grafeo (vectorized / SIMD execution)
  still leads by ~2.5–3× on uniform-random joins. froGQL's worst-case-optimal
  join pays off on dense, cyclic, or skewed joins, which a uniform-random graph
  does not stress.

## Correctness finding: cyclic joins

The harness also runs a directed triangle on a minimal hand graph
(`0→1→2→0`, plus a dangling `2→3`). The only triangle is `(0,1,2)`, whose correct
homomorphic count is its 3 rotations.

| Engine | triangle count | rows |
|---|--:|---|
| froGQL | **3** | `(p0,p1,p2) (p1,p2,p0) (p2,p0,p1)` |
| Grafeo | 4 | the 3 above **+ `(p3,p1,p2)`** |

`(p3,p1,p2)` requires the edge `p3→p1`, which does not exist — Grafeo does not
close the cycle and returns a spurious match. froGQL's worst-case-optimal join
computes the cyclic join correctly. (Observed on Grafeo 0.5.34; worth reporting
upstream.) Because the two engines compute different answers here, the triangle
is excluded from the speed table.

## Takeaway

On raw runtime over a uniform-random social graph, **Grafeo 0.5.34 is the faster
engine** across simple patterns and aggregates. froGQL's value is elsewhere: a
static typechecker that detects empty/ill-typed queries before execution, and a
worst-case-optimal join that stays correct on cyclic patterns. The closest
froGQL gaps here — per-call compile and a `count` fast-path — are known and
addressable.
