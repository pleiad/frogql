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

So the cross-system comparison stays apples-to-apples on query
shape — every system runs "one MATCH + relationship-type-implicit
label constraint." Kuzu just expresses it more elegantly because
the REL TABLE schema does the work that the other systems push into
the WHERE clause or the parser.

| System | Label disjunction form |
|---|---|
| gqlite (us) | `(c: Comment \| Post)` (native form, our parser) |
| graphqlite | `WHERE c:Comment OR c:Post` (their dialect rejects pipe) |
| auksys/gqlite | `WHERE c:Comment OR c:Post` (planner bug on pipe in multi-hop) |
| **Kuzu** | **`(c)` unlabeled, constrained by multi-typed `hasCreator` REL TABLE** |

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

### 3. Schema must declare every CSV column

Kuzu's COPY FROM rejects rows where the column count doesn't match
the table schema. The LDBC Person CSV has 8 columns; our IC2 only
queries 3 (id, firstName, lastName). Rather than pre-process CSVs,
the schema in `setup.py` declares all 8 — IC2 simply doesn't
reference the unused fields. Same approach for Comment (6 cols) and
Post (8 cols).

### 4. `knows` is materialized in both directions at load time

LDBC's `person_knows_person_0_0.csv` records each pair once. Kuzu
has no undirected REL TABLE primitive, so we generate a reversed CSV
in `setup.py` and COPY it into the same `knows` table. Same
convention used by every other system in the cross-system bench.

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
