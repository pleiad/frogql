# LDBC SNB Interactive Benchmark

Replicates a subset of the LDBC Social Network Benchmark Interactive
workload (arXiv:2001.02299, §6) against gqlite.

## Status

Currently runs **IC2 only** (recent messages by friends). The remaining
13 IC queries need features gqlite doesn't yet have:

- IC1, IC3, IC11, IC12, IC14 — shortest paths / variable-length paths
- IC4–IC8, IC10, IC11, IC13, IC14 — date arithmetic, OPTIONAL MATCH,
  ORDER BY / TOP-K, complex aggregation with HAVING
- IC9 — close to IC2 shape, but adds `comment.creationDate <= maxDate`
  filter (date predicate path)

IC2 itself is run with two simplifications vs the LDBC spec:

1. Parameter is `Person.firstName` (not `Person.id`). LDBC's `id` field
   is folded into the gqlite node *name* by the loader, so it's not
   addressable as a property. firstName is selective enough on SF0.1
   to cover the lookup pattern.
2. No `ORDER BY ... DESC` — gqlite doesn't support it yet. The
   `LIMIT 20` from the spec is honored via `Runtime::run_query`'s
   limit argument.

The query shape is preserved: 3-node 2-edge chain with one undirected
KNOWS edge and one reverse-direction HAS_CREATOR edge.

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
(327k nodes, 1.48M edges), 3 iterations per param, `limit=20`:

| Param      | min      | median   | mean     | max      |
|------------|----------|----------|----------|----------|
| Mahinda    |  51.5 s  |  56.3 s  |  55.0 s  |  57.1 s  |
| Carmen     |  37.9 s  |  50.0 s  |  64.6 s  | 106.0 s  |
| Bryn       |  84.2 s  |  85.4 s  |  86.0 s  |  88.4 s  |
| Cheng      |  86.6 s  |  95.4 s  |  93.7 s  |  99.1 s  |
| Hồ Chí     | 129.0 s  | (partial — bench killed before second iteration) |

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
optimizer doc). For `WHERE p.firstName = 'Mahinda'`, this means:

1. Enumerate every `Person~knows~Person` pair (14k undirected edges
   × 2 directions ≈ 28k base rows).
2. Join each onto every `Comment-hasCreator->Person` (151k edges).
3. *Then* filter by `p.firstName == 'Mahinda'`.

So per-query work scales with the join's cartesian intermediate, not
the parameter's selectivity. This is what makes IC2 here run in tens
of seconds rather than the sub-second times the LDBC spec assumes.
That's a real finding for a future optimizer pass — predicate pushdown
on `=` constants would collapse this into the firstName-anchored
person, dropping the join's left side from 28k to ≤2 rows.
