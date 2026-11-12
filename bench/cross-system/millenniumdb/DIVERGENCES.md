# MillenniumDB divergences from the canonical toml

Measured against MillenniumDB `dev` at commit `d67f3e7` (2026-11-11), the
newest public commit when this runner was written. Every claim below was
checked by running the query against a server built from that commit, on
the LDBC SF0.1 database `setup.py` builds. The join bugs (§3, items 3–5)
were also reproduced on the `v1.0.0` tag (`aaf65c0`, the Docker Hub image).

## 1. Which ICs run

| IC | Status | Checked against froGQL on SF0.1 |
|---|---|---|
| IC2 | runs | 15/15 row hashes identical |
| IC8 | runs | 15/15 row hashes identical |
| IC11 | runs | 15/15 row hashes identical |
| IC12 | runs | 15/15 identical content; the hash cannot match (list column, §4) |
| IC13 | runs | 15/15 row hashes identical (length computed by the runner, §2) |
| IC3, IC5, IC9 | `blocked/` | wrong rows: joins dropped (§3, items 3–5) |
| IC6 | `blocked/` | runs, but every count is lower: set semantics on `{1,2}` (§3, item 6) |
| IC1, IC4, IC7, IC14 | no translation | not expressible (§3, items 1, 2, 10) |

`run.py` only looks for `ic<n>.gql` next to itself, so an IC in `blocked/`
or without a file exits non-zero and `run_all.sh` lists it as `[FAIL]` in
`skipped.log`, the way it reports graphqlite's carve-outs. The `blocked/`
files are complete translations, each headed by why it is blocked; move
one up a level once MillenniumDB fixes the bug and the hash oracle will
say whether it then agrees.

Do not move a blocked IC back to get a number for it. IC3, IC5 and IC9
return wrong rows or do not finish, and IC6 answers a different question;
a latency for any of them is not comparable to froGQL's.

## 2. Dialect and rewrites (the ICs that run)

- **Parameters** are inlined as literals, like frogql's `ldbc_bench` and
  the grafeo runner. Strings are single-quoted; MillenniumDB rejects the
  ISO `''` escape, so `run.py` refuses a parameter holding a quote or a
  backslash (no SF0.1 parameter does).
- **`COLLECT_LIST` is spelled `COLLECT`** (IC12).
- **`GROUP BY friend`** in IC12, the node variable, instead of its three
  projected properties: the grammar allows at most two grouping keys
  (§3, item 7), and `friend` determines all three.
- **`CAST(id AS INTEGER)` is dropped from `ORDER BY`.** Both loaders store
  ids as integers, so the sort is identical.
- **IC13: path length in the runner.** `PATH_LENGTH` crashes the server
  (§3, item 2). The query returns the shortest path's edge group
  (`~[e:knows]~*`, `RETURN e`, printed `[_u1,_u2,...]`) and `run.py`
  reports its length as `shortestPathLength`, inside the timed region. The
  shortest-path search is still the engine's.

## 3. Engine bugs and gaps found

Each item has a query that shows it. The `books` database is MillenniumDB's
own example (`mdb import data/example/gql/books/books.gql books-db`); the
others run on this bench's SF0.1 database.

1. **`EXISTS` / `NOT EXISTS` segfault the server** (exit 139), in `WHERE`,
   under `NOT`, and in `RETURN` alike. `GQLParser.g4` has the rule
   (`#gqlExistsExpression`), `query_visitor.cc` has no visitor for it, so
   the expression comes out null.
   ```
   MATCH (a:Author) WHERE EXISTS { MATCH (a)-[:Published]->(b) } RETURN a.name   -- books
   ```
2. **`PATH_LENGTH(p)` segfaults the server**, for the same reason: the
   token is in `oneArgNumericFunctionName`, the visitor's dispatch has no
   branch for it. `CHAR_LENGTH` over a path or a group variable too.
   ```
   MATCH p = ANY SHORTEST (a:Author {name: 'John Doe'})~[:Knows]~*(b:Author {name: 'Jane Doe'})
   RETURN PATH_LENGTH(p)                                                          -- books
   ```
3. **A join is dropped when the shared variable is the target of the later
   edge**, whether the later pattern is a comma operand or a second
   `MATCH`. The result is the cross product.
   ```
   MATCH (f:Forum {id: 0})-[:hasMember]->(p:Person), (p)<-[:hasCreator]-(m) RETURN COUNT(*)
     -- 860 232 (= 3 members x 286 744 messages)
   MATCH (f:Forum {id: 0})-[:hasMember]->(p:Person)<-[:hasCreator]-(m) RETURN COUNT(*)
     -- 1 592, correct
   ```
   With the shared variable as the *source* of the later edge
   (`(p)-[:isLocatedIn]->(c)`), the comma join is correct.
