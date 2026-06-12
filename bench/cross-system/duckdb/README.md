# DuckDB — in-situ CSV baseline (no ingest)

This system is deliberately different from every other entry in the
cross-system bench: it does **not** load the LDBC data into a native
database. The runner opens an **in-memory** DuckDB connection, defines
each LDBC entity/relation as a **VIEW over `read_csv(...)`** of the raw
pipe-delimited CSVs, and runs the IC translations as plain SQL. Every
query execution re-scans the CSV files it touches.

## Why in-situ

ISWC review R4.2: the real user workload is "I have a CSV dataset, I
want to answer queries on it" — so the evaluation should price the
ingest, not just per-query latency. The in-situ baseline is the
zero-ingest extreme of that trade-off:

- **Graph engines** (frogQL, Kuzu, …): pay ingest once
  (`ingest_bench.sh` measures it), then answer each query fast.
- **DuckDB in-situ**: pay nothing up front, pay the CSV scan on every
  query.

Pairing the two yields the **break-even analysis** — after how many
queries does the engine's ingest investment pay off:

```
break_even = ingest_seconds / (duckdb_median_s - engine_median_s)
```

`breakeven.py` computes that table from an `ingest_bench.sh` output CSV
and a `results/<timestamp>/` directory.

Views (not `CREATE TABLE AS`) are load-bearing: a view re-executes its
`read_csv` on every query. Materializing the CSVs into in-memory tables
would silently turn this into an ingest-based system and invalidate the
whole analysis. Don't "optimize" that.

## What it runs

SQL translations of the canonical GQL in `bench/ldbc-queries/ic<n>.toml`
(the bench's source of truth): `ic2.sql`, `ic6.sql`, `ic11.sql`.
Translation carve-outs (undirected `knows`, the Comment|Post message
union, walk-multiset `{1,2}` repetition, sub-label encoding) are
documented in `DIVERGENCES.md`. Row-content equivalence against frogQL
is verified per param row by the shared sha256 oracle
(`_lib/row_hash.py`); all 15 rows of IC2/IC6/IC11 hash-match.

## Prerequisites

```bash
pip install -r requirements.txt   # duckdb + psutil
```

plus the LDBC SF0.1 CSVs (`./target/release/bench_setup` from the repo
root downloads them into
`bench/data/ldbc-sf0.1/social_network-sf0.1-CsvBasic-LongDateFormatter/`).
There is **no setup.py** for this system — the CSVs are the database.

## Running

```bash
# from the repo root, one IC:
bench/cross-system/.venv/bin/python bench/cross-system/duckdb/run.py \
    /tmp/duck_ic2.csv --ic 2 --iters 3 --warmup 1
```

Same CLI and output contract as the other runners: stdout-silent,
latency CSV (`query;backend;params;row;iter;result_count;elapsed_ns`,
backend label `duckdb-sql`), per-param-row `ROW ... hash=...` lines on
stderr, and a sibling `<out>.rows.jsonl` row dump.

## Break-even analysis

```bash
# 1. ingest costs (graph engines):
bench/cross-system/ingest_bench.sh --only gqlite,kuzu
#    -> bench/cross-system/results/ingest_<ts>.csv

# 2. latency CSVs for duckdb + the engines, all in one results dir
#    (the orchestrator does this; manually, just name the files
#    <system>.ic<n>.csv in one directory).

# 3. the table:
python bench/cross-system/duckdb/breakeven.py \
    bench/cross-system/results/ingest_<ts>.csv \
    bench/cross-system/results/<timestamp>/
```

Per IC, every engine whose median beats DuckDB's gets a break-even
query count; engines slower than the in-situ scan are flagged
`never (not faster)`. A combined row sums the per-IC medians (one
round of the IC mix) and reports break-even in rounds.

## Measurement caveats

- DuckDB is multi-threaded by default; we leave that alone — an
  analytical engine scanning CSVs with all cores is the honest version
  of this baseline.
- The OS page cache warms the CSV files after the first scan, so
  in-situ latency here is a warm-cache lower bound for DuckDB; a truly
  cold first query is slower. This biases AGAINST the graph engines in
  the break-even table, i.e. the reported break-even counts are
  conservative (upper bounds).
- Python-level `time.perf_counter_ns` around `conn.execute` +
  `fetchall()`, same convention as the other Python runners (~1–2 ms
  FFI overhead vs frogQL's in-process Rust harness).
