# LDBC SNB Interactive — Spec Source-of-Truth Checklist

Direct quotes from the LDBC SNB specification (`ldbc/ldbc_snb_docs`,
`main` branch) mapping each requirement to: *audited vs informal*,
*what our bench does*, *gap to close*.

The headline finding is in the spec itself:

> "We expect LDBC benchmarks to be used in many scenarios. For most
> research papers, fully audited results are unrealistic and even
> unaudited results can provide insight into the performance of the
> systems under test (SUT)."
>
> — `benchmark-checklist.tex`

…and:

> "Benchmark execution and auditing workflow. **For non-audited runs,
> the implementers perform the steps of the auditor.**"
>
> — `auditing.tex`, caption of figure `audit-workflow`

So the spec explicitly recognizes two tiers:
**informal / research-paper** vs **audited / publication**.
We are firmly in the first tier and the spec sanctions that mode.

---

## 1. The two-tier checklist

### 1.1 Research-paper tier (what we should answer)

`benchmark-checklist.tex` enumerates the **only** items the spec asks
for in research-paper use. Verbatim:

> "However, we ask authors to include the following information in their papers:
>
> - Were the results cross-validated for at least one scale factor?
> - Were the results cross-validated for all scale factors used in the benchmark?
> - Does the SUT have a persistent storage?
> - Does the SUT provide ACID transactions?
> - Does the SUT provide any level of fault-tolerance?
> - How many warm-up rounds were performed?
> - How many execution rounds were performed?
> - How were the execution times summarized?
> - Is the loading phase included in the query execution times?
> - If the SUT is not your own system, did you contact its developers or experts to help optimizing the queries?"

That's the SOT for what our bench's accompanying writeup must answer.

| spec ask | our answer |
|---|---|
| Results cross-validated at one SF? | **No** — see §3 below; #6 (no `coalesce`) and #7 (no `ORDER BY`) make byte-for-byte validation moot. We document this as a gqlite feature gap. |
| Results cross-validated at all SFs? | N/A — single SF (SF0.1). |
| Persistent storage? | **Yes** — gqlite is a single-file `.gdb`. |
| ACID transactions? | **No** — gqlite is read-only at query time; no transaction layer. |
| Fault-tolerance? | **No** — single-file embedded DB. |
| Warm-up rounds performed? | **0** measured, **3** discarded if `--iters >= 4`. The spec's audit warmup is 30+ minutes (see §2.5); we document the gap. |
| Execution rounds performed? | **1 per param × 15 params** (default `--iters 1`). |
| How summarized? | min/median/mean/max wall time per param. With `--iters 1`, the bench prints `wall=Xms` only and recommends `--iters >= 3`. |
| Loading phase included? | **No** — `LazyGraphStore::open` happens before the timed loop. Reported separately as "loaded N nodes / M edges in T s." |
| SUT-developer consulted? | gqlite is the project being benchmarked (i.e. the SUT *is* our own system). |

### 1.2 Audited tier (what we are NOT claiming)

The spec's audit chapter (`auditing.tex`) defines audit-grade
requirements separately. Quotes below describe the *audit* tier; we
are not claiming compliance.

> "Benchmark implementations shall use a stable version (\eg 0.3.6) of
> the test driver. The SUT's database software should be a stable
> version that is available publicly or can be purchased at the time
> of the release of the audit."
>
> — `auditing.tex`, §"Benchmark Software Components"

> "A qualifying run must use a test driver that adapts the provided
> test driver to interface with the SUT. Such an implementation, if
> needed, must be provided by the test sponsor. The parameter
> generation, result recording, and workload scheduling parts of the
> test driver should not be changed."
>
> — `auditing.tex`, §"Adaptation of the Test Driver to a DBMS"

The phrase used is "qualifying run," not "any run" — i.e. the driver
requirement applies to audit submissions, not to running the query.

---

## 2. Audit-only requirements (for reference, not applicable here)

These define the audited tier. We document them so the gap is clear.

### 2.1 Validation of query results

