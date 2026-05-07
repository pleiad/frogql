# Query Language

froGQL implements ISO GQL path pattern matching. This page covers the
read-only surface (queries that return rows). For mutations see
[`data-modification.md`](data-modification.md); for schema declarations see
[`graph-types.md`](graph-types.md).

## Full queries

```
MATCH <pattern> [WHERE <condition>] [RETURN <expressions>]
OPTIONAL MATCH <pattern> [WHERE <condition>]
```

`MATCH` is optional on the first clause (bare patterns parse). `OPTIONAL MATCH`
is supported as a top-level clause and follows ISO left-join semantics: rows
from the previous match are preserved even when the optional pattern fails to
bind, and the unbound variables become `null`.

```
gql> MATCH (p: Person)
     OPTIONAL MATCH (p)-[:DIRECTED]->(m: Movie)
     RETURN p.name, m.title
"Alice"   | null            -- Alice never directed
"Lana W." | "The Matrix"
```

## Path patterns

```
()                              any node
(x: Account)                    labeled node bound to x
(:Person & Teacher)             multiple labels (conjunction)
-[:Transfer]->                  directed labeled edge
<-[:Knows]-                     reverse direction
~[:Friends]~                    undirected edge
(x)-[:Transfer]->(y)            traversal
```

## Comma-join (multi-pattern queries)

```
(a)-[]->(b), (a)-[]->(c)                    star pattern
(a)-[]->(b), (b)-[]->(c), (c)-[]->(a)       triangle
```

Semantics: cross-product of results where shared variables unify.

## WHERE filters

```
MATCH (x: Account) WHERE x.amount > 1000 and x.active = true
MATCH (x) WHERE x.name = 'Alice'
MATCH (x) WHERE x.age is int              -- type test
MATCH (x) WHERE not x.blocked
MATCH (x) WHERE x.deleted_at IS NULL      -- absent properties read as null
MATCH (x) WHERE x.email IS NOT NULL
```

`null` is a first-class value with three-valued logic in comparisons (a
predicate involving `null` returns false and the row is dropped).
Aggregates skip `null` and empty aggregates emit `null`. Equality and
range pushdowns (`= != < <= > >=`) on node properties are evaluated
inside the LTJ loop when present.

## RETURN projection

```
MATCH (p: Person) -[:ACTED_IN]-> (m: Movie) RETURN p.name, m.title AS movie
MATCH (p: Person) -[:DIRECTED]-> (m: Movie) RETURN DISTINCT p.name
```

## EXISTS / NOT EXISTS

Boolean predicates over a subquery body. Use them to keep or drop outer
rows based on whether the body has any match.

```
MATCH (p: Person) WHERE EXISTS { (p)-[:ACTED_IN]->(:Movie) } RETURN p.name
MATCH (p: Person) WHERE NOT EXISTS { (p)-[:DIRECTED]->(:Movie) } RETURN p.name
```

The body accepts one or more `MATCH` / `OPTIONAL MATCH` clauses with
`WHERE` filters. `RETURN`, `GROUP BY`, and `LIMIT` are not allowed
inside the braces — the body's job is proving non-emptiness, not
projecting a result table.

Outer-bound variables are visible inside via correlation; inner-only
bindings stay local. References to inner variables from outside the
body are rejected at compile time:

```
MATCH (p) WHERE EXISTS { (p)-[:KNOWS]->(b) } RETURN p.name, b.name
                                             ^^^^^^^^^^^^^^^^^^^^
                                             error: b not bound
```

When the active graph type proves the body unsatisfiable (a label or
property the schema rejects), the optimiser folds `EXISTS` to `false`
and `NOT EXISTS` to `true` and the runtime never evaluates the body.

Two regimes at runtime:

- **Uncorrelated** body (no shared variable with the outer scope) runs
  once with `limit=1`; the bool result is reused across every outer
  row.
- **Correlated** body runs once with no limit; rows are projected onto
  the correlation variables and stored in a `HashSet`. Per outer row
  the predicate is one O(1) hash probe — semi-join for `EXISTS`, anti-
  join for `NOT EXISTS`.

## Repetition

```
(-[:Transfer]->){1,3}           1 to 3 hops (variables bound to lists)
(-[x]->){2,2}                   x ↦ [e1, e2]
((-[x]->){1,2}){1,2}            x ↦ [[e1], [e2, e3]]
```

## Labels

```
(:Person & Teacher)             conjunction (both)
(:Person | Company)             disjunction (either)
(:!Admin)                       negation (not)
```
