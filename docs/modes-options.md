# Modes and options

How to run froGQL in something other than its default configuration: which
knobs exist, what each one actually turns off, and what it costs. Every
measurement below is LDBC SF0.1 (`bench/data/ldbc-sf0.1.gdb`, 327 588 nodes
/ 1 477 965 edges) on a warm OS cache, macOS, release build. Treat them as
indicative: peak RSS varies ~10% run to run.

## 1. Two mechanisms

froGQL has two ways to change how a session behaves:

- **CLI flags** on the `frogql` binary. Ergonomic, discoverable via
  `frogql` with no arguments.
- **Environment variables**, read at open time or query time. These are the
  source of truth: every optimization in the engine has an env kill switch
  so a differential test can pin "optimized ≡ baseline" without a rebuild.

Where both exist, the flag sets the variable. `--no-auto-indexes` is exactly
`FROGQL_DISABLE_AUTO_INDEXES=1`, set before the store opens.

Every variable is prefixed `FROGQL_`. There is no fallback to any other
prefix: a variable the engine does not recognize is silently inert, so a
misspelled switch looks exactly like a default run.

## 2. CLI flags

```
frogql <database.gdb> [--no-typecheck] [--no-auto-indexes]
frogql <database.gdb> --import-csv <dir>       [flags]
frogql <database.gdb> --import-ldbc-csv <dir>  [flags]
frogql <database.gdb> --import-json <file>     [flags]
```

| Flag | Effect | Cost of using it |
|---|---|---|
| `--no-typecheck` | Skips the typechecker for the session. Queries compile straight to the runtime. | You lose the `guaranteed empty` short-circuit and all static errors. A query that the checker would reject now runs and returns nothing, slowly. |
| `--no-auto-indexes` | Skips the secondary-index auto-build at open. | Open drops the 0.54 s index phase and ~120 MiB of peak RSS, but `Eq` predicates stop constant-folding into point lookups. IC2 goes from ~9 ms back to seconds. |

A non-existent path is created, sqlite3-style, and opens as an empty
database ready for `INSERT` + `.save`.

## 3. Environment variables

### 3.1 Index and storage

| Variable | Effect |
|---|---|
| `FROGQL_DISABLE_AUTO_INDEXES=1` | Skip the secondary-index auto-build at open (same as `--no-auto-indexes`). |
| `FROGQL_DISABLE_INDEX_FOLD=1` | Keep the indexes, but skip the LTJ pre-pass that folds `x.attr = k` into a pinned constant and range predicates into a `NodeInSet`. Isolates "does the index exist" from "does the optimizer use it". |

### 3.2 Join strategy

| Variable | Effect |
|---|---|
| `FROGQL_LTJ_COMPACT=1` | Build the LTJ index as six LOUDS succinct tries instead of six sorted arrays. 2.87× smaller, 1.4–2.1× slower on IC latency. |
| `FROGQL_DISABLE_ANYDIR_LTJ=1` | Force the hash-join fallback for any-direction (`-[e]-`) patterns, and skip building the mirrored index entirely. |
| `FROGQL_DISABLE_SEEDED_REPEAT=1` | Use the legacy global repetition path instead of the seeded adjacency traversal. |
| `FROGQL_DISABLE_REPEAT_UNROLL=1` | Keep bounded `{n,m}` repetitions as `Repeat` instead of unrolling to a Union of LTJ-eligible arms. |
| `FROGQL_DISABLE_SHORTEST_BFS=1` | Force the generic k-shortest-walk enumerator instead of the BFS fast path. Expect IC1/IC13 to go from ~25 ms to tens of seconds, and IC14 to OOM. |

### 3.3 Correlated subqueries and clauses

| Variable | Effect |
|---|---|
| `FROGQL_DISABLE_EXISTS_PIN=1` | Materialize correlated `EXISTS` bodies once over the whole graph instead of probing with pinned LTJ. |
| `FROGQL_DISABLE_VALUE_SUBQUERY_PIN=1` | Same, for `VALUE { … }` subqueries. IC7 goes from ~9 ms to ~1.3 s. |
| `FROGQL_DISABLE_OPTIONAL_PUSHDOWN=1` | Evaluate `OPTIONAL MATCH` globally then left-join, instead of per-row bind pushdown. IS5 slows ~93×. |
| `FROGQL_ORDERBY_FORCE=pdqsort\|topk` | Pin one ORDER BY strategy, bypassing the btree-LTJ-real top-k selection. |