> "A benchmark should be published with a deterministically
> reproducible validation data set. Validation queries applied to the
> validation data set will deterministically produce a set of correct
> answers. This is used in the first stage of benchmark run to test
> for the correctness of an SUT or benchmark implementation. **This
> validation stage is not timed.**"
>
> — `auditing.tex`, §"Validation of Query Results"

> "The validation takes the form of a set of data generator
> parameters, a set of test queries that at least include one instance
> of each of the workload query templates and the expected results."

> "For counts, the value must be exact, for sums, averages and the
> like, at least 8 significant digits are needed, for statistical
> measures like graph centralities, the result must be within 1\% of
> the reference result."

**Applies to us?** Only if we claim correctness. We don't —
documented in `LDBC_BENCHMARK.md` under the "What this bench does
*not* do" section.

### 2.2 Required query mix (Interactive)

> "each query type is executed at a different rate. The way the
> execution rate is decided, also depends on the nature of the query:
> complex read, short read or update."
>
> — `interactive-workload-definition.tex`

> "Update queries' issue times are taken from the update streams
> generated by the data generator. These are the times where the
> actual event happened during the simulation of the social network."

> "Complex reads' times are expressed in terms of update operations.
> For each complex read query type, a frequency value is assigned
> which specifies the relation between the number of updates performed
> per complex read."

> "**short reads are inserted in order to balance the ratio between
> reads and writes**, and to simulate the behavior of a real user of
> the social network."

> "**Warning.** Note that in the current implementation of SNB
> Interactive v1, short queries are only produced if updates are
> enabled. In the absence of updates, no short queries will be
> executed."

**Applies to us?** Only for audit. Our bench runs IC2 only, no IS, no
IU. The spec's note above implies that IS would be skipped anyway in
our updates-disabled regime — that's the official behavior, not a
divergence on our side.

### 2.3 Throughput as the headline metric

> "Operations per second for a given SF (throughput). **This is the
> primary metric of this workload.**"
>
> — `auditing.tex`, §"Summary of Benchmark Results"

> "The tool produces for each of the distinct queries and transactions
> the following summary:
>
> - Run time of query in wall clock time.
> - Count of executions.
> - Minimum/mean/percentiles/maximum execution time.
> - Standard deviation from the average execution time."

**Applies to us?** Audit only. Our reporting is per-query wall time
(min/med/mean/max when `--iters >= 2`, else `wall=`), which is the
per-query subset of the above. We do NOT report throughput because
we don't run the full mix at a target rate — the throughput metric is
inseparable from the workload-mix requirement.

### 2.4 Run duration

> "A valid benchmark run must last at least 2 hours of wall clock
> time and at most 2 hours and 15 minutes."
>
> — `auditing.tex`, §"Query Timing During Benchmark Run"

**Applies to us?** Audit only. We run 15 params × 1 iter ≈ 20 min.

### 2.5 Warm-up

> "First, the SUT must undergo a warm-up period that takes at least
> 30 minutes and at most 35 minutes. The goal of this is to put the
> system in a steady state which reflects how it would behave in a
> normal operating environment. The performance of the operations
> during warm-up is not considered."
>
> — `auditing.tex`, §"Measurement Window"

**Applies to us?** Audit only. The research-paper tier just asks
"how many warm-up rounds were performed," not "30 minutes." We
discard up to N iters via `--warmup N` (default 0 in the LDBC bench;
3 recommended in the typecheck bench).

### 2.6 95% on-time requirement

> "In order to have a valid run, 95% of the queries must meet the
> following condition: actual_start_time − scheduled_start_time < 1 second"
>
> — `auditing.tex`, §"Query Timing During Benchmark Run"

**Applies to us?** Audit only — meaningful only when the driver is
firing queries on a schedule. We run sequentially with no schedule.

### 2.7 ACID

> "The Interactive workload requires full ACID support
> (\autoref{sec:acid-compliance}) from the SUT. This is tested using
> the LDBC ACID test suite."
>
> — `auditing.tex`, §"ACID Compliance"

**Applies to us?** Audit only. gqlite is read-only at query time, no
transactions; we'd fail the ACID test suite. Disclosed in §1.1.

