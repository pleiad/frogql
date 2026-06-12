# Final benchmark runs for the ISWC revision — server playbook

Everything below was validated end-to-end on a dev machine with
`--iters 1` (single query per system, row-equivalence checked). This
playbook is the full-iteration re-run on the benchmark server. Total
unattended time estimate: 1.5–3 h for the main run (grafeo dominates),
plus the SF3 download/build (~10–20 GB disk) for the scaling sweep.

## 0. One-time setup

```bash
cd <repo>/gqlrust && git pull
cargo build --release            # frogql, ldbc_bench, bench_setup, dml_bench

# Python deps for all six runners (kuzu, graphqlite, grafeo, neo4j
# driver, duckdb, psutil). Use a venv whose `python` is on PATH:
python3 -m venv bench/cross-system/.venv
source bench/cross-system/.venv/bin/activate
pip install -r bench/cross-system/kuzu/requirements.txt \
            -r bench/cross-system/graphqlite/requirements.txt \
            -r bench/cross-system/grafeo/requirements.txt \
            -r bench/cross-system/neo4j/requirements.txt \
            -r bench/cross-system/duckdb/requirements.txt

# Neo4j needs docker. If the server has it, nothing else to do —
# run_server.sh auto-starts the container (neo4j/docker.sh up).
# Without docker, neo4j is dropped from the run automatically.
docker info >/dev/null && echo "docker OK"
```

Notes:
- graphqlite needs a Python whose `sqlite3` allows extension loading
  (stock Debian/Ubuntu python3 is fine; macOS system python is not).
- `bench/data/` artifacts (LDBC CSVs, .gdb) are reused if present from
  previous runs; `run_server.sh` rebuilds anything missing.

## 1. Main cross-system run (review actions R1.5, R2.3, R3.2, R3.3)

13 ICs × {froGQL-lazy, froGQL-disk, GraphQLite, Kuzu, Grafeo, Neo4j,
DuckDB}, 10 iters + 2 warmup, 10 GiB RSS cap, 600 s/runner timeout:

```bash
source bench/cross-system/.venv/bin/activate
nohup bench/cross-system/run_server.sh > /tmp/frogql-bench.out 2>&1 &
tail -f /tmp/frogql-bench.out
```

Outputs land in `bench/cross-system/results/<ts>/`:
`comparison.txt` (per-IC latency tables + row-hash oracle),
`memory.csv` (peak RSS per system×IC), `setup_times.txt`,
`run_info.txt` (now includes cpu_model/cpu_cores/ram_gb).

Expected skips (visible in `skipped.log`, not errors):
- graphqlite on IC1/3/4/7/12/13 (no translations — documented carve-out)
- duckdb on everything except IC2/6/11 (in-situ baseline is 3 ICs by design)

## 2. Ablation run (R1.1, R2.1 — "which optimization buys what")

gqlite-only, 5 modes (baseline, no-fold, no-unroll, no-pin, no-bfs):

```bash
bash bench/cross-system/run_all.sh --only gqlite --backends lazy \
    --ablate --iters 10 --warmup 2 --timeout-s 900
```

The interesting cells: no-pin on IC4/IC7, no-bfs on IC1/IC13,
no-fold on IC2/IC8, no-unroll on IC6/IC11-shaped chains.

## 3. Ingest-cost table (R4.2)

Run AFTER the main run finishes (it force-rebuilds every system's DB):

```bash
bash bench/cross-system/ingest_bench.sh        # add --only ... to subset
```

Produces `results/ingest_<ts>.csv` (system;elapsed_seconds;db_bytes;status).
Local SF0.1 references (Apple M3 Max): gqlite 20.6 s / 1.49 GB,
kuzu 2.9 s / 94 MB, graphqlite 16.8 s / 347 MB, grafeo 39.0 s / 92 MB,
neo4j 44.3 s over bolt (incl. wipe).

## 4. Break-even analysis (R4.2)

```bash
python bench/cross-system/duckdb/breakeven.py \
    bench/cross-system/results/ingest_<ts>.csv \
    bench/cross-system/results/<main-run-ts>/
```

Prints, per IC, after how many queries each engine's ingest pays for
itself vs DuckDB scanning the CSVs in place.

## 5. Scaling sweep (R2.6) — SF0.1 → SF3

Downloads + builds SF3 on first run (large: dataset ~1.4 GB compressed,
.gdb build needs several GB of disk/RAM; check `df -h` first):

```bash
bash bench/scaling/run_scaling.sh --sfs 0.1,3 --iters 5 --warmup 1 \
    --mem-limit-gb 28
# SF10 too, if disk/RAM allow:
#   bash bench/scaling/run_scaling.sh --sfs 10 --iters 3 --warmup 1
```

Outputs `bench/scaling/results/<ts>/`: per-(SF, backend) latency CSVs,
`*.open.txt` (open-time phase breakdown), `*.mem.txt` (peak RSS —
works on Linux; was -1 on the macOS validation).

## 6. DML micro-benchmark (R1.3)

```bash
./target/release/dml_bench bench/data/ldbc-sf0.1.gdb \
    --n 1000 --probe-iters 7 > bench/cross-system/results/dml_bench.csv
```

Local references: INSERT ~800 K stmts/s (overlay append), MATCH+SET
~8.6 K/s, MATCH+DETACH DELETE ~15.6 K/s, post-DML first-query penalty
~660 ms (TripleIndex rebuild).

## 7. Plots

```bash
python bench/cross-system/plot_results.py \
    bench/cross-system/results/<main-run-ts>/cross_system.csv
```

`plot_results.py` already knows the new labels (froGQL (disk), Neo4j,
DuckDB (in-situ), the no-* ablation modes).

## Measurement caveat (quote when quoting numbers)

gqlite is benched through its Rust binary; external systems through
their Python drivers (~1–2 ms FFI per call); Neo4j additionally pays
bolt round-trip to a local dockerized server. Each system is measured
via its primary user-facing interface, not normalized — the
row-equivalence oracle is the apples-to-apples part.
