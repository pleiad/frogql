# Kuzu — divergences and integration notes

Kuzu is included as a fourth external system in the cross-system
bench. This file documents:
1. Which Cypher features differ between Kuzu's dialect and the canonical
   IC2 form, and how we handled each.
2. The system's archival status, why we kept it anyway, and how that
   affects reproducibility.

## Project status — archived but pinned

[Kùzu Inc.](https://blog.kuzudb.com/post/kuzudb-update/) archived the
upstream GitHub repository on **2025-10-10** with v0.11.3 as the
final release. The team posted a blog explaining they were "working
on something new"; the repo is now read-only; the docs site
(`docs.kuzudb.com`) was already partially down at the time we wrote
this integration. Two community forks exist (bighorn, LadybugDB) but
both are weeks-to-months old as of this integration.

**Why we kept Kuzu in the bench despite the archival:**

1. **It's a real, peer-reviewed engineered system.** [CIDR 2023
   paper](https://www.cidrdb.org/cidr2023/papers/p48-jin.pdf) from
   the Waterloo DSG, vectorized columnar engine, MIT-licensed C++,
   regularly tested against LDBC SF100 in their own CI before
   archival. None of the other external systems we evaluated have
   this engineering provenance.
2. **Reproducibility cuts both ways with archived projects.**
   The PyPI wheel `kuzu==0.11.3` is frozen — it will install
   identically next month, next year. Pinning to it gives the bench
   numbers a more stable reference than tracking a moving target
   would. We pin in `requirements.txt`.
3. **For a research-paper comparison, "actively maintained" is not
   a strict requirement.** Papers cite engines and benchmarks from
   well before their publication date all the time. What matters is
   that the system behaves identically when the paper is reviewed
   as when we ran the bench, which an archived project guarantees.

What we lose by using a frozen target:
- No bug fixes or performance regressions vs. a running competitor
  evaluating the same query today.
- Storage format changes between past versions stranded prior data;
  we get fresh DBs every setup so this doesn't bite, but it's a
  reason not to put long-term reliance on Kuzu DBs.

## Cypher dialect divergences — IC2-specific

### 1. Label disjunction via the `label()` builtin (query-level predicate)

The IC2 query needs to match messages of either label `Comment` or
`Post`. Kuzu's openCypher subset rejects every direct form the
other systems use:

- `MATCH (c:Comment|Post) ...` (Cypher 5+ pattern disjunction) →
  `Parser exception: Invalid input ...`
- `MATCH (c) ... WHERE c:Comment OR c:Post ...` (label predicate)
  → parser error. Kuzu has no `node:Label` predicate because every
  node lives in exactly one NODE TABLE, so the engine never exposes
  it.
- `MATCH ... UNION ALL MATCH ...` with `LIMIT 20` at the tail → the
  parser accepts it, but **`LIMIT` applies only to the second
  branch**, not the union total (returned 151K rows for `LIMIT 5`
  in our test). Silently wrong by orders of magnitude.
- Trailing `WITH ... LIMIT` after `UNION ALL` → parser error.
- `CALL { ... UNION ALL ... } RETURN ... LIMIT 20` → no `CALL{}`
  grammar in Kuzu 0.11.3.

**The form we ship** is `WHERE label(message) IN ["Comment", "Post"]`.
The `label(node)` builtin returns the node's NODE TABLE name as a
string; `IN [...]` is a real query-level predicate, evaluated per
row. Semantically equivalent to gqlite's `(message:Comment|Post)`
and graphqlite's `WHERE message:Comment OR message:Post`.

We also tried, and rejected, a *schema-constrained* form: leave
`(message)` unlabeled and rely on the multi-typed REL TABLE
declaration (`hasCreator FROM Comment TO Person, FROM Post TO
Person`) to constrain the endpoint at edge-traversal time. That
ran 17–175× faster than `label()` (because Kuzu's optimizer
exploits the REL TABLE constraint, but doesn't push `label()`
through multi-hop joins) — but it encoded the predicate at LOAD
TIME rather than QUERY TIME. A reviewer auditing whether the bench
matches the spec would have had to read `setup.py` to verify the
predicate. We chose the slower honest form. See §"Why we ship the
slower honest form anyway" below.

| System | Label disjunction form | Constraint at... |
|---|---|---|
| gqlite (us) | `(c:Comment \| Post)` (native ISO GQL) | Query level |
| graphqlite | `WHERE c:Comment OR c:Post` (Cypher 4.x label predicate) | Query level |
| **Kuzu** | **`WHERE label(c) IN ["Comment", "Post"]`** (Kuzu builtin) | **Query level** |

### Performance cost of the honest form, measured on Kuzu 0.11.3

| Query | rt with `label()` (spec-faithful) | rt with schema-constrained `(message)` (data-shape) |
|---|---|---|
| IC2 (1-hop friend + hasCreator) | ~520 ms | ~30 ms |
| IC8 (4-hop chain through replyOf) | **~14 s/iter** | ~80 ms |

Kuzu 0.11.3's optimizer doesn't push the `label()` predicate
through multi-hop joins — it materializes the full join result
first, then filters by label. The schema-level alternative would
let the multi-typed `hasCreator(FROM Comment, FROM Post)` REL
TABLE constrain the message-side at edge traversal time, ~175×
faster.

### Why we ship the slower honest form anyway

A previous round of this bench used the schema-constrained form
(`(message)` unlabeled, relying on REL TABLE constraints) for its
17-175× speedup. **That was a corner-cut.** The query body didn't
say "Comment-or-Post"; it said "anything reachable via hasCreator,"
and only happened to mean Comment-or-Post because of how
`setup.py` declared the REL TABLE. A reviewer auditing whether the
benchmark queries match the spec would have had to read setup.py
to verify the predicate — that's audit-trail rot the
DIVERGENCES.md couldn't paper over.

The bench's job is to measure what each engine actually does on
the spec-faithful query. If Kuzu's optimizer doesn't push label()
through multi-hop joins, that's **a real Kuzu finding worth
measuring**, not a number to mask by changing the query. Reporting
"Kuzu IC8 ~14 s/iter on the spec-faithful predicate" is more
useful to a reviewer (and more comparable to gqlite's plan-time
characteristics) than "Kuzu IC8 ~80 ms but only because we
silently pre-encoded the predicate at load time."

The bench wall-time goes up substantially (~30+ min per full run)
under this form. Acceptable: the bench is run on merge cadence,
not every dev iteration.

### 2. Multi-typed REL TABLE requires FROM/TO hints in COPY

`hasCreator` connects two source label types (Comment→Person and
Post→Person). Kuzu supports declaring this in one REL TABLE:

```cypher
CREATE REL TABLE hasCreator(FROM Comment TO Person, FROM Post TO Person)
```

But COPY FROM into such a table requires hints to disambiguate which
sub-table the rows belong to:

```cypher
COPY hasCreator FROM 'comment_hasCreator_person_0_0.csv'
  (DELIM='|', HEADER=true, FROM='Comment', TO='Person')
```

A plain `COPY hasCreator FROM ...` fails with
`Binder exception: The table hasCreator has multiple FROM and TO
pairs defined in the schema. A specific pair of FROM and TO options
is expected when copying data into the hasCreator table.`

This is standard Kuzu usage, just slightly more verbose than the
single-typed case.

### 3. Schema must declare every CSV column; MVAs need pre-aggregation

Kuzu's COPY FROM requires every CSV column to match the table
schema (no `DEFAULT` fill on COPY — we verified by probe). The LDBC
node CSVs have many columns (Person: 8, Comment: 6, Post: 8); our
schema declares them all and IC2 simply doesn't reference unused
fields.

For the two Multi-Valued Attributes (MVAs) on Person — `email` and
`language`, declared in our schema as `STRING[]` — there's no
matching column in `person_0_0.csv`. LDBC ships them as separate
files (`person_email_emailaddress_0_0.csv`,
`person_speaks_language_0_0.csv`) with one row per (Person, value).
`setup.py` pre-aggregates these to `{person_id: [value, ...]}` and
writes an augmented Person CSV with `email` and `language` columns
formatted as Kuzu LIST literals (`[a,b,c]`). Then COPY FROM the
augmented CSV. Same data model as gqlite's LDBC loader (which
surfaces these as `Value::List` properties on Person).

