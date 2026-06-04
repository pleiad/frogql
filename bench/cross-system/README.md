# Cross-system benchmark — the "external bench"

Side-by-side latency for LDBC SNB Interactive Complex queries on
gqlite + a set of external graph systems, against the same SF0.1
dataset and the same substitution-parameter rows. The runner
accepts `--ics <list>`; each invocation runs the selected ICs
across all (selected) systems.

This is the **external** bench in the project's two-bench split:

- **External bench** (this dir): how does gqlite compare to other
  graph databases on user-facing query latency. The headline numbers.
- **Internal bench** ([`bench/INTERNAL_BENCHMARK.md`](../INTERNAL_BENCHMARK.md)):
  how do gqlite's own components (the typechecker today; potentially
  other compiler microbenches later) perform in isolation. Engine
  diagnostics, not engine comparisons.

`src/bin/ldbc_bench.rs` predates this split — it ran LDBC ICs on
gqlite alone with backend selection (`memory|lazy|disk`) and
ablation env vars. Those were diagnostic; the cross-system bench's
`--ablate` covers the part that matters for the external comparison
(auto-indexes on/off). `ldbc_bench` stays in the tree but isn't the
focus going forward.

> **Reading this for the first time?** Three companion docs.
> [`SURVEY.md`](SURVEY.md) — single-page narrative covering every
> system we evaluated (working AND rejected), why we picked the ones
> we did, what's intentionally out of scope. [`QUERIES.md`](QUERIES.md)
> — the methodology behind every IC translation: fairness across
> systems, spec-faithfulness in two dimensions, the divergence
> taxonomy, the audit process. This README is the operational doc —
> how to set up, run, and read the results.

## What gets compared

