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

### 1. Label disjunction handled implicitly via the multi-typed REL TABLE

The IC2 query needs to match messages of either label `Comment` or
`Post`. Kuzu's openCypher subset accepts neither
- `MATCH (c:Comment|Post) ...` (Cypher 5+ direct union-label form)
- `MATCH (c) ... WHERE c:Comment OR c:Post ...` (label-predicate form)

Both fail with `Parser exception: Invalid input ...`. We initially
wrote the query with a `UNION ALL` of two MATCHes (which Kuzu does
accept), but ran into the next problem: **`LIMIT 20` after `UNION
ALL` doesn't apply to the union total.** It applies only to the
second branch, so the result was thousands of rows from branch 1
plus 20 from branch 2 — wrong by orders of magnitude.

Trailing `WITH ... LIMIT` after a UNION isn't supported in Kuzu's
parser. `CALL { ... } RETURN ... LIMIT 20` (the openCypher 5+
subquery form for "limit the union total") isn't either.

**The form that works** turned out to be much simpler: leave `(c)`
unlabeled. Because `hasCreator` is declared as a multi-typed REL
TABLE (`FROM Comment TO Person, FROM Post TO Person`), the engine
constrains the source endpoint of an unlabeled `(c)-[:hasCreator]->`
to nodes that are Comment-or-Post automatically. No union, no
predicate, no rewrite needed. The shape matches what gqlite, our
own ISO-GQL system, runs:

```cypher
MATCH (p:Person {id: $personId})-[:knows]-(friend)<-[:hasCreator]-(c)
WHERE c.creationDate <= $maxDate
RETURN ... LIMIT 20
```

| System | Label disjunction form | Constraint expressed at... |
|---|---|---|
| gqlite (us) | `(c: Comment \| Post)` (native ISO GQL) | Query level |
| graphqlite | `WHERE c:Comment OR c:Post` (Cypher 4.x dialect rejects pipe) | Query level |
| auksys/gqlite | `WHERE c:Comment OR c:Post` (planner bug on pipe in multi-hop) | Query level |
| **Kuzu** | **`WHERE label(c) IN ["Comment", "Post"]`** (Kuzu builtin) | **Query level** |

### Kuzu's situation in detail

Kuzu's openCypher dialect doesn't accept either of the syntactic
forms the other systems use:

- `(message:Comment|Post)` pattern disjunction → parser error
- `WHERE message:Comment OR message:Post` runtime predicate →
  parser error (Kuzu has no `node:Label` predicate because every
  node lives in exactly one NODE TABLE, so the engine never needs
  to ask that question at runtime)
- `UNION ALL ... LIMIT N` (canonical Cypher arg-max idiom) →
  silently broken; LIMIT applies per-branch only, not globally
  (returned 151K rows for LIMIT 5 in our test)
- `CALL { ... UNION ALL ... }` subquery wrap → no `CALL{}` grammar
- `WITH ... after UNION ALL` → parser error

The form that **does work and is what we ship**: the `label(node)`
builtin returns the node's NODE TABLE name as a string. `WHERE
label(m) IN ["Comment", "Post"]` is a real query-level predicate,
evaluated per row. Semantically equivalent to gqlite's
`(m:Comment|Post)` and graphqlite's `WHERE m:Comment OR m:Post`.

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

What does NOT work in Kuzu (tested):
- `(m:Comment|Post)` pattern disjunction → parser error
- `WHERE m:Comment OR m:Post` predicate (the openCypher form
  graphqlite and Neo4j accept) → parser error. Kuzu's data model
  puts each node in exactly one NODE TABLE, so the engine never
  exposes a `node:Label` predicate; you reach for `label(node)`
  string equality instead.
- `UNION ALL ... LIMIT N` (canonical Cypher arg-max idiom) →
  silently broken; LIMIT applies per-branch only, not globally
  (returned 151K rows for LIMIT 5 in our test). Not relevant once
  we found `label()`, but documented for future ICs.
- `CALL { ... UNION ALL ... }` subquery wrap → no `CALL{}` grammar.
- `WITH ... after UNION ALL` → parser error.

Earlier we considered relying on the multi-typed REL TABLE schema
(`(message)` unlabeled, with `hasCreator` declared `FROM Comment,
FROM Post`) as the constraint mechanism. That worked but encoded
the constraint at LOAD TIME rather than at QUERY TIME — it'd produce
different results on a different data shape. We replaced it with
the `label()`-based form before merging the bench, so our shipped
queries do express the spec's predicate at the query level.

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