### 4. `knows` is materialized in both directions at load time

LDBC's `person_knows_person_0_0.csv` records each pair once. Kuzu
has no undirected REL TABLE primitive, so we generate a reversed CSV
in `setup.py` and COPY it into the same `knows` table. Same
convention used by every other system in the cross-system bench.

### 5. PreparedStatement API is deprecated; we use `execute(query_str, params)`

Kuzu has both `Connection.execute(query_str, params)` (single-call,
documented preferred API) and a `PreparedStatement` class (separate
prepare + execute) that's officially deprecated in v0.11.x — calling
the prepared form emits a DeprecationWarning. The deprecated form
is ~10-15% faster in microbenchmarks (we measured 5.6ms vs 6.3ms
for IC2) because `execute(string, params)` pays a plan-cache lookup
+ framework overhead per call.

We use the **documented preferred API**: `execute(query_str, params)`.
That's what real apps would write today. The 0.7ms gap is real but
it's the cost of the supported path; capturing it as a "fairness
fix" by switching to the deprecated API would be measuring a path
no one is supposed to use anymore.

### 6. IC11 sub-types encoded as `type` column, not synthesized labels

The LDBC SNB IC11 spec references `:Company` and `:Country` as
schema-level sub-labels of `:Organisation` and `:Place`. Our Kuzu
loader (`setup.py`) keeps `Organisation` and `Place` as single NODE
TABLEs with the sub-type carried as a `type` column (lowercase:
`'company'` / `'university'` for Organisation, `'country'` / `'city'`
/ `'continent'` for Place). The cross-system IC11 cypher
(`ic11.cypher`) filters by the `type` column instead of matching the
sub-label directly:

