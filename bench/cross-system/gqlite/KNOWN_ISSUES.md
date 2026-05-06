# gqlite — known correctness issues that affect this bench

Issues found in gqlite itself (not in the bench harness) that affect
the cross-system row-equivalence comparison. Each one is filed as a
follow-up; the bench surfaces them as `WARN row N: HASH DISAGREES`
in `comparison.txt`'s row-content-equivalence section. Until the
issues are fixed, those WARNs are *expected* — the latency numbers
remain meaningful (gqlite is doing comparable join work) but the
result rows differ from the spec.

## #1 — Label-disjunction in a node descriptor doesn't filter the join

**Surfaced by**: IC2, IC8, IC9 cross-system smoke. gqlite returns 1
phantom row per IC2 cell (a Forum admitted to a `(m: Comment | Post)`
match through `<-[:hasCreator]-`) where Kuzu correctly returns no
such row. Reduced repro on `bench/data/ldbc-sf0.1.gdb`:

```cypher
MATCH (f:Person {id: 13194139533535})<-[:hasCreator]-(m: Comment) WHERE m.id = 1099511638444 RETURN m.id;
-- 0 rows ✓ (Forum 1099511638444 is not a Comment)

MATCH (f:Person {id: 13194139533535})<-[:hasCreator]-(m: Post) WHERE m.id = 1099511638444 RETURN m.id;
-- 0 rows ✓

MATCH (f:Person {id: 13194139533535})<-[:hasCreator]-(m: Comment | Post) WHERE m.id = 1099511638444 RETURN m.id;
-- 1 row  ❌ admits a Forum that has no `:hasCreator` edge to f.
```

`Comment` alone or `Post` alone correctly excludes the Forum.
The disjunction `Comment | Post` admits it.

**Located in code**: `src/runtime/ltj/pattern_extract.rs::node_var()`
emits `FilterKind::NodeLabel` filters via
`d.dtype.label.required_labels()`. That helper
(`src/typing/label_type.rs`) intentionally returns `[]` for
`LabelType::Or` — *"Does not include labels under Or or Neg (those
can't narrow the search)"* (verbatim from the doc-comment). Because
no NodeLabel filter is created for the disjunction, the LTJ runner
binds `m` to any node reachable via the edge, ignoring the label
constraint. The optimizer's index-fold path doesn't catch this
either; it relies on the same `required_labels()` source.

**Fix sketch**: add a `FilterKind::NodeLabelOr { var_id, labels:
Vec<String> }` variant, extract disjunctions in `node_var()`,
handle the new variant in `check_filters()` (`runtime/ltj/algorithm.rs`).
Surface ~3 files, no schema changes; the integration suite would
catch any regressions.

**Impact on the cross-system bench**:
- IC2 / IC8 / IC9: gqlite's hash differs from Kuzu's. Empirically
  the differences are SUBSTANTIAL, not just a few stray rows.
  Measured on the IC2 smoke against `bench/data/ldbc-sf0.1.gdb`
  (15 params rows × `LIMIT 20` ⇒ 20 rows per cell, gqlite vs Kuzu):

  | params row | rows agreed | rows gqlite-only | rows kuzu-only |
  |:---:|:---:|:---:|:---:|
  | 0  | 19 | 1  | 1  |
  | 1  | 13 | 7  | 7  |
  | 2  | 11 | 9  | 9  |
  | 7  |  5 | 15 | 15 |
  | 12 |  6 | 14 | 14 |
  | 14 | 18 | 2  | 2  |

  Most rows have 5–15 spurious gqlite-only rows that displace real
  Comment/Post matches under the `LIMIT 20` cap. The bench's row-
  content equivalence section will report a WARN on every (IC2, row)
  cell across all 15 params rows.

- IC5 / IC6 / IC11: not affected — they don't use `Comment | Post`
  disjunction.

- Latency: the work gqlite does is comparable in shape to the
  post-fix work (same join structure, just over-binding `m`), so
  IC2 latency is meaningful for relative comparisons. Absolute
  latency will likely *drop* when this is fixed because the LTJ
  inner loop has fewer phantom binds to enumerate.
