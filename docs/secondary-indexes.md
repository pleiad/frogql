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

## Persistence

Auto-built indexes are memory-only — they live in `RefCell<SecondaryIndex>`
on the `LazyGraphStore` and `build_auto_indexes_bulk` reproduces them
on every open in a single O(N) pass over the node records. Storing them
on disk would just duplicate work and grow the `.gdb`.

Declared (DDL) indexes ARE persisted. `header.secondary_index_root` (a
new slot at bytes 100-103 of the file header) points at a chain of
`PageType::SecondaryIndex` pages that hold a JSON-encoded list of
`(name, label, prop, kind)` tuples. The save path (`.save` /
`Connection.save()`) writes the chain in the same atomic `.tmp` rename
that persists the catalog; the open path replays each entry via
`build_declared` after the auto-build, so

```
gql> CREATE BTREE INDEX msg_date ON :Message(creationDate);
gql> .save
gql> .quit
$ frogql my.gdb
gql> .indexes
msg_date  BTREE  :Message {creationDate}  286592  declared
```

works without re-issuing the DDL each session.

**Backward compatibility.** Legacy `.gdb` files written before this
slot existed have `secondary_index_root == 0` (the byte range was
reserved and zero-initialised). The loader treats `0` as "no DDL
list" — identical behaviour to the pre-persistence path. A doc-comment
TODO records that the `0` legacy interpretation can be dropped once
every stored database has been re-saved with the slot populated.

DML invalidates indexes for the duration of the session (the runtime
clears `secondary` after every successful INSERT / SET / REMOVE /
DELETE). The next `.save` re-builds them from the post-mutation graph
and persists the DDL list back into the new file.
