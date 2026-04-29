# LDBC SNB Interactive Benchmark

Replicates a subset of the LDBC Social Network Benchmark Interactive
workload (arXiv:2001.02299, §6) against gqlite.

## Status

Currently runs **IC2 only** (recent messages by friends). The remaining
13 IC queries need features gqlite doesn't yet have:

- IC1, IC3, IC11, IC12, IC14 — shortest paths / variable-length paths
- IC4–IC8, IC10, IC11, IC13, IC14 — date arithmetic, OPTIONAL MATCH,
  ORDER BY / TOP-K, complex aggregation with HAVING

## IC2 fidelity vs the spec

LDBC IC2 spec text:

```cypher
MATCH (:Person {id: $personId})-[:KNOWS]-(friend:Person)
      <-[:HAS_CREATOR]-(message:Message)
WHERE message.creationDate <= $maxDate
RETURN friend.id, friend.firstName, friend.lastName,
       message.id,
       coalesce(message.content, message.imageFile),
       message.creationDate
ORDER BY message.creationDate DESC, message.id ASC
LIMIT 20
```

gqlite query (this bench):

```
MATCH (p:Person)~[:knows]~(friend:Person)<-[:hasCreator]-(c:Comment)
    | (p:Person)~[:knows]~(friend:Person)<-[:hasCreator]-(c:Post)
WHERE p.firstName = $firstName
  AND p.lastName  = $lastName
  AND c.creationDate <= $maxDate
RETURN friend.firstName, friend.lastName, c.content, c.creationDate
```

Matched faithfully:

- **`Message = Comment ∪ Post`** via path-pattern union (`|`). Both
  arms tested in one query; `c` binds to whichever matched.
- **`message.creationDate <= $maxDate`** — direct numeric WHERE
  predicate.
- **`LIMIT 20`** — passed via `Runtime::run_query`'s `limit`.
- 3-node 2-edge shape with undirected KNOWS and reverse-direction
  HAS_CREATOR.

Divergences (all due to gqlite features not yet implemented):

- **Anchor by `(firstName, lastName)` pair instead of `id`.** The
  gqlite LDBC loader folds the LDBC `id` column into the internal
  *node name*, not into a property, so `Person.id = $personId` is
  not addressable. The 5 params used here are
  `(firstName, lastName)` pairs that each map to *exactly one*
  Person in SF0.1 — same per-query selectivity as `id`, just
  spelled differently.
- **No `ORDER BY` clause.** gqlite's parser doesn't have ORDER BY
  yet. Output order is whatever the runtime emits. The 20 rows
  returned per query aren't guaranteed to be the 20 *most recent*.
  Wall time is unaffected by sort.
- **Drop `friend.id` and `message.id` from RETURN.** Same loader
  reason as above; both would render as `"NULL"`.
- **`coalesce(message.content, message.imageFile)` collapsed to
  `c.content`.** Posts in SF0.1 mostly have non-empty `content`;
  the few image-only posts will return blank content. gqlite has
  no `coalesce` builtin.

In short: this bench is **IC2-shape-equivalent** with only
projection / sort divergences. The lookup, the join structure,
the message-union, and the date predicate are all faithful.

## Setup

### 1. Download SF0.1 dataset

```bash
mkdir -p bench/data && cd bench/data
curl -L -o ldbc-sf0.1.tar.zst \
    https://datasets.ldbcouncil.org/snb-interactive-v1/social_network-sf0.1-CsvBasic-LongDateFormatter.tar.zst

# Decompress (needs zstandard tool or python -c "import zstandard; ...").
# On Windows without zstd installed:
python - <<'EOF'
import zstandard, tarfile
with open('ldbc-sf0.1.tar.zst','rb') as f, \
     zstandard.ZstdDecompressor().stream_reader(f) as r, \
     tarfile.open(fileobj=r, mode='r|') as t:
    t.extractall(path='ldbc-sf0.1')
EOF
```

The dataset extracts to
`bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter/`
with `dynamic/` and `static/` subdirectories. Total: ~327K nodes,
~1.5M edges.

### 2. Build the gqlite .gdb

```bash
cargo build --release --bin gqlite
./target/release/gqlite bench/data/ldbc-sf0.1.gdb \
    --import-ldbc-csv bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter \
    --no-typecheck
```

Output: `bench/data/ldbc-sf0.1.gdb` (~1.4 GB).

### 3. Run the benchmark

```bash
cargo build --release --bin ldbc_bench
./target/release/ldbc_bench bench/data/ldbc-sf0.1.gdb --iters 5 --limit 20
```

Output is two streams:
- **stdout**: machine-readable CSV `query;param;iter;result_count;elapsed_ns`.
- **stderr**: per-parameter summary (min / median / mean / max in ms).

## Results

Run on Windows (rustc 1.95.0, release profile), SF0.1 dataset
(327k nodes, 1.48M edges), `limit=20`. Single iteration per param
(LazyGraphStore page cache is cold on first iter, warm thereafter;
3+ iters are recommended for stable median — single-iter numbers
below are upper-bound order-of-magnitude):

| Param           | result_count | wall time |
|-----------------|--------------|-----------|
| Mahinda Perera  | 20           |   96.4 s  |
| Carmen Lepland  | 20           |  108.1 s  |
| Bryn Davies     | 20           |   85.0 s  |
| Cheng Yu        | 20           |  105.6 s  |
| Hồ Chí Loan     | 20           |  110.7 s  |

All five params returned 20 rows (the limit). These are **far above**
the LDBC interactive target of sub-second latency; cause is the
optimizer gap explained below.

## Why typechecker is off

LDBC IC queries are well-formed by construction. Skipping the
typechecker isolates runtime cost — comparing typecheck overhead vs
runtime is the *separate* typechecker benchmark.

## Things this surfaces

The current optimizer doesn't push down value-equality WHERE predicates
into descriptors (only `attr is type` predicates push down per the
optimizer doc). For `WHERE p.firstName = '...' AND p.lastName = '...'`,
this means:

1. Enumerate every `Person~knows~Person` pair (14k undirected edges
   × 2 directions ≈ 28k base rows).
2. Union-join each onto every `Comment-hasCreator->Person` (151k edges)
   and `Post-hasCreator->Person` (135k edges) — ~286k right side.
3. *Then* filter by `(firstName, lastName)` and `creationDate`.

So per-query work scales with the join's cartesian intermediate,
not the parameter's selectivity. This is what makes IC2 here run in
~100s rather than the sub-second times the LDBC spec assumes. That's
a real finding for a future optimizer pass — predicate pushdown on
`=` constants would collapse this into the (firstName, lastName)-
anchored person (1 row), dropping the join's left side from 28k to 1.