### 3.4 Diagnostics

| Variable | Effect |
|---|---|
| `FROGQL_TRACE_OPEN=1` | Print per-phase open latency to stderr. |
| `FROGQL_DEBUG_INDEXES=1` | Print the auto-built index list at open and every LTJ variable pinned through an index. |

## 4. What each phase costs

`FROGQL_TRACE_OPEN=1` on the default configuration:

```
  pager open:                 0.000s
  string table load:          0.146s
  topology + indexes:         0.202s  (327588 nodes, 1477965 edges)
  catalog load:               0.000s
  secondary index auto-build: 0.543s  (52 indexes)
  secondary index DDL replay: 0.000s  (52 indexes total)
Loaded 327588 nodes, 1477965 edges in 0.87s
LTJ TripleIndex built in 0.69s
```

The four structures worth knowing by size, since they dominate RSS. None of
them is persisted: every one is rebuilt from scratch on each open (the
mirror, only on demand):

| Structure | Build | Heap | Persisted? |
|---|---|---|---|
| LTJ TripleIndex, array | 0.68 s | 136.6 MiB | no, rebuilt every open |
| LTJ TripleIndex, compact | 0.82 s | 47.6 MiB | no, rebuilt every open |
| Secondary indexes (auto) | 0.54 s | ~120 MiB | no, rebuilt every open |
| Any-direction mirror | ~0.9 s | ~330 MiB | no, built on first `-[e]-` query |

Reproduce the first two with `cargo run --release --bin ltj_index_stats -- <db.gdb>`.

## 5. Recipes

### Minimum RAM

```bash
frogql db.gdb --no-auto-indexes
```

That is the whole recipe. Adding `FROGQL_LTJ_COMPACT=1` makes peak RSS
**worse**, not better. Measured at SF0.1 on `MATCH (p:Person) RETURN
COUNT(p)`, same run, peak RSS:

| Configuration | Peak RSS |
|---|---|
| default | 482 MiB |
| `--no-auto-indexes` | **365 MiB** |
| `FROGQL_LTJ_COMPACT=1` | 516 MiB |
| both | 415 MiB |

The compact index is 2.87× smaller *once built*, but building it sorts a
temporary vector per trie, and that transient exceeds what the smaller
steady-state structure saves. Reach for `FROGQL_LTJ_COMPACT` when the
session is long-lived and you care about resident size after warm-up; reach
for `--no-auto-indexes` when you care about the peak.

Queries get slower either way. This is a mode for measuring the memory
floor, not for latency numbers.

### Lowest latency

The default. No flags, no variables. The auto-indexes and the array
TripleIndex are the two structures every LDBC IC number depends on.

### Differential testing

Every kill switch exists so you can assert that the optimized path and the
baseline agree on results. Run the same query twice and diff:

```bash
for mode in "" "FROGQL_DISABLE_ANYDIR_LTJ=1"; do
  printf 'MATCH (a)-[e]-(b) RETURN COUNT(*) AS n\n.quit\n' \
    | env $mode frogql db.gdb
done
```

The suites that already do this: `tests/compact_ltj_test.rs`,
`tests/seeded_repetition_test.rs`, `tests/shortest_bfs_test.rs`,
`tests/anydir_path_consistency_test.rs`.

### Bench runs

Rebuild the `.gdb` with the binary under measurement before any run whose
numbers matter:

```bash
cargo build --release
cargo run --release --bin bench_setup -- --rebuild --skip-download
```

A `.gdb` written by an older binary keeps its stale persisted DEFAULT
schema, which can degrade compile-time numbers by ~3000× with no visible
error.

## 6. Gotchas

**Auto-indexes are never loaded, always built.** They are memory-only by
design, so "skip loading" and "skip creating" are the same act. Only
DDL-declared indexes (`CREATE INDEX`) are persisted, and `--no-auto-indexes`
does **not** skip their replay: that runs in a separate step after the
auto-build. On a `.gdb` with no DDL, the replay is 0 ms.

