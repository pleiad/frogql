# DuckDB in-situ — divergences vs the canonical GQL tomls

Source of truth is `bench/ldbc-queries/ic<n>.toml`. DuckDB runs SQL
over relational views of the raw LDBC CSVs, so every divergence here
is about encoding a graph pattern relationally — the row sets are
verified byte-identical to frogQL's via the cross-system sha256 row
oracle (all 15 param rows for IC2, IC6, IC11).

## Baseline-level (apply to every IC)

### 1. No graph, no ingest: views over `read_csv`

There is no load step. `run.py` defines one VIEW per LDBC
entity/relation over `read_csv('<csv>', delim='|', header=true,
columns={...})`. Explicit `columns` disables type sniffing so the
schema is deterministic: IDs and LongDateFormatter dates (epoch
millis) are `BIGINT`, text is `VARCHAR`. Views are intentional — each
query re-scans the CSVs, which is the entire point of the in-situ
baseline (see README.md). Do not replace them with tables.

### 2. Undirected `knows` via symmetric-closure view

`person_knows_person_0_0.csv` stores each friendship once. GQL's
`~[:knows]~` is undirected, so the `knows` view is
`SELECT src,dst FROM raw UNION ALL SELECT dst,src FROM raw`. Same
convention as every other system (Kuzu materializes the reversed CSV
at load time; here it's a view, so the doubling cost is paid per scan).

### 3. Node inequality / node identity → id comparison

GQL compares bound nodes (`person <> otherPerson`,
`otherTag <> tag`); SQL compares their `id` columns. LDBC ids are
unique per entity, so this is exact.

### 4. Empty CSV field → NULL ≡ absent property

DuckDB's CSV reader maps an empty field to NULL by default; the
frogQL LDBC loader omits empty fields, making the property absent
(→ null at query time). The two conventions agree, which is what
makes `COALESCE(content, imageFile)` behave identically.

### 5. Parameter binding

`$name` placeholders bound from a Python dict without the `$` prefix
(`conn.execute(sql, {...})`) — same convention as the Kuzu runner.

## IC2

- **`(message:Comment|Post)` label union** → `UNION ALL` of a comment
  branch and a post branch inside a derived table, each joined to its
  own `*_hasCreator` mapping CSV. Relational encoding of the same
  union; no dedup is needed because comment and post ids are disjoint.
- **`COALESCE(message.content, message.imageFile)`**: the comment CSV
  has no `imageFile` column, so the comment branch projects `content`
  directly; the post branch keeps the COALESCE (image posts have empty
  `content` → NULL, see baseline §4).
- `<-[:hasCreator]-` direction → join on the `(message, person)`
  mapping CSVs; `~[:knows]~` → the symmetric `knows` view (§2).

## IC6

- **`~[:knows]~{1,2}` repetition** → a `fof` CTE that is the WALK
  *multiset*: `UNION ALL` of the 1-hop expansion and the 2-hop
  self-join, **no DISTINCT**. This mirrors the tomls' documented
  `no-with-distinct` divergence from the LDBC spec: a
  friend-of-friend reachable via multiple knows paths binds multiple
  times and inflates `COUNT(post)` identically in every engine of this
  bench. (A 2-hop walk returning to the start is excluded by
  `pid <> $personId`, same as the toml's `person <> otherPerson`.)
- **`COUNT(post)`** → `COUNT(*)`: `post` is never NULL in the inner
  join, so counting rows is identical.
- **`GROUP BY otherTag.name`** → `GROUP BY t2.name`; `ORDER BY
  postCount DESC, otherTag.name ASC` carried over verbatim (SQL allows
  ordering by the select alias).
- `(post:Post)` label check is implied: `post_hasCreator_person` /
  `post_hasTag_tag` rows only reference posts.

## IC11

- **`~[:knows]~{1,2}`** → same walk-multiset `fof` CTE as IC6
  (no-with-distinct semantics; duplicate `(otherPerson, company,
  workAt)` rows are intentional and hash-verified against frogQL).
- **`:Company` / `:Country` sub-labels** → `type` column filters on
  the `organisation` / `place` views (`'company'` / `'country'`,
  lowercase as in the static CSVs). Same encoding the Kuzu translation
  uses; frogQL synthesizes compound labels from the same column at
  import time. Same data, different encoding.
- **`CAST(personId AS INTEGER)` ORDER BY tie-break** → bare
  `personId`: the column is already `BIGINT` here, so the cast is a
  no-op (same note as the toml's `no-tointeger` divergence for IC2).
- `workAt.workFrom` edge property lives in the
  `person_workAt_organisation` CSV (`workFrom` column) — projected
  straight from the join, no encoding change.

## What's NOT divergent

- ORDER BY / LIMIT carried over verbatim per toml; sort keys are
  deterministic for the hash oracle (unique tie-breaks, and the only
  duplicate rows are full-row duplicates).
- Dates compare as epoch-millis integers on both sides (no date-type
  conversion anywhere).
- Result types: DuckDB returns Python-native int/str/None, the same
  shapes (`i`/`s`) frogQL's harness canonicalizes.
