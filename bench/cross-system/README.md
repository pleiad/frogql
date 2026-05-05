# Cross-system benchmark

Side-by-side latency for LDBC SNB Interactive Complex queries on
gqlite + a set of external graph systems, against the same SF0.1
dataset and the same substitution-parameter rows. Currently only IC2
is wired up (it's the only IC whose `bench/ldbc-queries/ic<n>.toml`
has `status = "implemented"`); more come online as the parser gains
features. The runner accepts `--ic <n>`; each invocation runs one IC
across all (selected) systems.

> **Reading this for the first time?** [`SURVEY.md`](SURVEY.md) is
> the single-page narrative covering every system we evaluated
> (working AND rejected), the architectural pattern across the
> rejections, and what's intentionally out of scope. This README is
> the operational doc — how to set up, run, and read the results.

## What gets compared

| System | Subdir | Status |
|---|---|---|
| gqlite (lazy backend) | [`gqlite/`](gqlite/) | ✅ implemented |
| GraphQLite — colliery-io/graphqlite (Cypher, SQLite-backed) | [`graphqlite/`](graphqlite/) | ✅ implemented |
| Kuzu — kuzudb (vectorized columnar engine, CIDR 2023; pinned to v0.11.3 since [upstream archived 2025-10-10](https://github.com/kuzudb/kuzu)) | [`kuzu/`](kuzu/) | ✅ implemented; see [`kuzu/DIVERGENCES.md`](kuzu/DIVERGENCES.md) for the archival-status framing and the `UNION ALL` query-shape divergence |
| GraphLite — GraphLite-AI/GraphLite (ISO GQL, Sled-backed) | — | not yet integrated |
| GQLite — webbery/gqlite (custom DSL, dead since April 2023) | — | not yet integrated |

## Setup

### 1. Shared dataset

The bench depends on the same LDBC SF0.1 dataset our regular
`ldbc_bench` uses. From the repo root:

```bash
cargo build --release
# Linux/macOS:
./target/release/bench_setup
# Windows (PowerShell or cmd, NOT MSYS bash — see "Windows note" below):
cargo run --release --bin bench_setup
```

That produces:
- `bench/data/ldbc-sf0.1.gdb` — gqlite's binary (used by `gqlite/run.sh`)
- `bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter/`
  — raw LDBC CSVs (used by external systems' loaders)
- `bench/data/substitution_parameters-sf0.1/.../interactive_2_param.txt`
  — 15 IC2 param rows (`personId|maxDate`)

> **Windows note**: the binary is named `bench_setup.exe`, which
> Windows treats as an installer (`setup` in the name triggers UAC
> elevation). Invoking from MSYS bash fails with `os error 740`.
> Run it from PowerShell or cmd.exe instead, OR via `cargo run`
> in a non-MSYS shell. `run_all.sh` does NOT auto-rebuild gqlite's
> `.gdb`; it verifies that the file exists and instructs you to
> rebuild manually if missing.

### 2. Per-system prerequisites

Each external system has its own install line. To install all of
them in one go:

```bash
bash bench/cross-system/install_python_deps.sh
```

That script just runs `pip install -r requirements.txt` in each
implemented per-system subdir. If you'd rather install them
piecemeal, they're:

| System | Install command | Other prerequisites |
|---|---|---|
| `gqlite` | `cargo build --release` (covered by step 1) | none |
| `graphqlite` | `pip install -r bench/cross-system/graphqlite/requirements.txt` | none |
| `kuzu` | `pip install -r bench/cross-system/kuzu/requirements.txt` | none — `kuzu==0.11.3` pinned, archived but reproducible |

Each per-system subdir has its own `README.md` documenting prereqs,
CLI, and any per-system gotchas. Failing-but-scaffolded systems
(e.g. on the `bench/cross-system-failed-attempts` branch) ship a
`DIVERGENCES.md` that explains why their integration didn't pan out.

### 3. Per-system data load

`run_all.sh` orchestrates the data-load step automatically per
system: for each system in turn, it invokes the system's `setup.py`
(or, for gqlite, verifies the `.gdb` is present), then runs each
requested IC. By default setup is skipped if the system's database
already exists; pass `--rebuild-setup` to force a clean reload.

The setup loads the FULL LDBC SF0.1 dataset for each system —
every node and edge type, including Multi-Valued Attributes
(`person_email_emailaddress`, `person_speaks_language`) as
list-typed properties on Person. This means setup is IC-agnostic:
adding a new IC translation (`ic<n>.cypher` per system) does NOT
require any setup-script changes. Same DB, all ICs.

To pre-load explicitly (or to time setup separately):

```bash
python bench/cross-system/graphqlite/setup.py --force
python bench/cross-system/kuzu/setup.py --force
```

## Running

```bash
# Default: IC2 against every implemented system.
bench/cross-system/run_all.sh

# Multi-IC sweep, all in one invocation:
bench/cross-system/run_all.sh --ics 2,3,11

# Subset of systems (useful while iterating on a per-system runner):
bench/cross-system/run_all.sh --only gqlite
bench/cross-system/run_all.sh --only gqlite,graphqlite

# Tune iteration count:
bench/cross-system/run_all.sh --iters 30 --warmup 3

# Force a clean re-load of every system's DB before benching
# (captures real setup time; default skips setup if DB exists):
bench/cross-system/run_all.sh --rebuild-setup

# Ablation mode — gqlite runs in three modes (baseline + two
# disabled-optimization variants), each emitted as a separate
# `backend` label. Other systems run normally; the comparison
# table renders the ablation modes as additional columns.
bench/cross-system/run_all.sh --ablate
bench/cross-system/run_all.sh --ablate --only gqlite   # ablation table only
```

**Ablation mode** (`--ablate`): when set, gqlite runs four times
across two axes (optimization knobs on the lazy backend, plus the
disk backend as a separate storage shape) and emits per-iter rows
with distinct backend labels:

| Mode | Env / args | Tests |
|---|---|---|
| `lazy-baseline` | (none) | All optimizations on (LTJ + auto-indexes + index folding + TripleIndex cache) |
| `lazy-no-auto-indexes` | `GQLITE_DISABLE_AUTO_INDEXES=1` | Skip the `(label, prop)` secondary-index auto-build at open |
| `lazy-no-fold` | `GQLITE_DISABLE_INDEX_FOLD=1` | Disable LTJ index-driven constant folding (the pre-pass that turns `MATCH (n {id:X})` into a single-NodeId pre-bind) |
| `disk-baseline` | `--backend disk` | DiskGraphStore (no LRU page cache, no secondary indexes today) — RAM/disk tradeoff vs lazy |

The ablation modes show up as additional `backend` columns in
`comparison.txt` so the same `compare_results.py` machinery
renders them — no separate ablation script. See
[`SURVEY.md`](SURVEY.md#ablation-results) for the headline
findings (which optimization buys what).

The orchestrator iterates **systems on the outer loop, ICs on the
inner**: for each system, set up once (loading full LDBC SF0.1),
then run all requested ICs against that DB, then move on. Memory
is reclaimed naturally between systems via process exit — no
shared state, no concurrent system DBs in memory at once.

Output lands in `bench/cross-system/results/<timestamp>/`:
- `<system>.ic<n>.csv` per (system, IC) — raw per-iter rows in
  schema `query;backend;params;row;iter;result_count;elapsed_ns`
- `cross_system.csv` — concatenation of all the above
- `comparison.txt` — `compare_results.py` output (latency table +
  count/shape consistency check + side-by-side comparison)
- `setup_times.txt` — per-system load wall time (when measurable)
- `<system>.setup.log`, `<system>.ic<n>.stderr.log` — per-stage logs
- `skipped.log` — any (system, IC) pair that couldn't run
- `run_info.txt` — timestamp, host, gqlite commit, etc.

## The query

The canonical IC2 lives in [`bench/ldbc-queries/ic2.toml`](../ldbc-queries/ic2.toml)
— same TOML our regular `ldbc_bench` consumes. Per-system harnesses
translate it into their native query syntax (`graphqlite/ic2.cypher`,
`graphlite/ic2.gql`, etc.); each translation file's first comment
points back to the toml.

The toml documents divergences from the LDBC spec (no ORDER BY, no
`coalesce`, lowercase edge labels) — these are gqlite parser
limitations. Per the plan: every system runs **our** divergent IC2,
not the spec version. That keeps the comparison apples-to-apples
even though the other systems could technically execute spec IC2.
The doc-pointer convention makes this honest: if you read
`graphqlite/ic2.cypher` and wonder why it doesn't have `ORDER BY`,
the comment-link explains.

## Reading the results

`comparison.txt` has three sections:

1. **Per-cell summary** — for each (params_row, system) pair, median
   latency, p95, iter count, the result_count, and the result_shape
   (per-row type signatures, deduped — e.g. `i,s,s,i,s,i|i,s,s,i,n,i`
   for IC2 where `c.content` is sometimes null).
2. **Count + shape consistency** — for each params_row, do all systems
   agree on row count AND per-row column types? Without ORDER BY the
   actual row contents legitimately differ (each system picks a
   different N rows from the full result), but the column count and
   types must match. `WARN` flags disagreement, which means a
   per-system query translation bug.
3. **Side-by-side latency** — one row per params_row, one column per
   system, median ms.

## Measurement basis (read this before quoting numbers)

Cross-system bench numbers are useful only if you understand what's
being measured. Every choice that's not strictly identical across
systems is documented here so reviewers can interpret the numbers
honestly.

### What's IDENTICAL across systems

- **Same dataset.** LDBC SNB SF0.1, all entities, all edges, all MVAs.
  Each system's `setup.py` (or, for gqlite, `bench_setup`) loads the
  full dataset — no per-IC subset fragmentation.
- **Same parameter rows.** All 15 LDBC IC2 substitution params; same
  `personId|maxDate` values fed to every system.
- **Same IC translation, structurally.** Each per-system
  `ic<n>.cypher` (or `.gql`) is a translation of the canonical query
  in `bench/ldbc-queries/ic<n>.toml`. Per-system divergences from
  that canonical shape are documented in each subdir's
  `DIVERGENCES.md`.
- **Same warmup + iter counts.** Every system gets the same `--warmup`
  iters discarded before measurement and the same `--iters` measured.
- **Same result-shape verification.** Every system's runner emits
  `SHAPE row=N count=N shape=<sig>` lines compared against the toml's
  `expected_shape`; `run_all.sh` tallies pass/fail per system.

### What's DIFFERENT across systems (and why)

1. **FFI overhead.** gqlite is benched via the Rust `ldbc_bench` binary
   — the per-iter timer wraps `Runtime::run_query()`, no Python in the
   path. graphqlite and Kuzu are benched through their Python wheels;
   their per-iter timer wraps `g.query(...)` / `conn.execute(...)`,
   which includes ~1-2ms of Python ↔ C/C++ FFI overhead per call.
   We deliberately measure each system through its **primary
   user-facing interface** (gqlite's CLI/Rust, the others' Python
   wrappers) rather than artificially inflating gqlite's number to
   "match" the others. A user picking gqlite would write Rust; a user
   picking Kuzu would write Python. The numbers reflect that.
2. **Compile (parse + plan) cost.** gqlite compiles the query once
   per param row outside the timed loop; the timer measures
   execution only. Kuzu and graphqlite handle this internally via
   plan caches keyed by query string — repeated calls don't re-parse.
   No system pays per-iter compile cost in the measurement.
3. **Query shapes within IC2.** Each system uses its native idiom
   for the label-disjunction step (`(c:Comment|Post)` for gqlite,
   `WHERE c:Comment OR c:Post` for graphqlite, multi-typed REL TABLE
   `(c)` unlabeled for Kuzu). Different shapes → different optimizer
   paths within each engine, but logically the same query. We do NOT
   constrain engines to a foreign shape. Per-system shape divergences
   are listed in each subdir's `DIVERGENCES.md`.
4. **No ORDER BY anywhere.** The canonical IC2 in
   `bench/ldbc-queries/ic2.toml` drops `ORDER BY` because gqlite's
   parser doesn't support it yet. We apply the same drop to every
   system for fairness with our own engine. **This means our IC2
   numbers are NOT comparable to published LDBC IC2 numbers** (which
   include ORDER BY). Document this anywhere external comparison is
   made.
5. **Setup time.** Reported in `setup_times.txt` per system.
   gqlite's setup is user-managed (the `bench_setup` binary can't
   be invoked from MSYS bash on Windows due to UAC); it shows
   "user-managed" in the table. The other systems' setup is invoked
   directly by `run_all.sh` and timed.
6. **Pinned versions.** kuzu is pinned to `0.11.3` (the upstream
   project archived 2025-10-10; the wheel is frozen and reproducible).
   graphqlite tracks its latest PyPI release. gqlite is whatever
   the bench branch builds.

### What's INTENTIONALLY not measured

- Memory footprint
- Cold-cache (first-iter) vs warm-cache breakdown
- Multiple scale factors (SF1, SF10, SF100). LDBC SF0.1 is the
  smallest; we'd add larger SFs if a finding requires them.
- Concurrency / multi-thread query throughput

## Out of scope

- Other ICs (IC1, IC3...IC14, BI*) — adding them is mechanical
  (new translation file per system) but defer until requested.
- Spec-faithful IC2 (ORDER BY, coalesce) — needs gqlite parser
  features first; revisit when those land.
- LDBC-driver-mediated audited compliance — that's a different
  deliverable (~3 weeks more work). This bench is research-paper-tier.
- CI integration — bench machines vary too much to threshold on.