```cypher
MATCH ... -[:workAt]-> (company:Organisation) -[:isLocatedIn]-> (country:Place)
WHERE company.type = 'company' AND country.type = 'country' AND ...
```

Pre-fix the cypher used the spec form `(:Company) ... (:Country)`
directly; Kuzu's strict-schema binder rejected it with
`Binder exception: Table Company does not exist`. Post-fix the query
runs cleanly and the result hash on the one non-empty parameter row
is byte-identical to gqlite's, which uses synthesized
`:Company`/`:Country` sub-labels via its LDBC loader's
`LabelType::And` promotion of the same `type` column. Same data,
different encoding.

Splitting `organisation_0_0.csv` and `place_0_0.csv` into separate
NODE TABLEs at load time would let the spec query work verbatim, but
requires:

1. Pre-splitting both CSVs by the `type` column in `setup.py`.
2. Declaring `Company`, `University`, `Country`, `City`, `Continent`
   as separate NODE TABLEs.
3. Rewriting `isLocatedIn`, `studyAt`, `workAt` as multi-typed REL
   TABLEs with FROM/TO hints per CSV — same idiom already used for
   `hasCreator`, `hasTag`, `replyOf`, `isLocatedIn`, `likes`.

Achievable but out of scope for the current submission. Documented
inline in `ic11.cypher` and verified equivalent via the cross-system
row-content hash oracle.

## What's NOT divergent

- **Parameter binding** — Kuzu's `Connection.execute(query, params)`
  takes `$param`-style placeholders in the query and a Python dict
  whose keys do NOT include the `$` prefix. Same convention as
  graphqlite. (auksys/gqlite was the odd one — its dict keys DO
  include `$`.)
- **PK auto-indexing** — declaring `PRIMARY KEY(id)` on each NODE
  TABLE gets us a real index for the query-time
  `MATCH (p:Person {id: $personId})` lookup. No extra DDL needed.
- **Result iteration** — `QueryResult.has_next()` /
  `.get_next()` returns Python-native types (int / float / str /
  None / list). No custom converters needed for IC2's column types.

## Additional ICs (IC1, IC3, IC4, IC7, IC12, IC13)

The cross-system set grew from {2,5,6,8,9,11} to every IC frogQL
implements. The translations for the new six are written from the
canonical toml + LDBC reference Cypher and adapted to this loader's
schema. They have NOT been row-hash-verified by the author (no LDBC
dataset / Kuzu install was available when they were written); the
server run's row-equivalence oracle is the verification gate. Per-IC
notes:

- **IC3** (`ic3.cypher`) — `label()` message-union (as IC2/IC8/IC9),
  `:City`/`:Country` via `Place.type`, `DURATION({days:N})` → `N *
  86400000` ms, two `count(DISTINCT ...)` + their sum. Scalar columns,
  so a row-hash match against frogQL is expected if the query is right.
- **IC4** (`ic4.cypher`) — `NOT EXISTS { MATCH ... }` existential
  subquery (Kuzu ≥ 0.4). Scalar columns; row-hash match expected.
- **IC13** (`ic13.cypher`) — recursive `SHORTEST` join `-[:knows*
  SHORTEST 1..30]-`; `length(path)` is the hop count, `-1` when the
  pair is in different components. Single scalar column; match expected
  if the SHORTEST syntax + bound are right.
- **IC1** (`ic1.cypher`) — recursive `SHORTEST` to named friends plus
  two `collect({...})` list-of-struct columns and the Person MVAs
  (`email`/`language` STRING[]). **Row-hash match NOT expected** —
  struct/list columns repr differently across engines. Latency +
  scalar columns are the comparable parts.
- **IC7** (`ic7.cypher`) — arg-max-per-liker via `collect({...})` +
  `[1]`; projects a STRUCT column (`latestLike`). **Row-hash match NOT
  expected** (struct repr).
- **IC12** (`ic12.cypher`) — recursive `isSubclassOf*0..10`,
  `collect(DISTINCT tag.name)` list column. **Row-hash match NOT
  expected** (list element order).

The list/struct ICs are kept for their latency signal; the row oracle
will (correctly) flag the column-encoding divergence — record it,
don't mask it.