4. **After a quantified path, an edge pointing into its endpoint is not
   joined**, even in one linear pattern.
   ```
   MATCH (a:Person {id: 32985348834013})~[:knows]~{1, 1}(o:Person)<-[:hasCreator]-(m) RETURN COUNT(*)
     -- 2 867 440
   MATCH (a:Person {id: 32985348834013})~[:knows]~(o:Person)<-[:hasCreator]-(m) RETURN COUNT(*)
     -- 4 176, correct
   ```
   An outgoing edge after the quantifier is joined correctly but slowly:
   `~[:knows]~{1, 2}(o)-[:isLocatedIn]->(c)` takes 23 s for 451 rows.
5. **A path-pattern union returns only its first operand's rows.**
   ```
   MATCH (p:Person {id: 32985348834013})~[:knows]~(o:Person)<-[:hasCreator]-(m)
       | (p:Person {id: 32985348834013})~[:knows]~()~[:knows]~(o:Person)<-[:hasCreator]-(m)
   RETURN COUNT(*)
     -- 4 176; the operands have 4 176 and 204 748 rows, froGQL returns 208 924
   ```
6. **Quantified paths return each endpoint once**, even with an explicit
   `ALL WALK`; ISO returns one row per walk.
   ```
   MATCH ALL WALK (p:Person {id: 32985348834013})~[:knows]~{1, 2}(o:Person) WHERE p <> o RETURN COUNT(*)
     -- 451; froGQL: 658 rows over 451 distinct endpoints
   ```
7. **`GROUP BY` takes at most two keys.** The grammar rule is
   `groupingElement (COMMA groupingElement)?`, where ISO has `*`.
8. **The Python driver's second query in a session segfaults the server.**
   `millenniumdb-driver` 1.2.1 over websocket: the first
   `MATCH (p:Person {id: 933}) RETURN p.id` answers, the same query sent
   again kills the server (exit 139). Over HTTP the same query five times
   in a row is fine, which is why `run.py` uses HTTP.
9. **The server's `--timeout` is not honoured for every query.** A
   union-in-parentheses variant of IC9 ran for 959 s under `--timeout 90`.
   The orchestrator's own `--timeout-s` is what bounds a run.
10. **Rejected as unsupported:** `OPTIONAL MATCH` (`Operation 'Optional
    match' not supported yet.`) and `NEXT` (`Operation 'Composite query'
    not supported yet.`). There are no value subqueries, no `RECORD`
    constructor and no list comprehension.

What that leaves out:

- **IC1**: two `OPTIONAL MATCH` clauses (10), `PATH_LENGTH` (2), `RECORD` (10).
- **IC4**: a `NOT EXISTS` anti-join (1). GQL path patterns have no
  negation, and the aggregate rewrite (group by friend and tag, keep the
  pairs whose earliest post is in range, re-aggregate by tag) needs `NEXT`
  (10).
- **IC7**: an arg-max per liker (`VALUE { … ORDER BY … LIMIT 1 }`) and
  `NOT EXISTS` (1, 10). One aggregation level cannot return an arg-max.
- **IC14**: `VALUE { … }` subqueries, list comprehension, `NODES(path)` (10).
- **IC3, IC5, IC9** each need a join that 3 or 4 drops. IC5's optional
  side also cannot be emulated with a union (5). IC9 written the other way
  round, `(message)-[:hasCreator]->(otherPerson)~[:knows]~{1, 2}(person)`,
  does not finish in 60 s.
- **IC6** can be written as one chain that avoids 3 and 4, and runs (1.5 to
  6 s a row), but 6 makes every post count lower than froGQL's.

## 4. Row-hash comparability

`_lib/row_hash.py` renders a list as Python `repr`, frogql's `ldbc_bench`
as Rust `Debug`, and the engines order `COLLECT` output differently.
**IC12's hash cannot match** for that reason, as for every other system
with a list column. Its content was compared separately, normalising the
lists on both sides, and all 15 parameter rows are identical. The other
four ICs return scalar columns and match byte for byte.

## 5. How results are read

`run.py` posts each query to the server over HTTP and reads the TSV
result. The writer prints a string as `"<raw>"` with **no escaping**, NULL
as a bare `NULL`, numbers bare, and a list as `"[...]"`. That is only
unambiguous while no stored string holds a tab, a quote or a newline. The
SF0.1 dump `setup.py` writes holds none of the three (checked: zero lines
with a tab, an escaped quote or an escaped backslash), and a row whose
cell count differs from the header fails the run instead of being misread.

## 6. Loader

`setup.py` mirrors froGQL's own LDBC loader rule for rule (see its
docstring): the same labels (`:Place :City`, `:Organisation :Company`, the
`type` column dropped as a property), the same int → float → bool → string
typing, empty cells absent, `email` / `speaks` as list properties, and
**`knows` as one undirected edge per CSV row** (`a~b`), which is how
froGQL stores it. Totals match froGQL: 327 588 nodes, 1 477 965 edges
(1 463 892 directed, 14 073 undirected).
