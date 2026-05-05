# webbery/gqlite — discarded

We attempted to integrate **[webbery/gqlite](https://github.com/webbery/gqlite)**
as a fourth external system in the cross-system bench. After
**reading the full README, the bison grammar (`src/gql.y`), the
lexer (`src/gql.l`), the public C API (`include/gqlite.h`), and the
representative tests (`test/movielens.cpp`, `test/grammar.cpp`,
`test/benchmark.cpp`)**, we determined the system cannot be benched
against LDBC SNB IC2.

The original [bench plan](../../README.md) flagged this project as
"dead since April 2023" and time-boxed it at three days. We found
this was correct in substance and concrete in detail; this file
records the specific blockers so the rejection is reviewable rather
than vibes-based.

## What it is

C++17 graph database, bundled libmdbx (LMDB-derivative) for storage.
- 207 commits, master branch
- Last commit: **2023-04-08** (over 2 years before this review)
- Custom JSON-like DSL (the README calls it "Graph Query Language",
  marked as "Unstable")
- C API only; no Python, Ruby, Crystal, or other bindings
- Bison/flex parser; CMake build with bundled flex/bison binaries
  for Windows; submodules: eigen, fmt, libmdbx
- Targeted at "ending device" — IoT/edge form factors
- README states the DSL is "Unstable"; CHANGELOG.md is one line:
  "vertex operation support"

## Blocker 1 — DSL cannot express `LIMIT`

The bison grammar (`src/gql.y`) DECLARES a `limit` token (line 115)
and the lexer (`src/gql.l` line 147) emits it for the literal
`"limit"` keyword:

```yacc
%token limit profile property
```
```lex
"limit"             { stm._errIndx += yyleng; return limit;};
```

But **NO grammar production rule references it.** The complete query
production is (`gql.y` lines 362-379):

```yacc
'{' query_kind '}'                                    // query w/o graph or where
'{' query_kind ',' a_graph_expr '}'                   // query w/ graph
'{' query_kind ',' a_graph_expr ',' where_expr '}'    // query w/ graph + where
```

That's the entire query grammar. No `LIMIT N`, no projection beyond
`query_kind`, no `ORDER BY`. The `limit` keyword is half-implemented:
it lexes but won't parse. We confirmed by grep — `limit` does not
appear in any production rule.

**For LDBC IC2, which requires `LIMIT 20`**, this is a hard blocker.
Without LIMIT, every IC2 invocation would return thousands of rows
(matching the issue we hit with Kuzu when LIMIT was attached to the
wrong UNION branch). Fairness vs. the other systems' 20-row results
would be lost.

## Blocker 2 — No label-disjunction syntax

IC2 needs to match messages of either label `Comment` or `Post`.
The other systems express this in their native idioms:

| System | Label-disjunction form |
|---|---|
| gqlite | `(c: Comment \| Post)` |
| graphqlite | `WHERE c:Comment OR c:Post` |
| auksys/gqlite | `WHERE c:Comment OR c:Post` |
| Kuzu | unlabeled `(c)` constrained by multi-typed REL TABLE |

webbery/gqlite's WHERE clause grammar (`gql.y` lines 448-449,
700-743) is structured as MongoDB-style operator predicates:

```javascript
{
  query: 'movie',
  where: {tag: ['black comedy']}
}
```

with `$lt`, `$gt`, `$and`, `$or` as operators on property values.
**There is no syntax for "this entity belongs to label A or label
B"** — `query` and `in` clauses each take ONE group/label. Querying
two labels requires two separate query-statements, with results
combined externally. This is structurally different from what the
other systems run, breaking the bench's "same logical shape" rule.

## Blocker 3 — No bulk-load API

The canonical loading pattern from `test/movielens.cpp` (their own
example loader for the MovieLens dataset, ~10K movies, ~5K tags):

```cpp
char upset[512] = { 0 };
sprintf(upset,
  "{upset: 'movie', vertex: [[%d, {title: '%s', genres: '%s'}]]};",
  id, title.c_str(), genres.c_str());
gqlite_exec(pHandle, upset, gqlite_exec_callback, nullptr, &ptr);
```

