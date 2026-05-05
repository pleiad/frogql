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

## Blocker 1 — DSL cannot express top-level `LIMIT`

The bison grammar (`src/gql.y`) DECLARES a `limit` token (line 115)
and the lexer (`src/gql.l` line 147) emits it inside the `<GQL>`
exclusive-state block whenever it sees the literal `"limit"` keyword:

```yacc
%token limit profile property
```
```lex
"limit"             { stm._errIndx += yyleng; return limit;};
```

But **NO grammar production rule references the `limit` token in
`src/gql.y`.** Verified by both `grep -n 'limit' src/gql.y`
(returns 4 hits, all of which are: line 59/62/65 `numeric_limits`
from C++ stdlib calls, and line 115 the `%token` declaration
itself) and an `awk` pass that found zero production-rule bodies
mentioning `limit`. The complete query production is
(`gql.y` lines 362-379):

```yacc
'{' query_kind '}'                                    // query w/o graph or where
'{' query_kind ',' a_graph_expr '}'                   // query w/ graph
'{' query_kind ',' a_graph_expr ',' where_expr '}'    // query w/ graph + where
```

That's the entire query grammar. No `LIMIT N`, no projection beyond
`query_kind`, no `ORDER BY`.

> **Caveat noticed:** `test/vertex/grammar.gql:35` contains
> `{query: '废墟', in: 'vertex_db', where: {feature_name: {limit: 3,
> $near: [...]}}};` — suggesting `limit` is intended as an option
> *inside* the `$near` (vector-similarity) operator clause. But
> tracing through `condition_property → right_value → condition_object
> → condition_properties → condition_property → ... → OP_NEAR ':' '{'
> geometry_condition '}'` and reading `geometry_condition` (gql.y:813,
> `OP_GEOMETRY ':' a_vector ',' range_comparable`), we find no
> production that allows the `limit` token to appear there either.
> The test is likely either aspirational (future-version syntax) or
> a known-failure regression case. Even if `limit:` somehow did
> parse inside `$near`, that would be vector-search top-k semantics
> ("k nearest neighbors"), which is **not** the result-set LIMIT
> that IC2 requires (`LIMIT 20` after the projection of arbitrary
> matched rows).

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

webbery/gqlite's `query_kind_expr` rule (`gql.y:436`) is:

```yacc
query_kind_expr:
      KW_EDGE
    | LITERAL_STRING
    | a_graph_properties
```

i.e. the query target is exactly one of: the `edge` keyword (all
edges), a single string label like `'movie'`, or a property
accessor like `movie.title`. **No production accepts an array of
label names.** The `in:` clause (gql.y:442 — `KW_IN ':'
LITERAL_STRING`) is also single-string — the graph instance.

WHERE clauses (gql.y:448-449, 700-790) are MongoDB-style operator
predicates on property values:

```javascript
{
  query: 'movie',
  where: {tag: ['black comedy']}
}
```

with `$lt`, `$gt`, `$gte`, `$lte`, `$and`, `$or`, `$near`,
`$geometry`. **None of them are label predicates** — there's no
`{label: 'Comment' or 'Post'}` form. Querying two labels would
require two separate query-statements with results combined in
client code, which is structurally different from what the other
systems run and breaks the bench's "same logical shape" rule.

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

1. Build with CMake (~we tried, hit blockers — see below).
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

## Build attempt (Windows, May 2026): three sequential failures

For completeness, we tried to actually build the library on a
Windows machine before settling on the discard verdict. Three
separate failures, each requiring manual intervention:

### Failure 1 — git-describe submodule version

Cloning with `git clone --depth 1 --recursive` (the README's
recommendation) brings the `libmdbx` submodule at commit
`d5e4c198` from 2022, which predates any tagged release in our
shallow checkout. CMake configure fails at
`third_party/libmdbx/cmake/utils.cmake:84`:

```
fatal: No names found, cannot describe anything.
CMake Error: Please fetch tags and/or install latest version of
git ('describe --tags --long --dirty' failed)
```

Worked around by `git fetch --tags --unshallow` in the submodule,
then creating a synthetic `v0.0.0-bench` tag at the pinned commit
so libmdbx's CMake tag-matcher succeeds.

### Failure 2 — flex/bison not on PATH for MSBuild custom commands

The README claims: *"An version of flex&bison is placed in dir
`tool`. So it's not need to install dependency."* But the CMake
custom command at `tool/CMakeLists.txt` uses `WORKING_DIRECTORY
${CMAKE_SOURCE_DIR}/tool` and runs `flex` bare, expecting Windows
to find `flex.exe` via the working directory. **MSBuild does not
honor cwd-based binary lookup for custom build steps**:

```
"flex" no se reconoce como un comando interno o externo,
programa o archivo por lotes ejecutable.
[generated_tokens.vcxproj] error MSB8066: Custom build for
'CMakeFiles/.../generated_tokens.rule' exited with code 9009.
```

Worked around by pre-generating `include/token.cpp`, `token.h`,
and `gql.cpp` manually with the bundled `tool/flex.exe` and
`tool/bison.exe`, AND by prefixing `tool/` to `PATH` for the
`cmake --build` invocation. Without both fixes, the parser/lexer
never gets generated.

### Failure 3 — libmdbx version-mismatch in generated `version.c`

After the build advances past parser generation, libmdbx's
`version.c` (CMake-templated from `version.c.in`) fires a
compile-time `#error`:

```c
#if MDBX_VERSION_MAJOR != 0 || MDBX_VERSION_MINOR != 0
#error "API version mismatch! Had `git fetch --tags` done?"
#endif
```

The generated version.c writes `0 / 0` (because the synthetic tag
we created has no version), but `mdbx.h` defines
`MDBX_VERSION_MAJOR=0, MINOR=11`. The mismatch fires the
preprocessor `#error`. Worked around by patching the generated
version.c to remove the `#error`.

### Failure 4 — MASM assembly file not built/linked

After all of the above, the build advances to linking the gqlite
library and fails with:

```
LINK : fatal error LNK1181: cannot open input file
'gqlite.dir\Release\jump_x86_64_ms_pe_masm.obj'
[gqlite.vcxproj]
```

The repo contains x86_64 hand-written MASM assembly files
(README states 18.2% of the codebase is "Assembly") for what
appear to be Boost.Context-style coroutine `jump_*`/`make_*`
context switches. CMake doesn't enable MASM (`enable_language(ASM_MASM)`)
in the project, so MSBuild compiles the `.cpp` units that reference
the assembly stubs but never assembles the `.asm` files into
`.obj`s. Linker has nothing to resolve `jump_fcontext` against.

We stopped here. **Each individual failure is patchable. Together
they say the project's CMake configuration has not been exercised
on a fresh Windows checkout in years**, which is consistent with
the 2-year-stale last commit. Combined with the grammar-level
blockers (no LIMIT, no label disjunction), the build issues are
the second-order signal — even if all three were patched, the
grammar still can't express IC2.

### What the build outcome means for the verdict

This isn't a "Windows is hard" complaint — every other system in
this bench (kuzu, graphqlite, gqlite itself) builds clean from a
fresh `git clone` on the same Windows + MSVC 2022 environment.
The build issues are specific to webbery/gqlite's pre-2024 CMake
configuration and unmaintained submodules. They strengthen the
discard verdict rather than weaken it: a system that can't be
built without manual patches in 2026 is, in practical terms,
unmaintained.

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