### 2.8 Scale factor for audited runs

> "Audited \emph{benchmark runs} of the Interactive workload shall use
> SF30 or larger data sets. The rationale behind this decision is to
> ensure that there is a sufficient number of update operations
> available to guarantee 2.5 hours of continuous execution."
>
> — `auditing.tex`, §"Scale Factors"

> "The \emph{validation run} shall be performed on the SF10 data set
> and use at least 100,000 operations."

**Applies to us?** Audit only. We run SF0.1 — well below SF30. The
"informal / research-paper" tier doesn't mandate any scale.

### 2.9 Query-language rules (audit)

> "If a domain-specific query language is used, \eg GQL, SPARQL, SQL,
> SQL/PGQ, Cypher, or Gremlin, then **explicit query plans are
> prohibited in all the read-only queries.**"
>
> — `auditing.tex`, §"Implementations Using a Domain-Specific Query Language"

> "Explicit query plans include but are not limited to:
>
> - Directives or hints specifying a join order or join type
> - Directives or hints specifying an access path, \eg which index to use
> - Directives or hints specifying an expected cardinality, selectivity, fanout..."

**Applies to us?** Yes if we claimed audit. Our IC2 query has no
hints; we'd pass this check. (Documented for completeness.)

---

## 3. Why we don't validate row contents

Even before the audit-tier requirements bite, two **gqlite feature
gaps** make byte-for-byte validation pointless:

### 3.1 No `ORDER BY`

The IC2 spec query ends with `ORDER BY message.creationDate DESC,
message.id ASC`. gqlite's parser doesn't have `ORDER BY` yet. Without
it, `LIMIT 20` returns *some* 20 valid friend-message pairs, not the
20 *most recent*. The validation reference outputs assume the sort.

### 3.2 No `coalesce`

The IC2 spec returns `coalesce(message.content, message.imageFile)`.
gqlite has no `coalesce` builtin; we return `c.content` directly,
which is blank for image-only Posts.

Either alone would invalidate `diff -q got.txt expected.txt`. Both
are gqlite implementation gaps tracked separately, not bench-side
divergences.

---

## 4. What changes if we wanted full audit compliance

In rough cost order from cheapest to most expensive:

1. **Driver integration** — adapt the LDBC Java driver via HTTP/JNI
   bridge to gqlite. ~3 days for IC2-only per the earlier estimate.
   Unlocks: query mix scheduling, throughput-at-SLA reporting, audit
   JSON format, 95% on-time check.
2. **Close `ORDER BY` and `coalesce` gaps** — gqlite parser/runtime
   work. ~1.5 weeks. Unlocks: byte-for-byte validation for IC2.
3. **Full IC suite** — each IC unlocks a different gqlite feature
   (shortest paths, OPTIONAL MATCH, date arithmetic, transitive
   closure, etc.). Many weeks of impl work, query-by-query.
4. **ACID layer** — gqlite is read-only; adding write transactions
   with isolation guarantees is a fundamental architecture change.
5. **Scale to SF30+** — ~50× more data than SF0.1. Mostly throughput
   on the load side and runtime feasibility on the optimizer side.
6. **Audit submission** — third-party auditor, FDR (Full Disclosure
   Report), $$$.

The gap between "informal IC2 timing" (now) and "audited submission"
is mostly **gqlite feature work**, not benchmark plumbing.

---

## 5. Sources

All quotes are from the LDBC SNB documentation repository at
<https://github.com/ldbc/ldbc_snb_docs>, `main` branch, fetched at
the time of writing. Specific files cited:

- `executive-summary.tex`
- `benchmark-checklist.tex` — research-paper tier requirements
- `benchmark-specification.tex` — overview, portability statement
- `interactive-workload-definition.tex` — query mix, target throughput
- `auditing.tex` — audit-tier requirements (validation, warmup,
  measurement window, 95% on-time, ACID, scale, query-language rules,
  reporting metrics, software components)

The authoritative paper version is arXiv:2001.02299
("The LDBC Social Network Benchmark"). The LaTeX sources above
are what compile into that PDF.