**One DSL exec per CSV row.** This is the same architectural shape
that made auksys/gqlite take indefinite hours on LDBC SF0.1 (~600K
entities including MVAs). We don't need to retest the failure mode;
we know how it ends.

The `gqlite_create` / `gqlite_execute` / `gqlite_next` prepared-
statement API exists (see `include/gqlite.h`) but the readme +
tests don't show it being used for bulk inserts; it's positioned
for repeated reads, not writes.

## Blocker 4 — Buffer overflow risks in their own load idiom

The 512-byte buffer above is theirs. LDBC content fields are 80+
characters; with proper escaping, JSON-encoding, and group/property
keys, a single `upset` statement easily exceeds 512 bytes for any
real `Comment` or `Post` row. Any harness we built would have to
fix the example pattern just to load LDBC text fields without
truncation.

## Blocker 5 — C-only API; no per-system harness shape

graphqlite, kuzu, and auksys/gqlite all have Python bindings. Our
existing per-system runners (`run.py`) wrap those bindings. Webbery
provides only a C API (with C++ examples). To integrate it we'd
need to write a C++ harness around `gqlite_exec` + a callback —
roughly the same scope as the per-system Rust harness we wrote for
GraphLite, except in a less mature toolchain (CMake submodule build
with flex/bison rather than a single Cargo command).

## Blocker 6 — Designed-for scale

The README states GQLite's purpose is "for testing abilities in
ending device" and the goal is "small, fast, light-weight." Their
own reference tests (`test/movielens.cpp`) run on the MovieLens
small dataset (~10K movies, ~100K tags, ~100K ratings). LDBC SF0.1
is **30× larger** (~327K nodes, ~1.5M edges) and is the smallest
LDBC scale factor — i.e. SF1 / SF10 / SF100 are 10× / 100× / 1000×
this. The system was not designed for or tested at the scale this
bench runs.

## What it would have taken to integrate

For completeness, the integration shape if we'd pushed through:

1. Build with CMake on Windows-MSYS or PowerShell (~30 min). Their
   `tool/` directory ships flex/bison binaries for Windows, so this
   step is plausible.
2. Write a C++ bench harness (`run.cpp`) that opens the DB, parses
   IC2 params from the LDBC params file, builds the per-param-row
   DSL string, executes via `gqlite_exec`, captures result rows in
   a callback, emits the cross-system CSV (~150 lines).
3. Write a C++ loader (`setup.cpp`) that streams every LDBC CSV and
   issues one `upset` per row (~600K calls for SF0.1). Plus an MVA
   pre-aggregation pass.
4. Write IC2 in their DSL **somehow** — without LIMIT, with two
   separate queries (one per label) combined in C++, accepting that
   results are unbounded per query.
5. Hope the `libmdbx` storage handles LDBC scale without crashing.

Estimated effort: 1-2 working days, with a high probability that
the `limit`-less DSL produces results that aren't comparable to the
other systems' LIMIT-20 outputs anyway.

## Verdict

**Discard.** The system has structural blockers (no LIMIT, no
label-disjunction, per-row load idiom) that make the bench
comparison structurally impossible, not just slow. The original
plan's two-year-old "dead since April 2023" diagnosis was right
about the symptom and right about the prognosis.

This file replaces the empty `webbery_gqlite/` slot in the
cross-system harness. No `setup.*` or `run.*` is provided; the
orchestrator's `run_all.sh` already detects missing runners and
logs `[SKIP]` to `skipped.log`.

## What we got from the attempt

- Concrete documentation of why this system doesn't fit, not just
  "the project is old" — useful for the writeup's "Threats to
  validity" / "Systems considered but rejected" subsection.
- A more complete picture of the candidate-system landscape: of the
  five external systems we evaluated (graphqlite, GraphLite-AI,
  auksys/gqlite, Kuzu, webbery/gqlite), only graphqlite and Kuzu
  cleared the bar (and Kuzu only barely — see `kuzu/DIVERGENCES.md`
  on its archival status). The "small embedded graph DB" niche has
  multiple aspirational projects and no successful LDBC-scale
  contender.
- Independent verification of the original bench plan's
  prioritization. Time-boxing webbery low was correct.