**The any-direction mirror is lazy.** It is built on the first query
containing `-[e]-`, never at open. `warm_triple_index()` only warms the
plain index. If your workload has no any-direction patterns, you never pay
the ~330 MiB, and no flag is needed to get that.

**Disabling any-direction LTJ can cost more than the mirror.** The fallback
hash-join materializes both sides in full. On `MATCH (m)-[e]-(p) RETURN m
LIMIT 1` at SF0.1: 0.71 GiB / 0.9 s with mixed LTJ, versus 1.88 GiB / 4.0 s
with `FROGQL_DISABLE_ANYDIR_LTJ=1`. Use the switch for differential testing,
not to save memory.

**`FROGQL_LTJ_COMPACT` raises peak RSS while lowering steady-state.** The
compact index is 2.87× smaller once built (47.6 vs 136.6 MiB), but the build
sorts a temporary vector per trie, and that transient costs more than the
structure saves: 516 MiB peak versus 482 MiB on the default path. It is a
resident-size optimization, not a peak-memory one.

**`--no-typecheck` is not just a speed knob.** The typechecker is what
rejects unbounded repetition without a §16.6 prefix. Without it, the query
compiles through `compile_query_unchecked` and the runtime panics on an
invariant the checker was supposed to guarantee:

```
$ frogql examples/movies.gdb --no-typecheck
gql> MATCH (a)-[:ACTED_IN]->{1,}(b) RETURN a LIMIT 1
thread 'main' panicked at src/runtime/engine.rs:1488:51:
unbounded repetition reached the runtime without a finite-making prefix
```

Reach for the flag to measure typechecker overhead, not to run queries the
checker rejects.

---

## Vector search

`NEAREST` has two orthogonal axes: which **strategy** evaluates it, and
which **source** supplies the nearest-first ranking. Under either exact
source every strategy returns the same answer — pinned by
`tests/vector_strategy_equiv_test.rs` — so those are cost knobs, not
semantic ones.

| Var | Effect |
|---|---|
| `FROGQL_VEC_STRATEGY=post\|pre\|inltj` | which strategy runs (default `post`) |
| `FROGQL_VEC_SOURCE=hnsw\|localsort\|globalsort` | where the ranking comes from (default `hnsw`) |
| `FROGQL_VEC_LEVEL=<n>` | VEO position of the search variable; in-LTJ only |
| `FROGQL_VEC_TAU_EPS=<f>` | relative slack on the top-k threshold cut |
| `FROGQL_DISABLE_VECTORS` | ignore every sidecar |
| `FROGQL_DEBUG_VEC` | print the executed arm and its counters |

**`FROGQL_VEC_SOURCE=hnsw` changes the answer, deliberately.** HNSW is
approximate; `localsort` and `globalsort` are exact. Recall is a
measurement, not a defect.

**`hnsw` and `globalsort` are the same walk.** Both build a corpus-wide
ranking and make every visit re-scan it from the top. Switching between
them changes only what it costs to build that ranking — `nn_pops` is
identical, `nn_expanded` is not. And HNSW evaluates ~32 neighbour
distances per expansion, so on a small corpus it can end up doing *more*
work than sorting everything: at 3 000 items, level 1, it expanded 1306
nodes (~42 k distances) against a flat 3 000. `localsort` is the source
that genuinely walks differently — it never leaves the level.

**The strategy you ask for is not always the one that runs.** A shape an
interleaving arm cannot hook into — an `OPTIONAL MATCH`, a §16.6 prefix, a
pattern that does not decompose, a search variable the secondary index
folded to a constant — falls back to post-filtering. `FROGQL_DEBUG_VEC`
prints what actually ran and why; `vec_bench` warns on the mismatch. Never
read a timing as belonging to the requested arm without checking.

**`FROGQL_VEC_LEVEL` is a real trade, not a tuning default.** At level 0
the neighbour stream drives the whole join. Deeper, the candidate set at
each visit is smaller but the level is reached once per binding above it.
On 3k items / 300 users / dim 16 with a broad pattern and k=10, level 0
ran at 1.0 ms against post-filter's 14.3 ms, while level 1 slowed to
3.9 ms — 300 visits and ~10k neighbour pops to do the same work. Which
side wins depends on the graph, which is why it is a knob.

Full write-up: `docs/internals/vector-search.md`.
