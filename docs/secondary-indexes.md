# Secondary indexes

froGQL auto-builds hash indexes on `(label, prop)` pairs whose values are
unique within the label, in a single O(N) pass at `LazyGraphStore::open`.
On the LDBC SF0.1 dataset that captures `Person.id`, `Tag.name`,
`Country.name`, `TagClass.name`, every other `*_id` column the loader
produced — 26 indexes in total, no DDL required. The LTJ optimizer
constant-folds any `NodeAttrCmp { Eq, value }` predicate that hits an
index, substitutes the resolved NodeId in every triple position, and
excludes the variable from the VEO so leapfrog never enumerates it.

Measured impact on **LDBC IC2** (`MATCH (p:Person {id: $personId})~[:knows]~...`
over `bench/data/ldbc-sf0.1.gdb`, 15 params × 3 iters, lazy backend,
`--limit 20`):

| | Median | Range |
|---|---|---|
| Without secondary index (`GQLITE_DISABLE_INDEX_FOLD=1`) | 2417 ms | 2317–2582 ms |
| With secondary index (default) | **1377 ms** | 1363–1392 ms |
| **Speedup** | **1.76×** | |

IC2 itself uses a top-level `Comment | Post` union that falls back to
hash-join, but each branch independently decomposes into LTJ-eligible
triples and benefits from the start-node pin. Diagnostic env vars:
`GQLITE_DEBUG_INDEXES=1` prints the auto-built indexes and pinned
variables; `GQLITE_DISABLE_INDEX_FOLD=1` reverts to the pre-index plan
for A/B benchmarking.

## Declared indexes (`CREATE INDEX` DDL)

For `(label, prop)` pairs the auto-builder doesn't cover (because the
values aren't unique), declare the index explicitly:

```
gql> CREATE BTREE INDEX msg_date ON :Message(creationDate);
INDEX 'msg_date' created (BTREE on (:Message {creationDate}), 286592 entries) in 0.31s.

gql> CREATE HASH INDEX person_first ON :Person(firstName);
INDEX 'person_first' created (HASH on (:Person {firstName}), 587 entries) in 0.01s.

gql> SHOW INDEXES;     -- or .indexes meta-command
gql> DROP INDEX msg_date;
```

Both prefix (`CREATE BTREE INDEX foo ...`) and suffix (`CREATE INDEX foo
... USING BTREE`) syntaxes are accepted; HASH is the default kind.
HASH and BTREE coexist on the same `(label, prop)` pair — they serve
different query patterns and the LTJ optimizer picks the right one per
filter.

The optimizer wires both kinds into the LTJ pre-pass:

- `NodeAttrCmp { Eq, value }` → hash lookup, constant-fold or NodeInSet.
- `NodeAttrCmp { <, <=, >, >=, value }` → btree range lookup,
  precomputed sorted set, replace the per-row property comparison with
  an O(log n) binary-search membership test (`FilterKind::NodeInSet`).

All indexes are in-memory (rebuilt every open). Persistence in the .gdb
file header chain — so declared indexes survive close/reopen — is the
next step on the roadmap.
