# Secondary indexes

froGQL auto-builds hash **and btree** indexes on `(label, prop)` pairs whose
values are unique within the label, in a single O(N) pass at
`LazyGraphStore::open`.
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
| Without secondary index (`FROGQL_DISABLE_INDEX_FOLD=1`) | 2417 ms | 2317–2582 ms |
| With secondary index (default) | **1377 ms** | 1363–1392 ms |
| **Speedup** | **1.76×** | |

IC2 itself uses a top-level `Comment | Post` union that falls back to
hash-join, but each branch independently decomposes into LTJ-eligible
triples and benefits from the start-node pin. Diagnostic env vars:
`FROGQL_DEBUG_INDEXES=1` prints the auto-built indexes and pinned
variables; `FROGQL_DISABLE_INDEX_FOLD=1` reverts to the pre-index plan
for A/B benchmarking.

## Choosing which kinds get built

`FROGQL_AUTO_INDEX_KINDS=both|hash|btree|none` (CLI: `--auto-indexes <k>`)
controls what the auto-builder produces. `both` is the default.

| kind | serves | consulted by |
|---|---|---|
| hash | `x.p = v` | `fold_indexed_constants`, `get_candidate_nodes` |
| btree | `x.p < v`, `ORDER BY x.p` | `fold_range_filters`, `try_btree_ltj_real` |

The btree is a **full second copy of the postings**, so a workload of pure
equality predicates pays for it and never reads it. At SF0.1 the pair costs
a few MiB and the setting is noise. On a 160 M-node RDF dump it is ~34 GiB
of a 123 GiB machine — the difference between opening the database and
being killed by the OOM reaper:

| setting | index heap |
|---|---|
| `both` | ~34 GiB |
| `hash` | ~13 GiB |
| `none` | 0, and every `x.id = v` becomes a scan |

`--no-auto-indexes` predates this and is the all-or-nothing form; it is
`--auto-indexes none`.

Whatever is built, the answer is the same: an index is an accelerator, and
declining one may only cost time. `tests/auto_index_memory_test.rs` pins
every setting against a full scan.

## Postings are inline when single

A value's node ids are a `Posting`:

```rust
enum Posting { One(Id), Many(Box<Vec<Id>>) }
```

16 bytes, and heap-free in the `One` case. That case is not an
optimization for the common path — on an auto index it is the *only* case,
because the auto-builder only indexes a `(label, prop)` whose values are
unique within the label.

The `Vec<Id>` it replaced cost 24 bytes of header plus a heap block the
allocator rounds up to 32, to carry a single 4-byte id, once per node, in
both the hash and the btree. At 160 M nodes that is ~9.6 GiB of allocator
padding around 1.2 GiB of ids, spread over 320 M tiny allocations. Measured
by the index's own accounting: 76 → 40 bytes an entry.

`Many` is boxed so the enum stays 16 bytes: an inline `Vec` would put its
three-word header in every `One` as well, which is the cost the type exists
to avoid. `as_slice` returns `slice::from_ref` for `One`, so every reader
keeps the `&[Id]` it had and cannot tell the two representations apart.

## A pattern with no edges reaches the index

`MATCH (a:Img) WHERE a.id = 7` has no edges, so it never decomposes into
triples, so the LTJ constant-folding pre-pass never runs — and for a long
time that pre-pass was the *only* caller of `lookup_node_eq` in the engine.
The query scanned every node carrying the label with a hash index sitting
beside it unused: 0.606 s against 0.000 s at a million nodes, and 56 s on
the 160 M-node dump.

`Runtime::get_candidate_nodes` now asks `indexed_candidates` before the
label sets. The claim is a **narrowing only** — `filter_node` still runs
over whatever comes back — so a superset is safe and a subset would not be.
Two things make it a superset:

- `LabelType::required_labels()` is empty for a disjunction, so `(x:A|B)`
  falls back to the label sets rather than narrowing wrongly to `A`;
- an index on `(l, attr)` holds every `l`-node that carries `attr`, and one
  that does not carry it reads as null, which no `=` satisfies.

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

## Which values can be keyed

`IndexKey` covers `Int`, `Float`, `Str` and `Bool`. Lists and records are not
indexable (no obvious total order), and a null property is absent from the
node record altogether, so there is nothing to index and nothing to miss.

**Numbers are one key domain, not two** (issue #96). A float whose value is an
exact integer within `i64` is stored as an `Int` key, so `3` and `3.0` share
one entry; every other float is held as its order-preserving bit image, which
keeps the btree in float order. Comparing an `Int` key against a `Float` key
widens to `f64` — the same widening the runtime's `cmp_values` applies.

That last point is the invariant the whole feature rests on: **whether a
predicate is answered from an index or from a scan must not change the
answer.** The index does not need to be mathematically exact, it needs to
agree with the runtime's own comparison. Anything that changes the engine's
numeric comparison semantics has to change `IndexKey`'s ordering with it.

Before this, floats were dropped at build time, which cost more than speed: a
property mixing ints and floats produced a *partial* index that every consumer
read as complete, so float-valued rows silently vanished from range filters,
`ORDER BY` and equality (`p.m = 3` did not find a node holding `3.0`). The
auto-builder was safe only by accident — it requires every node of the label
to contribute an indexable value, so one float disabled it outright, which is
also why a float property was never accelerated. `CREATE INDEX` had no such
guard. Both halves were the same missing key type, seen from either side of
that check.

One residual: `build_declared` still has no completeness guard, so a manual
index on a property holding a **list** or **record** value is partial in the
same way. Floats were the only common case, but the guard is the general fix.

## Persistence

Auto-built indexes are memory-only — they live in `RefCell<SecondaryIndex>`
on the `LazyGraphStore` and `build_auto_indexes_bulk` reproduces them
on every open in a single O(N) pass over the node records. Storing them
on disk would just duplicate work and grow the `.gdb`.

That reasoning is sound at LDBC scale, where the pass is 0.54 s. On a
160 M-node dump it is ~74 s of every open, and there is no way to avoid
paying it other than declining the index (`--auto-indexes none`) and taking
the scans. The LTJ index has an answer for this — `ltj_build` writes it to
a sidecar, see `docs/modes-options.md` §3.1 — and the secondary index does
not yet.

Note that even a *declared* index is recomputed at open: what
`header.secondary_index_root` persists is the declaration, not the
contents, and the open path replays it through `build_declared`, which
scans.

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