| System | Subdir | Status |
|---|---|---|
| gqlite (lazy backend) | [`gqlite/`](gqlite/) | ✅ implemented |
| GraphQLite — colliery-io/graphqlite (Cypher, SQLite-backed) | [`graphqlite/`](graphqlite/) | ✅ implemented; see [`graphqlite/DIVERGENCES.md`](graphqlite/DIVERGENCES.md) for the int64 RETURN-projection bug surfaced by the row-equivalence oracle |
| Kuzu — kuzudb (vectorized columnar engine, CIDR 2023; pinned to v0.11.3 since [upstream archived 2025-10-10](https://github.com/kuzudb/kuzu)) | [`kuzu/`](kuzu/) | ✅ implemented; see [`kuzu/DIVERGENCES.md`](kuzu/DIVERGENCES.md) for the archival-status framing and the `label()`-predicate query-shape divergence (Kuzu's optimizer doesn't push `label()` through multi-hop joins → IC8 ~14s/iter, an honest finding) |
| Grafeo — GrafeoDB/grafeo (GQL-native, vectorized/SIMD; pinned to v0.5.34) | [`grafeo/`](grafeo/) | ✅ implemented — all six ICs (2,5,6,8,9,11) row-equivalence verified byte-identical to gqlite; see [`grafeo/README.md`](grafeo/README.md) for the dialect map and [`grafeo/DIVERGENCES.md`](grafeo/DIVERGENCES.md) (multi-MATCH fold, WHERE/OPTIONAL ordering, label-alternation → WHERE, Company/Country via `type`) |
| GraphLite — GraphLite-AI/GraphLite (ISO GQL, Sled-backed) | — | not yet integrated |
| GQLite — webbery/gqlite (custom DSL, dead since April 2023) | — | not yet integrated |

**IC coverage.** The harness now defaults to every IC frogQL implements
(IC1,2,3,4,5,6,7,8,9,11,12,13; IC10 and IC14 remain blocked). The six
original cross-system ICs (2,5,6,8,9,11) are row-equivalence verified;
the newer six (1,3,4,7,12,13) ship Kuzu/Grafeo translations that the
author could not validate offline — the server run's row-hash oracle is
the verification gate. ICs that project list/struct columns (IC1, IC7,
IC12) are expected to **diverge on the row hash** by column encoding
even when logically correct, so they are kept primarily for the
**latency + memory** signal; per-IC notes live in each system's
`DIVERGENCES.md`.

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

### On a server (recommended entry point)

For an unattended run on a separate Linux box — frogQL vs Kuzu vs
Grafeo on **every implemented IC**, measuring **latency and peak
memory** with a hard **10 GiB per-runner cap** — use the server
driver. It builds the gqlite binaries, downloads/builds the LDBC
dataset if missing, optionally installs the external engines' Python
deps, then drives `run_all.sh` and tees a durable log:

```bash
# One-shot, unattended (logs to the file AND results/<ts>.server.log):
nohup bench/cross-system/run_server.sh --install-deps \
    > /tmp/frogql-bench.out 2>&1 &
tail -f /tmp/frogql-bench.out

# Defaults: --systems gqlite,kuzu,grafeo, all implemented ICs,
#           --iters 10 --warmup 2 --mem-limit-gb 10.
# Override any of them; unknown flags pass through to run_all.sh:
bench/cross-system/run_server.sh --systems gqlite,kuzu --ics 2,3,4 \
    --mem-limit-gb 8 --rebuild-data
bench/cross-system/run_server.sh --help
```

### Memory cap + measurement (10 GiB default)

Every runner invocation is wrapped in `_lib/memrun.py`, a pure-stdlib
(`/proc`-based, no psutil) monitor that:

- samples the runner's **whole process-group RSS** every 50 ms and
  records the **peak**;
- **SIGKILLs the group** the instant the peak crosses the cap
  (`--mem-limit-gb`, default 10), recording a `memory_error` instead of
  letting a runaway query take the host down;
- writes a per-(system, IC) `key=value` summary, folded into
  `results/<ts>/memory.csv` and surfaced in `comparison.txt`.

A `memory_error` row in `memory.csv` / `skipped.log` (`[MEMLIMIT]`) is
the headline for "this query couldn't run in 10 GiB on this engine" —
a real finding, not a gap to hide.

### Lower-level: run_all.sh directly

```bash
# Default: every implemented IC against every implemented system.
bench/cross-system/run_all.sh

# Multi-IC sweep, all in one invocation:
bench/cross-system/run_all.sh --ics 2,3,11

# Subset of systems (useful while iterating on a per-system runner):
bench/cross-system/run_all.sh --only gqlite
bench/cross-system/run_all.sh --only gqlite,graphqlite

# Lower the per-runner memory cap to 4 GiB:
bench/cross-system/run_all.sh --mem-limit-gb 4

# Tune iteration count:
bench/cross-system/run_all.sh --iters 30 --warmup 3

# Force a clean re-load of every system's DB before benching
# (captures real setup time; default skips setup if DB exists):
bench/cross-system/run_all.sh --rebuild-setup

# Ablation mode — gqlite runs twice (baseline + LTJ index-fold
# disabled), each emitted with a distinct `backend` label. Other
# systems run normally.
bench/cross-system/run_all.sh --ablate
bench/cross-system/run_all.sh --ablate --only gqlite
```

**Ablation mode** (`--ablate`): one knob, two modes:

| Mode | Env | Tests |
|---|---|---|
| `lazy-baseline` | (none) | All optimizations on (LTJ + auto-indexes + index folding + TripleIndex cache) |
| `lazy-no-fold` | `GQLITE_DISABLE_INDEX_FOLD=1` | Disable LTJ index-driven constant folding (the pre-pass that turns `MATCH (n {id:X})` into a single-NodeId pre-bind). The auto-indexes are still built; they just don't get used in the LTJ pre-pass. |

This is the only gqlite-internal knob the external bench tracks —
the surgical "did the LTJ fold optimization actually buy anything"
comparison. Other gqlite ablations (lazy-vs-disk RAM tradeoff, full
auto-index disable, etc.) belong in the internal bench, not here.

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
- `memory.csv` — per (system, IC): `status` (ok / memory_error /
  runner_error), peak RSS (MiB), cap (MiB), wall seconds. Written from
  the `memrun.py` monitor; `memory_error` = killed at the cap.
- `<system>.ic<n>.mem.txt` — raw `memrun.py` key=value summary per run
- `<ts>.server.log` — full tee'd output when launched via `run_server.sh`
- `comparison.txt` — `compare_results.py` output (latency table +
  count/shape consistency check + side-by-side comparison + memory +
  cap table + per-(system, IC) shape pass/fail)
- `setup_times.txt` — per-system load wall time **and on-disk DB
  size** (`db_bytes` column). Sizes are measured against each
  system's `marker` path (see `SETUP_MARKER` in `run_all.sh`); for
  Kuzu this is the single `.db` file, not the sibling
  `_kuzu_work/` working dir.
- `<system>.setup.log`, `<system>.ic<n>.stderr.log` — per-stage logs
- `skipped.log` — any (system, IC) pair that couldn't run
- `run_info.txt` — timestamp, host, gqlite commit, etc.

## The queries

Each IC's canonical form lives in `bench/ldbc-queries/icN.toml` —
the same TOML our regular `ldbc_bench` consumes. Per-system Cypher
files (`graphqlite/icN.cypher`, `kuzu/icN.cypher`) are translations
of the toml; each one's first comment-block points back to the toml.

The shipped queries are **spec-faithful**: ORDER BY, COALESCE, label
disjunction, bare-node compare, variable-length hops, sub-labels for
Company/Country. Currently implemented: IC2, IC5, IC6, IC8, IC9, IC11.
Still blocked by missing parser features: IC1, IC3, IC4, IC7, IC10,
IC12, IC13, IC14 — each blocked toml's `blocked_reason` field lists
the specific gaps.

The methodology behind these translations — what fairness means
across systems, how we taxonomize divergences (loader-level vs
dialect-level vs parser-gap vs semantic), and how the audit proceeds
when parser features land — is in [`QUERIES.md`](QUERIES.md). Read
that if you want to extend the IC catalog or argue with a particular
divergence.

Per-system divergences spanning multiple ICs (Kuzu's `label()`
predicate, graphqlite's `.ldbcId` accessor, etc.) are documented in
each subdir's `DIVERGENCES.md`.

## Reading the results

`comparison.txt` has five sections, each rendered per IC for
multi-IC runs:

1. **Per-cell summary** — for each (params_row, system) pair, median
   latency, p95, iter count, and the result_count.
2. **Result-count consistency** — for each params_row, do all systems
   agree on row count? With ORDER BY in the canonical toml the
   row contents are deterministic, so counts must match exactly
   across systems. `WARN` flags disagreement.
3. **Side-by-side latency** — one row per params_row, one column per
   system, median ms.
4. **Memory footprint** — peak RSS during the query loop per
   (system, IC), parsed from `*.stderr.log` files. The `over
   baseline` column subtracts the runner's at-startup RSS so the
   delta is roughly "engine + DB state" across runners (see Notes
   in the section header for caveats — graphqlite's RSS is small
   because SQLite uses mmap; data lives in OS page cache, not
   process RSS).
5. **Row-content equivalence** — per (IC, params_row), do all
   systems produce byte-identical canonical rows? Each runner
   sha256-hashes its iter-0 result and emits a `ROW row=N count=N
   shape=<...> hash=<hex>` stderr line; this section compares the
   hashes across systems. Mismatch → real per-system translation
   bug; the section points at the sibling `<system>.icN.rows.jsonl`
   files for diff. With ORDER BY in every toml the iter-0 result
   is deterministic, so byte-equal blobs across systems mean
   byte-equal results. Hash subsumes a per-column-type shape check
   — any column-count or per-cell-type drift changes the hash, so
   the older "Shape verification" section was retired.

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
- **Same row-content oracle.** Every system's runner emits
  `ROW row=N count=N shape=<sig> hash=<sha256-hex>` lines plus a
  sibling `<system>.icN.rows.jsonl` envelope per params-row. The
  hash is the cross-system equivalence check; with ORDER BY in
  every toml the iter-0 results are deterministic, so byte-equal
  rows → identical hashes.

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
   for the label-disjunction step: `(c:Comment|Post)` (gqlite, ISO
   GQL pattern alternation), `WHERE c:Comment OR c:Post` (graphqlite,
   Cypher 4.x label predicate), `WHERE label(c) IN ["Comment","Post"]`
   (Kuzu, single-NODE-TABLE-per-node data model means it has no
   `node:Label` predicate; the `label()` builtin is the closest
   spec-faithful equivalent). All three are query-level predicates,
   semantically equivalent. Per-system shape divergences are listed
   in each subdir's `DIVERGENCES.md`.
4. **Setup time.** Reported in `setup_times.txt` per system.
   gqlite's setup is user-managed (the `bench_setup` binary can't
   be invoked from MSYS bash on Windows due to UAC); it shows
   "user-managed" in the table. The other systems' setup is invoked
   directly by `run_all.sh` and timed.
5. **Pinned versions.** kuzu is pinned to `0.11.3` (the upstream
   project archived 2025-10-10; the wheel is frozen and reproducible).
   graphqlite is pinned to `0.4.4`. gqlite is whatever the bench
   branch builds.

### What's INTENTIONALLY not measured

- Cold-cache (first-iter) vs warm-cache breakdown — every runner
  takes `--warmup` iters and discards them; only the warm path is
  in the per-iter CSV.
- Multiple scale factors (SF1, SF10, SF100). LDBC SF0.1 is the
  smallest; we'd add larger SFs if a finding requires them.
- Concurrency / multi-thread query throughput.

(Memory footprint *is* measured — peak RSS during the query loop,
in section 4 of `comparison.txt`. graphqlite's RSS is small because
SQLite uses mmap; data lives in OS page cache, not process RSS, so
the cross-system column isn't strictly apples-to-apples — see the
section's own caveats.)

## Out of scope

- Other ICs (IC1, IC3...IC14, BI*) — adding them is mechanical
  (new translation file per system) but defer until requested.
- LDBC-driver-mediated audited compliance — that's a different
  deliverable (~3 weeks more work). This bench is research-paper-tier.
- CI integration — bench machines vary too much to threshold on.

