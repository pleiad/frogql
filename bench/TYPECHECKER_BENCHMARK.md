# Typechecker Benchmark

Measures the typechecker's wall-time and compares it to the runtime
work it lets us skip. The success bar from the requirements doc:

> "typechecker is not slower detecting errors/invalid than actually
> running the full query"

…is met by **3-4 orders of magnitude** on the cases this bench covers.

## What it measures

Three things, per query:

1. **Per-phase compile time** — parse / elaborate / typecheck are timed
   separately so it's visible where time goes inside the "compile" pass.
2. **Typecheck vs runtime** — for valid queries, typechecker overhead
   on top of runtime; for guaranteed-empty / invalid queries, the
   typechecker decision time vs the runtime work it skipped.
3. **Short-circuit firing** — per-query verdict ("empty?", "saved?")
   so a regression that drops the §10 Theorem 6.5 short-circuit shows
   up immediately.

## Pre-work landed in this branch

The runtime previously ran even when the typechecker reported the
result as guaranteed empty (rules.md §10 Theorem 6.5: pattern type-
checks but ⊥ in path/env ⇒ runtime returns ∅). Two changes wire the
short-circuit through:

- `CompileResult.guaranteed_empty: bool` exposes the typechecker's
  empty verdict.
- `gqlite` REPL and Python `Connection.execute` short-circuit when
  `guaranteed_empty` is true — they skip the runtime call and return
  zero rows. The REPL prints `0 rows (typechecker: guaranteed empty,
  runtime skipped)` so the user sees what happened.

Without this, a typechecker-fast verdict still pays the runtime cost
on every query, and any "typecheck-faster-than-runtime" claim is
moot.

## Setup

Same .gdb as `LDBC_BENCHMARK.md` produces (LDBC SF0.1 imported via
`gqlite db.gdb --import-ldbc-csv ...`). 327k nodes, 1.5M edges.

```bash
cargo build --release --bin typecheck_bench
./target/release/typecheck_bench bench/data/ldbc-sf0.1.gdb --iters 30 \
    > /tmp/tc_data.csv 2> /tmp/tc_summary.txt
```

stdout is per-iteration CSV (`category;case;phase;iter;ns;flags`),
stderr is the human summary.

## Categories

- **valid** — query passes typecheck, returns non-empty results.
- **empty** — query passes typecheck but is guaranteed empty by typing
  (label not in the schema, edge-direction not present, etc.).
  The short-circuit fires; runtime is skipped.
- **invalid_unbound** — query has a free variable in RETURN.
  Typechecker errors out before runtime; runtime is skipped.
- **invalid_parse** — query fails to parse. Elaboration / typecheck
  / runtime are all skipped; only `parse` time is reported.

## Results (LDBC SF0.1, 30 iterations, median)

```
cat   case                    parse_us  elab_us  tc_us   rt_ms   empty?  saved?
─────────────────────────────────────────────────────────────────────────────────
valid v_simple                  16.30     3.80    110.30   19.77   no      no
valid v_chain2                  20.70     4.30    232.50  1664.18  no      no
valid v_chain3                  23.10     4.50    250.60  1601.84  no      no
empty e_unknown_label            1.80     0.30     22.30    0.00   yes     yes
empty e_unknown_edge             4.20     0.90     43.40    0.00   yes     yes
empty e_chained_unknown          2.80     0.70     57.20    0.00   yes     yes
inval i_unbound                  1.60     0.20     20.20    0.00   no      yes
inval i_unbound_chain            5.40     1.30    110.20    0.00   no      yes
inval i_parse                    2.20     —        —        —      no      yes
```

### Where time goes inside compile

For valid queries: parse ~20 μs, elaborate ~4 μs, typecheck ~100-250 μs.
**Typecheck is the dominant phase**, but the entire pipeline is
under 0.5 ms.

### Typechecker overhead on valid queries

| Query    | Typecheck phase | Runtime |
|----------|-----------------|---------|
| v_simple | 0.11 ms         | 19.77 ms |
| v_chain2 | 0.23 ms         | 1664 ms (1.66 s) |
| v_chain3 | 0.25 ms         | 1602 ms (1.60 s) |

Typecheck is **0.6 % of total** on the simplest valid query (where
runtime is only ~20 ms) and **<0.02 %** on chain queries that
actually traverse the graph. Negligible overhead in either regime.

### Typechecker rejection vs would-be runtime

For empty / invalid queries the typechecker stops before runtime —
the meaningful comparison is "how long would the runtime have taken
if we hadn't short-circuited?". Measured with `--no-typecheck`
against the same LDBC SF0.1:

| Case                | Typecheck verdict | Runtime if not skipped | Speedup |
|---------------------|-------------------|------------------------|---------|
| e_unknown_label     |  22 μs            |  1820 ms               |  ~83000x |
| e_unknown_edge      |  43 μs            |  1699 ms               |  ~40000x |
| i_unbound           |  20 μs            |    26 ms (and silently returns wrong NULLs) |  ~1300x |
| i_unbound_chain     | 110 μs            |  1791 ms (and silently returns wrong NULLs) | ~16000x |

Two flavors of win:

1. **Empty queries** — typecheck is microseconds, runtime is seconds.
   Four orders of magnitude faster, *and* the runtime would have
   produced the same (empty) answer the typechecker statically
   guaranteed.

2. **Unbound-variable queries** — runtime returns wrong-but-not-empty
   results (NULLs joined with the rest of the pattern) in seconds.
   Typecheck rejects in microseconds with a precise error message.
   This is the bigger win: typecheck is *correct* where the runtime
   silently is not.

The user's bug report from earlier in this session
(`MATCH (x: Person) RETURN y.name` returning 100 rows of `"NULL"`)
is exactly this case — and is exactly what the typechecker rejects
when it is not bypassed.

## Honest limits

- LDBC's DEFAULT schema is *inferred* from the data (every label and
  edge that exists), so "empty by typing" here means "label that
  doesn't exist in the data at all". A more interesting case would
  be a hand-written GRAPH TYPE that's stricter than the data, where
  some valid-looking queries are rejected by typing — that needs a
  custom schema and is left for a follow-up bench.
- The runtime numbers for empty cases (1.7-1.8 s) are dominated by
  the same predicate-pushdown gap the LDBC bench surfaces: the
  optimizer enumerates the join then filters, instead of pushing
  down. With pushdown those would be milliseconds and the speedup
  ratios would shrink to 100-1000x — still a clear win, but not
  five orders of magnitude.
- iteration count (30) is conservative. The CSV stream lets you
  recompute confidence intervals if needed; the median is stable
  to ~5-10 % at this iteration count for queries where wall time
  is dominated by the runtime work, much tighter for the
  microsecond-scale typecheck phases.
