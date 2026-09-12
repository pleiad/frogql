# froGQL for AI agents

You are writing queries against **froGQL**, a graph database that speaks
**ISO/IEC 39075:2024 GQL** — not Cypher, not SPARQL, not SQL. This file
exists because the same five mistakes show up over and over, and each one
costs a round trip that a page of reading would have saved.

Everything below was executed against a real database before it was
written down. Where a feature does not exist, this file says so, in
[§12](#12-what-frogql-does-not-have) — read that section before you invent
a function.

---

## 1. The five mistakes, first

**1. Writing Cypher.** `CREATE`, `MERGE`, `WITH`, `UNION`, `STARTS WITH`,
`-[:R*1..2]->` and `size()` are all parse errors here. The GQL spellings
are `INSERT`, (no MERGE), (no WITH — see §5.7), (no statement-level UNION — but see the path union in §5.3), (no string
operators), `-[:R]->{1,2}` and (no size). §12 lists every one.

**2. Building patterns out of triples.** froGQL indexes edges as triples
internally, and that is an implementation detail you must not write in
the query. There is no `(s, p, o)` surface. A pattern is a drawing of the
graph:

```gql
MATCH (p:Person)-[:ACTED_IN]->(m:Movie)<-[:DIRECTED]-(d:Person)
RETURN p.name, m.title, d.name
```

**3. Loading data by writing a JSON file.** `INSERT` is a first-class
statement and it is usually what you want. §4 shows the whole DML surface.
Reach for a JSON or CSV import when you have a bulk file already, not as a
way to avoid learning `INSERT`.

**4. Assuming the aggregates are missing.** `COUNT`, `COUNT(*)`, `SUM`,
`AVG`, `MIN`, `MAX`, `COLLECT_LIST` and `DISTINCT` all exist. So do
`SQRT`, `ABS`, `POW`, `CEIL`, `ROUND`, `FLOOR`, `SIGN`, `SIN`, `COS`,
`TAN`, `EXP`, `LN`, `LOG`, `RADIANS`, `DEGREES`, `CAST`, `CASE WHEN`.
§6 is the complete list. If a query using one fails, the shape is wrong,
not the function missing — the commonest cause is a missing `GROUP BY`,
which in this grammar goes **after** the `RETURN` list.

**5. Passing a row limit and reading it as the whole answer.** `limit` on
`execute()` is an *execution* cap, not a display one: the engine stops
producing, so a truncated result is indistinguishable from a complete
one. It defaults to 0 — no cap — so you get everything unless you ask
otherwise. If you do pass one, `len(rows) == limit` means "there may be
more", not "there are exactly this many".

---

## 2. Connecting

### CLI

```bash
frogql movies.gdb                      # open (creates it empty if absent)
frogql movies.gdb --import-json g.json # create from a JSON snapshot
frogql movies.gdb --import-csv  dir/   # create from CSV + spanner_import_config.json
frogql movies.gdb --no-typecheck       # skip the typechecker this session
```

The REPL reads one statement per line. Meta-commands start with `.`
(sqlite convention): `.schema`, `.indexes`, `.save`, `.dump-json <path>`,
`.dump-gql <path>`, `.limit <n>`, `.pager on|off|always`, `.timeout
<secs>`, `.vec`, `.help`, `.quit`.

`.dump-json` round-trips through `--import-json`. `.dump-gql` writes an
`INSERT` script instead, and **refuses any string containing a single
quote** — the lexer cannot round-trip it — so it is not a general backup
path.

**`.save` is not automatic.** Mutations live in an in-RAM overlay until
you call it, exactly like SQLite's explicit commit. Forgetting it loses
the work.

### Python

```python
import frogql

conn = frogql.open("movies.gdb")        # creates it if absent
rows = conn.execute("MATCH (m:Movie) RETURN m.title", limit=0)
#  -> [{'m.title': 'The Matrix'}, ...]

conn.execute("INSERT (:Movie {title: 'Arrival', released: 2016})")
conn.save()                             # <- required, nothing is written without it

conn.node_count, conn.edge_count
conn.schema()                           # inferred schema, with a 'formatted' string
conn.graph_types()
```

`limit` defaults to 0, meaning no cap. `frogql.import_json(db, json)` and
`frogql.import_csv(db, dir)` build a database from a file.

With a `RETURN`, a row is `{alias: value}`; an unaliased projection is
keyed `col0`, `col1`, …. **Without** a `RETURN`, a row is
`{var: {kind, id, labels, props}}` per pattern variable plus `_paths`.

### Node

Same surface, camelCased: `open`, `importJson`, `importCsv`, and
`execute(query, limit?)`, `save()`, `schema()`, `graphTypes()`,
`nodeCount`, `edgeCount`.

### Browser (WASM)

`frogql-wasm` wraps the in-memory store only: `open_json(json)` →
`Connection` with `execute`, `to_json`, `schema`, `node_count`,
`edge_count`. Reads and DML work; DDL and `CREATE INDEX` do not (there is
no catalog in memory). Persist by storing `to_json()` yourself.

---

## 3. The shape of a query

```
MATCH? <pattern> [, <pattern>]*
[WHERE <expr>]
[NEAREST <k> <var>.<attr> TO <expr> [AS <d>]]
[RETURN [DISTINCT] <items>]
[GROUP BY <var or expr>]
[ORDER BY <key> [ASC|DESC]]
[LIMIT <n>]
```

Two things about that order catch people out.

**`GROUP BY` comes after `RETURN`.** Not before it, and not instead of
it. This is the ISO order and it is the opposite of SQL's.

**`MATCH` is optional.** A bare pattern is a complete query, and with no
`RETURN` the REPL prints every bound variable with its labels and
properties — the fastest way to see what is actually in the graph:

```gql
(x:Movie) LIMIT 3
```

---

## 4. Writing data

### INSERT

```gql
-- standalone nodes
INSERT (:Actor {name: 'Ada', born: 1815})
INSERT (:Actor {name: 'Grace', born: 1906}), (:Film {title: 'Looper', year: 2012})

-- an edge between things that already exist: MATCH them first
MATCH (a:Actor {name: 'Ada'}), (f:Film {title: 'Looper'})
INSERT (a)-[:ACTED_IN {role: 'lead'}]->(f)

-- a node and its edge in one statement
MATCH (f:Film {title: 'Looper'})
INSERT (:Actor {name: 'Bruce'})-[:ACTED_IN]->(f)
```

`{k: v}` inside a pattern means two different things depending on where
it is. In `INSERT` it **sets** the properties. In `MATCH` it **filters**
on them — `(a:Actor {name: 'Ada'})` is exactly `(a:Actor) WHERE a.name =
'Ada'`.

### SET / REMOVE / DELETE

```gql
MATCH (a:Actor {name: 'Grace'}) SET a.born = 1907, a.note = 'edited'
MATCH (a:Actor {name: 'Grace'}) SET a = {name: 'Grace H.', born: 1907}  -- replace all props
MATCH (a:Actor {name: 'Grace'}) REMOVE a.note
MATCH (a:Actor {name: 'Grace'}) SET a:Pioneer        -- add a label
MATCH (a:Actor {name: 'Grace'}) REMOVE a:Pioneer     -- drop a label

MATCH (f:Film {title: 'Looper'}) DETACH DELETE f     -- node + its edges
MATCH (a:Actor)-[r:ACTED_IN]->(:Film) DELETE r       -- just the edge
```

`DELETE` without `DETACH` refuses to remove a node that still has edges.
One DML operation per statement; `MATCH … INSERT … SET …` chains are not
supported yet. Each statement is all-or-nothing.

### Loading in bulk

For a few thousand elements, `INSERT` in a loop through the Python or
Node binding is fine — mutations stage in RAM and only `save()` touches
disk. Measured on a 4.6 M-edge database: ~300 000 node inserts/second,
~9 000 `MATCH`+`SET`/second.

For a file you already have, the importers skip the query layer entirely:

```bash
frogql db.gdb --import-json snapshot.json   # {"nodes": [...], "edges": [...]}
frogql db.gdb --import-csv  dir/            # needs spanner_import_config.json
frogql db.gdb --import-ldbc-csv dir/        # LDBC SNB CsvBasic
import_ttl  db.gdb dump.nt                  # streaming N-Triples / Turtle
```

The JSON shape is what `.dump-json` writes, so it round-trips:

```json
{"nodes": [{"id": "a", "labels": ["Movie"], "props": {"title": "The Matrix"}}],
 "edges": [{"id": "e1", "labels": ["ACTED_IN"], "props": {},
            "endpoints": ["p", "a"], "directionality": "->"}]}
```

`directionality` is `"->"` or `"~~"` (undirected).

**Indexes are not maintained per statement.** The secondary indexes are
rebuilt when the database opens, and the LTJ join index keeps a small
delta per mutation. So a bulk load already pays its index cost once, at
the end, rather than per row — you do not need to drop and recreate
anything.

---

## 5. Patterns

### 5.1 Nodes and edges

```gql
(x)                     any node, bound to x
(x:Movie)               label
(:Movie)                label, unbound
(x:Movie {title: 'Q'})  label + property filter
(x:A&B)                 both labels
(x:A|B)                 either label

-[:R]->     directed, labelled          -[e:R]->   ... binding the edge to e
<-[:R]-     the same edge, backwards
-[:R]-      either direction
~[:R]~      undirected edges only
-->         unlabelled directed edge (sugar for -[]->)
-[:A|B]->   either label
-[:A&B]->   both labels
```

The same algebra works on edges and on nodes. `(x:A|B)` and `-[:A|B]->`
are "either label"; `(x:A&B)` and `-[:A&B]->` are "both". An edge
carrying two labels is **one** match, not one per label.

### 5.2 Joining

Juxtaposition is a path; a comma is a join on shared variables.

```gql
-- a three-hop path
MATCH (p:Person)-[:ACTED_IN]->(m:Movie)<-[:DIRECTED]-(d:Person)
RETURN p.name, d.name

-- two patterns that must agree on m
MATCH (a:Person)-[:ACTED_IN]->(m), (b:Person)-[:ACTED_IN]->(m)
WHERE a.name < b.name
RETURN a.name, b.name
```

That `a.name < b.name` is the idiom for "one of each unordered pair", and
it works: strings, booleans, numbers and same-type temporal values are
all ordered.

### 5.3 Union of paths

`|` between two path terms is a **union of whole patterns**, not just of
labels. The arms may differ in length and may bind different variables;
the result is the bag union of both.

```gql
-- every acting-or-directing pair, in one pattern
MATCH (p:Person)-[:ACTED_IN]->(m:Movie) | (p:Person)-[:DIRECTED]->(m:Movie)
RETURN p.name, m.title

-- arms of different lengths: one hop or two
MATCH (a:Person)-[:FOLLOWS]->(b:Person)
    | (a:Person)-[:FOLLOWS]->(x:Person)-[:FOLLOWS]->(b:Person)
RETURN a.name, b.name
```

Parenthesise it to concatenate more pattern onto the union:
`((a)-[:R]->(b) | (a)-[:S]->(b))-[:T]->(c)`.

This is the closest thing to SQL's `UNION`, and it is not the same
thing: it unions *patterns inside one query*, not two independent
queries with their own `RETURN` lists. There is no statement-level
`UNION`.

Do not confuse it with the **label** union `-[:A|B]->` and `(x:A|B)`,
which is a different operator in a different position (§5.1). The label
form picks which edges match; the path form picks which *shapes* match,
and the arms can differ in length and in the variables they bind.

### 5.4 Repetition

```gql
-[:FOLLOWS]->{1,2}      one or two hops
-[:FOLLOWS]->{2}        exactly two
(...)?                  optional
```

**Unbounded repetition (`*`, `+`, `{n,}`) needs a path prefix** that makes
it finite. Without one it is a type error, with a message that names the
options:

```gql
-- error
MATCH (p:Person)-[:FOLLOWS]->*(q:Person) RETURN p.name

-- fine
MATCH ANY SHORTEST (p:Person)-[:FOLLOWS]->*(q:Person) RETURN p.name, q.name
MATCH TRAIL (p:Person)-[:FOLLOWS]->*(q:Person) RETURN p.name, q.name
```

Cypher's `*1..2` is **not** valid here; the quantifier goes after the
whole edge, `-[:R]->{1,2}`.

### 5.5 Path prefixes (ISO §16.6)

Prefixes scope to one comma operand.

| prefix | meaning |
|---|---|
| `WALK` (default) | anything goes |
| `TRAIL` | no repeated edge |
| `SIMPLE` | no repeated node (a closing cycle is allowed) |
| `ACYCLIC` | no repeated node at all |
| `ALL` (default) | every match |
| `ANY [n]` | up to n per endpoint pair |
| `SHORTEST n [PATHS]` | the n shortest per endpoint pair |
| `SHORTEST n GROUPS` | every path at the n shortest lengths |
| `ANY SHORTEST` / `ALL SHORTEST` | the common spellings of `SHORTEST 1 PATHS` / `GROUPS` |

### 5.6 Named paths

```gql
MATCH path = ANY SHORTEST (a:Person {name: 'Tom Hanks'})
                          -[:ACTED_IN|DIRECTED]-*
                          (b:Person {name: 'Keanu Reeves'})
RETURN PATH_LENGTH(path) AS len
```

`PATH_LENGTH` (edges), `CARDINALITY` (nodes + edges) and `ELEMENTS` are
ISO. `NODES` and `EDGES` also work and are a froGQL extension.

### 5.7 OPTIONAL MATCH

There is no `WITH`, so the way to chain is a second match clause:

```gql
MATCH (p:Person)
OPTIONAL MATCH (p)-[:DIRECTED]->(m:Movie)
RETURN p.name, m.title
```

Left-join semantics: `m.title` is `NULL` where the optional side found
nothing.

---

## 6. Expressions and functions

### 6.1 Aggregates

| | |
|---|---|
| `COUNT(*)` | rows, nulls included |
| `COUNT(x)` / `COUNT(DISTINCT x)` | non-null values |
| `SUM`, `AVG`, `MIN`, `MAX` | each takes an optional `DISTINCT` |
| `COLLECT_LIST(x)` | the group's values as a list (aliases `COLLECT`, `ARRAY_AGG`) |

Nulls are eliminated before the reducer runs; an empty aggregate is
`NULL`. Aggregates compose inside arithmetic:

```gql
MATCH (m:Movie) RETURN COUNT(*) AS n, AVG(m.votes) AS avg_votes, AVG(m.votes) + 0 AS also
```

**Mixing an aggregate with a plain column needs `GROUP BY`:**

```gql
-- error: "RETURN mixes aggregate and non-aggregate items ..."
MATCH (p:Person)-[:ACTED_IN]->(m:Movie) RETURN p.name, COUNT(DISTINCT m) AS films

-- correct
MATCH (p:Person)-[:ACTED_IN]->(m:Movie)
RETURN p.name, COUNT(DISTINCT m) AS films
GROUP BY p
ORDER BY films DESC
LIMIT 3
```

Group by the **variable** (`GROUP BY p`) when you want node identity, or
by an expression (`GROUP BY p.name`) when you want the value.

### 6.2 Scalar functions

| group | functions |
|---|---|
| rounding | `FLOOR`, `CEIL` (`CEILING`), `ROUND` |
| arithmetic | `ABS`, `SIGN`, `SQRT`, `POW` (`POWER`), `EXP`, `LN`, `LOG(base, x)` |
| trigonometry | `SIN`, `COS`, `TAN`, `RADIANS`, `DEGREES` |
| conversion | `CAST(x AS INTEGER \| FLOAT)` |
| paths | `PATH_LENGTH`, `CARDINALITY`, `ELEMENTS`, `NODES`, `EDGES` |
| temporal | `DATE()`, `DATE('2024-01-31')`, `LOCAL_DATETIME('2024-01-31T10:00:00')` |
| vectors | `VECTOR(<node>, '<attr>')` |

Rules they all follow: **null in, null out**; a non-numeric argument is a
type error; a result outside the reals (`SQRT(-1)`, `LN(0)`) is `NULL`.
`ABS` and `SIGN` keep an integer integral, everything else returns a
float — wrap in `CAST(… AS INTEGER)` to narrow.

```gql
MATCH (m:Movie)
RETURN m.title, SQRT(m.votes) AS s, ROUND(m.votes / 7.0) AS r, POW(m.votes, 2) AS p
```

All of these are **soft keywords**: they are only special immediately
before `(`, so `abs`, `round`, `sign` and `degrees` stay usable as
variable, label and property names. That rule is why `DURATION` needs
its parentheses — `DURATION({days: 3})` parses, `DURATION {days: 3}`
reads `DURATION` as a variable name.

Temporal values of the same type compare and sort. **Date arithmetic
does not exist**: `DATE('2024-01-31') + DURATION({days: 3})` types as
undefined and yields `NULL`.

### 6.3 Operators

`+ - * /` and `MOD` (**there is no `%`** — it is not even lexed),
`= <> != < <= > >=`, `AND OR NOT`, `IN`, `IS NULL`, `IS NOT NULL`,
`x IS <Type>` / `x TYPED <Type>` (`IS STRING`, `IS str` and
`TYPED str` are all accepted), `CASE WHEN … THEN … ELSE … END`,
`RECORD { k: expr }`, and list literals — **of constants only**:
`[1, 2]` is fine, `[1, m.released]` is "non-constant list literal
elements are not supported yet".

A record-valued column can be indexed in `ORDER BY` (`ORDER BY dir.n`,
§6.4), but a `RETURN` item cannot refer to another item's alias: `RETURN
RECORD {a: x.t} AS r, r.a` is "Variable r not found".

```gql
MATCH (m:Movie)
RETURN CASE WHEN m.votes > 1000 THEN 'big' ELSE 'small' END AS size, COUNT(*) AS n
GROUP BY size
```

Comparison is ISO three-valued: any null operand makes the result
unknown, and an unknown `WHERE` drops the row. A genuine type mismatch
(`1 = 'a'`) is an error that empties the path rather than aborting.

### 6.4 Subqueries

```gql
-- EXISTS / NOT EXISTS: a MATCH+WHERE body, no RETURN
MATCH (p:Person) WHERE EXISTS { MATCH (p)-[:DIRECTED]->(:Movie) } RETURN p.name

-- VALUE: a correlated scalar, exactly one RETURN item
MATCH (m:Movie)
RETURN m.title,
       VALUE { MATCH (m)<-[:DIRECTED]-(d:Person) RETURN d.name LIMIT 1 } AS director
```

---

## 7. Schema and graph types

froGQL infers a schema from the data and keeps it as the reserved graph
type `DEFAULT`. Read it before writing queries — it tells you the labels,
the property names and which properties are optional:

```
.schema
```

```
Node types:
    movie = (:Movie {released INT, tagline STRING | NULL, title STRING, votes INT})
    person = (:Person {born INT | NULL, name STRING})

Edge types:
    (person)-[:ACTED_IN {roles LIST<STRING>}]->(movie)
    (person)-[:DIRECTED {}]->(movie)
```

`STRING | NULL` means *some* elements carry it. `movie` and `person` are
names this renderer assigns so the edge lines stay readable; they are not
part of the query language.

The persisted `DEFAULT` can be older than the data. `USE GRAPH TYPE
DEFAULT` re-infers it from the graph and saves it.

Declared graph types are also available — `CREATE GRAPH TYPE <name> AS {
… }`, `USE`, `DROP`, `SHOW GRAPH TYPES`, `VALIDATE GRAPH TYPE <name>` —
and a declared (non-`DEFAULT`) active type makes `INSERT` validate
against it.

---

## 8. Indexes

Indexes on unique-valued `(label, property)` pairs are built
automatically when the database opens; you rarely need to declare
anything. `.indexes` lists what exists.

```gql
CREATE BTREE INDEX ix_title ON :Movie(title)
DROP INDEX ix_title
SHOW INDEXES
```

`HASH` serves `=`; `BTREE` serves ranges and `ORDER BY`. Declared indexes
persist on `.save`; auto ones are rebuilt each open.

---

## 9. Vector search

Non-ISO extension. Vectors live in sidecar files built offline by
`vec_build`; the clause sits between the matches and the `RETURN`:

```gql
MATCH (i:Image)
NEAREST 10 i.hog TO [0.1, 0.2, 0.3] AS d
RETURN i.url, d
ORDER BY d
```

`TO` takes a float list or `VECTOR(<node>, '<attr>')`, which reads
another node's stored vector — "the ten images most like this one". A
correlated form (`NEAREST 10 b.hog TO VECTOR(a, 'hog')`) ranks once per
binding of `a`.

**A `NEAREST` query against an attribute with no sidecar parses, runs and
returns zero rows.** Zero rows there means "no vectors", not "nothing is
near". `.vec` (bare) prints the counters that tell the two apart.

Tune with `.vec strategy|source|level|tau-eps|memo-cuts|debug`; bare
`.vec` prints the last query's counters.

---

## 10. Start-up options

Defaults are good. Change them when you know which problem you have.

### CLI flags

| flag | effect |
|---|---|
| `--no-typecheck` | skip the typechecker. Also disables the guard that rejects unbounded repetition without a prefix, which then panics — use for benchmarking, not to get past an error |
| `--no-auto-indexes` | skip the secondary-index auto-build. The single biggest cut to peak memory |
| `--auto-indexes hash\|btree\|both\|none` | `hash` serves `=` only, at half the memory |

### Environment variables

| variable | effect |
|---|---|
| `FROGQL_LTJ_REPR=array` | six sorted arrays instead of the default LOUDS tries. ~2.9× more memory, 1.4–2.1× faster queries |
| `FROGQL_LTJ_PERSIST=0` | do not write the `<db>.ltj` index sidecar |
| `FROGQL_LTJ_SOURCE=build` | ignore an existing sidecar and rebuild |
| `FROGQL_DISABLE_LTJ_DELTA=1` | rebuild the whole join index after each mutation instead of keeping a delta |
| `FROGQL_VEO=simple` | fix the join variable order up front instead of re-picking it per binding |
| `FROGQL_VEC_STRATEGY=post\|pre\|interleave\|memo` | which vector-search algorithm runs |
| `FROGQL_VEC_SOURCE=hnsw\|localsort\|globalsort` | where the nearest-first ranking comes from |
| `FROGQL_TRACE_OPEN=1` | per-phase open latency, and why a sidecar was or was not used |
| `FROGQL_DEBUG_VEO=1` | the executed variable order |
| `FROGQL_DEBUG_INDEXES=1` | which indexes were built and which variables got pinned |

The defaults worth knowing about, because they change what a session
costs rather than what it answers:

- **The compact index is the default.** Roughly a third the memory of the
  arrays, somewhat slower per query. On a large graph the arrays do not
  fit, and an index that does not fit is infinitely slower than one that
  does.
- **The join index is written to `<db>.ltj` on first open** and read back
  afterwards. Building it is `O(E log E)` — 252 seconds, measured, on a
  617 M-edge graph — and it is a pure function of a graph that did not
  change. Delete the file to force a rebuild.
- **The variable order is adaptive**, re-picked per binding from the
  subtree sizes the index reports.
- **Mutations do not invalidate the join index**; a small delta is kept
  beside it.

---

## 11. Reading answers

- A `limit` you passed is an execution cap: `len(rows) == limit` means
  there may be more, not that there are exactly that many.
- `RETURN x` on a node or edge variable prints its labels and properties,
  not an id. Internal ids are not stable — saving renumbers them — so do
  not store one or use it as a key. Use a property you control.
- A pattern with no `RETURN` prints every bound variable the same way,
  plus the matched path.
- Duplicate rows are correct. GQL binding tables are **bags**: two
  parallel edges between the same pair are two matches even when the edge
  variable is not projected. `RETURN DISTINCT` is how you deduplicate.

---

## 12. What froGQL does **not** have

Do not write these. Each one is a parse error, and each is something an
agent has tried.

| you might write | what to do instead |
|---|---|
| `CREATE (n:Foo)` | `INSERT (:Foo)` |
| `MERGE (n:Foo)` | `MATCH` first, then `INSERT` if empty |
| `WITH x AS y` | a second `MATCH` clause, or `OPTIONAL MATCH` |
| `UNION` between two queries | path union `\|` between two patterns in one query (§5.3) |
| `-[:R*1..2]->` | `-[:R]->{1,2}` |
| `STARTS WITH`, `CONTAINS`, `=~` | no string operators at all |
| `upper()`, `lower()`, `size()`, `length()`, `substring()`, `split()`, `coalesce()` | no string or list functions |
| `$param` | inline the literal; there are no query parameters |
| `CALL`, `YIELD`, procedures | none |
| `FOREACH`, `UNWIND` | none |
| `SET n += {...}` | `SET n.a = …, n.b = …`, or `SET n = {...}` to replace |
| `LOAD CSV` | `--import-csv` at the CLI, or `frogql.import_csv` |
| `MATCH … INSERT … SET …` in one statement | one DML operation per statement |
| `ORDER BY` inside a subquery body other than `VALUE` | not supported |
| `x % 2` | `x MOD 2` |
| `[1, x.y]` | list literals hold constants only |
| `RETURN f(x) AS a, a + 1` | a `RETURN` item cannot reference another item's alias |

Also absent: `NEXT` / linear composition, percentile aggregates, and
user-defined functions.

---

## 13. If a query fails

1. **Read the message.** The typechecker reports the shape it wanted.
   `guaranteed empty` means the types can never match — usually a label
   or property that does not exist. Check `.schema` before guessing.
2. **Check `GROUP BY` placement** if the message mentions aggregates. It
   goes after `RETURN`.
3. **Check the quantifier** if the message mentions unbounded repetition:
   add `ANY SHORTEST` or `TRAIL`.
4. **`--no-typecheck` is not the fix.** It turns a type error into a
   wrong answer or a panic.
