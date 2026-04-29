# LDBC SNB Interactive Benchmark

Replicates a subset of the LDBC Social Network Benchmark Interactive
workload (arXiv:2001.02299, §6) against gqlite.

## Status

Currently runs **IC2 only** (recent messages by friends). All 14 IC
queries are catalogued under `bench/ldbc-queries/ic<n>.toml` — IC2
is `status = "implemented"`, the other 13 are `status = "blocked"`
with the gqlite features they need listed under `required_features`.
Run `ldbc_bench --ic blocked` for the inventory.

Per-IC blockers at a glance (from the TOML files):

- IC1, IC3, IC5, IC6, IC9, IC10, IC11, IC12 — variable-length paths
- IC1, IC13, IC14 — shortest path / all-shortest-paths
- IC1, IC5, IC7, IC10 — `OPTIONAL MATCH`
- IC3, IC4, IC5 — date arithmetic (date ranges)
- IC3, IC4, IC5, IC6, IC10, IC12 — aggregation (`COUNT`, `GROUP BY`)
- All ICs — `ORDER BY`
- IC2 — `coalesce` (semi-blocking — query runs, but `imageFile`
  fallback for empty Posts isn't honored)

## Architecture: per-IC TOML query catalog

Each IC has a TOML file in `bench/ldbc-queries/`:

```
ic1.toml   ← blocked, lists required_features
ic2.toml   ← implemented, carries query template + params_file ref
ic3.toml   ← blocked
...
ic14.toml  ← blocked
```

Implemented files carry:
- `params_file` — name of the LDBC `interactive_<n>_param.txt` (looked
  up under `--params-dir`)
- `query` — multi-line query template with `{paramName}` placeholders
- `divergences` — per-IC notes on spec deltas (no coalesce, no ORDER BY, etc.)

Blocked files carry:
- `blocked_reason` — prose explaining why we can't run it
- `required_features` — list of gqlite feature gaps

Adding a new IC = drop a TOML file in the directory and (if implemented)
make sure the query parses against gqlite. No bench code changes for
adding queries; the runner discovers them at startup.

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

gqlite query (this bench, post `loader/ldbc-id-property`):

```
MATCH (p:Person {id: $personId})~[:knows]~(friend:Person)<-[:hasCreator]-(c:Comment)
    | (p:Person {id: $personId})~[:knows]~(friend:Person)<-[:hasCreator]-(c:Post)
WHERE c.creationDate <= $maxDate
RETURN friend.id, friend.firstName, friend.lastName,
       c.id, c.content, c.creationDate
```

The `(:Person {id: $personId})` form is verbatim spec syntax; gqlite
parses it and elaborates it to `(:Person) WHERE x.id = $personId`
internally. The `maxDate` predicate stays in `WHERE` because `<=` is
not equality and the descriptor shorthand only handles equality.

### Divergences from the spec query

Two real differences from the IC2 spec's text:

| # | Spec | gqlite (this bench) | Effect |
|---|---|---|---|
| 1 | `coalesce(message.content, message.imageFile)` | `c.content` only | Image-only Posts return blank instead of the imageFile path. gqlite has no `coalesce` builtin. |
| 2 | `ORDER BY creationDate DESC, id ASC` | (no ordering) | The 20 rows under `LIMIT 20` are *some* 20, not the 20 most recent. gqlite parser doesn't have ORDER BY yet. |

Everything else (the `(p: Person {id: …})` anchor, the `Comment | Post`
union, the `<= maxDate` predicate, the LIMIT, the `:knows`/`:hasCreator`
edge labels from the loader, the 15 substitution parameters from
LDBC's official `interactive_2_param.txt`) is the same shape and same
data as the spec — modulo gqlite's grammar.

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

### 1b. Download substitution parameters

The dataset above was built with `parametergenerator.parameters:false`
(check `params.ini`), so the substitution parameters aren't bundled.
LDBC ships them as a separate archive — same source, same SF, same
seed:

```bash
cd bench/data
curl -L -o substitution_parameters-sf0.1.tar.zst \
    https://datasets.ldbcouncil.org/snb-interactive-v1/substitution_parameters-sf0.1.tar.zst

python - <<'EOF'
import zstandard, tarfile
with open('substitution_parameters-sf0.1.tar.zst','rb') as f, \
     zstandard.ZstdDecompressor().stream_reader(f) as r, \
     tarfile.open(fileobj=r, mode='r|') as t:
    t.extractall(path='substitution_parameters-sf0.1')
EOF
```

The IC2 params file ends up at
`bench/data/substitution_parameters-sf0.1/substitution_parameters-sf0.1/interactive_2_param.txt`
and contains 15 `(personId, maxDate)` pairs. The bench's `PARAMS`
constant is the verbatim contents of that file (with names resolved
from `person_0_0.csv` purely for human-readable output).

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

# IC2 against Lazy (the default — mirrors REPL/Python behavior):
./target/release/ldbc_bench bench/data/ldbc-sf0.1.gdb --ic 2 --iters 3

# Same query against Disk:
./target/release/ldbc_bench bench/data/ldbc-sf0.1.gdb --ic 2 --backend disk --iters 3

# All currently-implemented ICs (skips blocked ones):
./target/release/ldbc_bench bench/data/ldbc-sf0.1.gdb --ic all

# Inventory of blocked ICs and their required_features (no run):
./target/release/ldbc_bench placeholder --ic blocked
```

Flag reference:
- `--ic <n>|N,M|all|blocked` — which IC(s) to run; default `2`
- `--backend memory|lazy|disk` — which GraphAccess; default `lazy`
- `--params-dir <dir>` — override default location of LDBC param files
- `--queries-dir <dir>` — override default `bench/ldbc-queries`
- `--iters N` — measured iterations per param row
- `--limit N` — `LIMIT` for `Runtime::run_query`
- `--csv-dir <dir>` — required only with `--backend memory`

Output is two streams:
- **stdout**: machine-readable CSV
  `query;backend;ic;param_idx;iter;result_count;elapsed_ns`.
- **stderr**: per-row summary (min / median / mean / max in ms when
  `--iters >= 2`; `wall=` only when `N=1`).

## Results

Run on Windows (rustc 1.95.0, release profile), SF0.1 dataset
(327k nodes, 1.48M edges), `limit=20`. Substitution parameters from
the official LDBC `interactive_2_param.txt` archive (15 entries).

### Backends compared: Lazy and Disk

The bench supports three backends — `Memory`, `Lazy`, `Disk` — but
**the headline comparison is Lazy vs Disk only**. The `Memory`
backend (`Graph` loaded from CSV with everything in RAM) is included
as a smoke-test option in the bench code (`--backend memory`) but
**not in the paper** because it's an unrealistic configuration for
a database product: it requires the entire dataset to fit in process
memory and re-parses the CSV at every startup. gqlite's actual
embedded-DB story is Lazy and Disk against the `.gdb` file format.

```
# Lazy backend (default) — open .gdb with LazyGraphStore
ldbc_bench bench/data/ldbc-sf0.1.gdb --ic 2 --backend lazy --iters 3

# Disk backend — open .gdb with DiskGraphStore
ldbc_bench bench/data/ldbc-sf0.1.gdb --ic 2 --backend disk --iters 3
```

### Speed vs RAM: Lazy vs Disk (IC2, 1 iter per param)

| backend | load time | load RSS Δ | per-param median | per-param range | peak RSS Δ |
|---|---|---|---|---|---|
| lazy | 7.13 s | +392 MiB | 8.42 s | 7.24 – 10.39 s | +399 MiB |
| disk | **1.74 s** | **+318 MiB** | **7.71 s** | 7.27 – 8.46 s | +340 MiB |

### Findings

- **Disk is slightly faster than Lazy on this workload** (~10%)
  despite Lazy having an in-process LRU page cache. The **OS page
  cache is doing most of the caching anyway** at SF0.1 scale, so
  Lazy's process-level cache adds bookkeeping without much extra
  benefit. (This may flip at larger scales where the OS cache can't
  hold the working set.)
- **Lazy has the higher variance** (3.1 s spread) — first few params
  are colder; the LRU cache fills up and warms up over the run.
  Disk has the tightest spread (1.2 s) — it never has cache to warm,
  so each query is roughly the same cost.
- **Disk loads fastest** (1.74 s vs Lazy's 7.13 s) because it doesn't
  build the label index in RAM at open time. Lazy pre-populates
  topology + label index up front.
- **Disk uses the least RAM** (+318 MiB), Lazy more (+399 MiB peak —
  index + topology + LRU cache).

### Caveat on RSS measurement

`sysinfo` reports OS working-set on Windows, which includes mmap'd
file pages. The **relative** Lazy-vs-Disk delta (~80 MiB, the
in-process label index + LRU cache) is meaningful; absolute values
are confounded by mmap.

### Per-param times (Lazy backend, all 15 params)

For completeness, here's what every param did under the default
backend. The table reports a single iter; the per-param spread
reflects mostly caller-degree variation (more friends → more join
work) plus cache warmup on the first 1-2 params.

| `Person.id`      | resolves to     | rows | wall time |
|------------------|-----------------|------|-----------|
| 19 791 209 300 143 | Bichang Li     | 20   |   7.77 s |
| 10 995 116 278 647 | Ali Kountche   | 20   |   7.55 s |
| 32 985 348 834 326 | Eduard Eduard  | 20   |   7.24 s |
| 30 786 325 579 117 | Chunlai Wang   | 20   |   7.53 s |
|              1 644 | Paul Roberts   | 20   |   7.73 s |
|  6 597 069 766 983 | A. C. Bos      | 20   |  10.39 s |
|  8 796 093 023 470 | Carlos Parra   | 20   |  10.11 s |
|  2 199 023 256 520 | Lei Liu        | 20   |   9.45 s |
| 26 388 279 067 159 | John Brown     | 20   |   9.31 s |
| 28 587 302 323 283 | John Sharma    | 20   |   9.31 s |
| 21 990 232 556 837 | Bing Li        | 20   |   8.62 s |
| 28 587 302 322 755 | Wei Huang      | 20   |   9.02 s |
| 26 388 279 067 442 | Lin Li         | 20   |   8.42 s |
| 24 189 255 811 500 | Dominic Santos | 20   |   8.44 s |
| 15 393 162 790 221 | Pierre Arnaud  | 20   |   7.76 s |

All 15 params returned 20 rows (the LIMIT). Wall times are still
far above LDBC's sub-second interactive target — root cause is the
optimizer gap explained below ("Things this surfaces"), independent
of which backend is used.

Cold-cache (first run after machine restart, .gdb not yet resident
in OS cache) is roughly **10× slower** — 50-150 s/param vs the 7-10 s
warm numbers above. Re-run after a restart if you need cold numbers.

## Why typechecker is off

LDBC IC queries are well-formed by construction. Skipping the
typechecker isolates runtime cost — comparing typecheck overhead vs
runtime is the *separate* typechecker benchmark.

## Things this surfaces

The current optimizer doesn't push down value-equality WHERE predicates
into descriptors (only `attr is type` predicates push down per the
optimizer doc). For `WHERE p.id = $personId AND c.creationDate <= $maxDate`,
this means:

1. Enumerate every `Person~knows~Person` pair (14k undirected edges
   × 2 directions ≈ 28k base rows).
2. Union-join each onto every `Comment-hasCreator->Person` (151k edges)
   and `Post-hasCreator->Person` (135k edges) — ~286k right side.
3. *Then* filter by `p.id = ...` and `c.creationDate <= ...`.

So per-query work scales with the join's cartesian intermediate,
not the parameter's selectivity. This is what makes IC2 here run in
~100s rather than the sub-second times the LDBC spec assumes. That's
a real finding — predicate pushdown on `=` constants would collapse
this into a single `p` (1 row), dropping the join's left side from
28k to 1, and the right side from 286k to ~friends-of-1 = ~150 rows.
Estimated ~100× speedup.

**Fix:** out of scope here (bench branch, no impl changes). The fix
would touch `src/optimizer/pushdown.rs` (extract `var.attr op literal`),
`src/syntax/descriptor.rs` (carry post-elab predicates), and
`src/runtime/engine.rs` (evaluate per candidate before joining).
Estimated ~100× speedup; tracked as a follow-up impl PR.

## Loader prerequisite

The bench query uses `WHERE p.id = $personId` directly. This requires
the LDBC loader to expose the LDBC `id` column as a queryable property
(as well as as the internal node name). That fix lives on
`loader/ldbc-id-property` and is in the parent commit of this branch;
without it, every IC1-IC14 query that anchors by `id` is unexpressible
against gqlite. See the loader's commit message for the rationale.
