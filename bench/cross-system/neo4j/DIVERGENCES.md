# Neo4j — divergences and integration notes

Neo4j 5 community (Docker, `neo4j:5`) is the cross-system bench's
production-system reference point. This file documents every semantic
carve-out between the canonical GQL in `bench/ldbc-queries/ic<N>.toml`
and the `ic<N>.cypher` translations, plus the loader- and
measurement-level conventions.

**Verification status:** all 12 implemented ICs (1, 2, 3, 4, 5, 6, 7,
8, 9, 11, 12, 13) are row-hash-verified against gqlite on LDBC SF0.1 —
15/15 substitution-parameter rows each, byte-identical canonical blobs
(see §3 for how structured columns are compared).

## 1. Data model (loader-level, mirrors kuzu/setup.py)

- **Node labels** Person, Comment, Post, Forum, Organisation, Place,
  Tag, TagClass with the LDBC CSV property names verbatim.
- **Sub-types as `type` property, not sub-labels.** The spec's
  `:Company` / `:University` / `:Country` / `:City` / `:Continent`
  are encoded as `Organisation.type` / `Place.type` (lowercase) and
  filtered in the queries (`WHERE company.type = 'company'`). Same
  encoding as Kuzu; gqlite reaches the same end-state via synthesized
  compound labels. The row-hash oracle confirms identical rows across
  encodings (IC1, IC3, IC11).
- **Relationship types lowercase** (`knows`, `hasCreator`, ...,
  filename-stem convention), including the multi-source/target rels:
  `hasCreator` (Comment→Person, Post→Person), `hasTag` (Comment/Post/
  Forum→Tag), `replyOf` (Comment→Comment, Comment→Post),
  `isLocatedIn` (Comment/Post/Person/Organisation→Place), `likes`
  (Person→Comment, Person→Post). Neo4j is schema-free on rel
  endpoints, so unlike Kuzu no multi-typed table declaration is
  needed — the CSVs just load into one rel type.
- **`knows` single direction.** The CSV records each pair once; we
  load only that direction and every query matches it undirected
  (`-[:knows]-`), yielding one row per pair — the same convention the
  row-hash oracle forced on Kuzu (a both-directions load doubles
  rows).
- **Person MVAs** `email` / `language` pre-aggregated from their
  one-row-per-(person,value) CSVs into list properties on Person.
  List order = CSV order, which matches gqlite's loader (IC1's
  friendEmails/friendLanguages hash-match relies on this).
- **Dates stay epoch-millis ints.** No Neo4j temporal types: the ICs
  do `$startDate + $durationDays * 86400000` int arithmetic and the
  hash oracle compares ints. (`DURATION({days: N})` in the toml
  desugars to exactly this.)
- **Empty CSV fields are not stored.** Absent property == null, so
  `coalesce(message.content, message.imageFile)` picks `imageFile`
  for image posts, matching gqlite's loader. Storing `''` instead
  would silently flip IC2/IC9 content cells.
- **Uniqueness constraints on `id` per label, created before load.**
  The analog of Kuzu's `PRIMARY KEY(id)`: backs both the loader's
  edge-phase `MATCH {id}` joins and the ICs' start-node lookups.

## 2. Dialect mapping (toml GQL → Neo4j 5 Cypher)

| toml construct | Neo4j 5 form | ICs |
|---|---|---|
| `(m:Comment\|Post)` alternation | `(m:Comment\|Post)` — native label expression, 1:1 | 2,3,7,8,9 |
| `~[:knows]~` (undirected) | `-[:knows]-` (any-direction over single-direction storage) | all |
| `~[:knows]~{1,2}` | `-[:knows*1..2]-` | 3,5,6,9,11 |
| `ANY SHORTEST ... {1,3}` | endpoints bound first, then `shortestPath((p)-[:knows*1..3]-(friend))` | 1 |
| `ANY SHORTEST ... ~*` | `OPTIONAL MATCH p = shortestPath((p1)-[:knows*]-(p2))`; NULL path → -1 via CASE | 13 |
| `PATH_LENGTH(path)` | `length(p)` | 1,13 |
| `[:isSubclassOf]->{0,}` under `ACYCLIC` | `-[:isSubclassOf*0..]->` (VLR never repeats a relationship; the hierarchy is a DAG, so unbounded is finite and ACYCLIC-equivalent) | 12 |
| `DURATION({days: N})` | `N * 86400000` (int ms) | 3,4 |
| `NOT EXISTS { MATCH ... }` | `NOT EXISTS { MATCH ... }` / `NOT EXISTS { (a)-[:r]-(b) }` | 4,7 |
| `VALUE { ... ORDER BY ... LIMIT 1 }` (arg-max) | `WITH ... ORDER BY ...` then `collect({...})[0]` per group | 7 |
| `RECORD {k: v}` | map literal `{k: v}` | 1,7 |
| `COLLECT_LIST(...)` | `collect(...)` (+ CASE wrap, see below) | 1,12 |
| `GROUP BY <vars/exprs>` | implicit Cypher grouping on non-aggregated RETURN items | 1,3,4,5,6,7,12 |
| `CAST(x AS INTEGER)`, `FLOOR` | `toInteger(...)`, `floor(...)` | 7 (ordering CASTs elsewhere are no-ops: ids are stored as ints) |

Per-IC notes beyond the table:

- **IC1 — collect null-elimination.** gqlite's `COLLECT_LIST(RECORD
  {...})` over an unmatched OPTIONAL row drops the record (aggregate
  null elimination → empty list). Cypher's `collect({...})` would
  keep one all-null map. The translation wraps the map in
  `CASE WHEN uni IS NULL THEN NULL ELSE {...} END`; `collect()`
  skips nulls, restoring the empty-list behavior. Verified by the
  hash oracle (rows with study-only / work-only friends).
- **IC1 — collect order pinned by `ORDER BY uni.id, company.id`.**
  gqlite produces the study×work cross-product in ascending
  organisation-id order (its OPTIONAL MATCH expansion binds nodes in
  internal-id order = CSV order = ascending LDBC id); Neo4j's natural
  traversal yields reverse insertion order. The pre-collect ORDER BY
  makes both engines' lists element-for-element equal. This pins an
  order the spec leaves unspecified — a determinism patch, not a
  semantics change.
- **IC5/6/9/11 — flat chain, no `WITH DISTINCT` dedup**: mirrors the
  toml's `no-with-distinct` divergence so all systems run the same
  variant; engine agreement (the hashes) is what's verified, not
  spec faithfulness.
- **IC7 — `minutesLatency`** = `toInteger(floor(Δms / 60000.0))`;
  latency is non-negative so trunc == FLOOR == the toml's
  `CAST(FLOOR(CAST(... AS FLOAT) / 60000.0) AS INTEGER)`.
- **IC13 — disconnected pairs**: endpoints matched first, then
  OPTIONAL shortestPath, so a no-path pair still yields one row with
  -1 (the toml's `CASE WHEN PATH_LENGTH(path) IS NULL`).

## 3. Row-hash oracle for structured columns

The canonical blob (`_lib/row_hash.py` ↔ `src/bin/ldbc_bench.rs`)
embeds structured cells (lists, records) via Rust's `{:?}` Debug of
froGQL's `Value`. The Neo4j driver returns Python lists/dicts, whose
`repr()` differs, so `run.py` re-encodes structured cells into the
same Debug form before hashing (`_encode_value`):

- `List([Str("a"), Int(1), ...])`, `Record({"k": Int(1), ...})` with
  record keys sorted (Value::Record is a BTreeMap);
- nested strings escaped per Rust's `escape_debug` (`\"`, `\\`,
  `\n`, and `\u{hex}` for non-printables — LDBC content contains
  U+00A0 NBSP, which gqlite's blob shows as `\u{a0}`);
- nested strings also rstripped, extending the oracle's top-level
  "loaders disagree on trailing whitespace" normalization into
  structured cells (gqlite's CSV loader trims trailing blanks; ours
  stores fields verbatim).

This is a re-encoding of the same logical value, not a row
transformation: scalar cells pass through untouched and the
canonicalizer treats the pre-encoded string exactly as the Rust side
emits it. With it, the list/record ICs (1, 7, 12) hash-match —
something the Kuzu runner (which passes raw reprs) documents as "not
expected".

- **IC12 `tagNames` order**: `collect(DISTINCT tag.name)` order is
  unspecified by Cypher; Neo4j's production order happens to match
  gqlite's on SF0.1 (both reflect tag insertion/id order). If a
  future dataset breaks the tie differently, this is the first place
  to look — the fix would be pinning an order on both sides, which
  the spec does not define.

## 4. Measurement conventions

- **Client-server, not embedded.** Neo4j runs as a daemon in Docker;
  every query pays bolt serialization + a localhost round-trip that
  the embedded systems (gqlite, Kuzu) don't. That overhead is part of
  Neo4j's user-facing interface and is NOT subtracted — same
  rationale as the Python-FFI caveat in `bench/cross-system/README.md`.
  Quote it when quoting numbers.
- **Latency window** = `session.run` + draining the result cursor.
  One driver + one session reused across all params/iters (mirrors
  the embedded runners' connection reuse). The query-plan cache warms
  on the first execution per IC, like Kuzu's.
- **Server resources**: heap 4G, page cache 2G (docker.sh). SF0.1 is
  ~small relative to both, so reads are cache-resident after warmup.
- **RSS line is client-side only** — the engine's memory lives in the
  container. Kept for output-schema parity; don't compare it against
  the embedded systems' RSS.

## 5. What's NOT divergent

- **Parameter binding** — `$name` placeholders with `$`-less dict
  keys, same convention as Kuzu.
- **Result iteration** — the bolt driver returns Python-native
  scalars (int / float / str / bool / None); ints survive 64-bit
  LDBC ids losslessly.
- **LIMIT / ORDER BY semantics** — Cypher's apply as the toml
  intends; no Kuzu-style `LIMIT` quirk after `WITH ... ORDER BY`
  (IC5's `WITH ... ORDER BY ... LIMIT 20 RETURN` is the canonical
  Cypher idiom and needs no giant-LIMIT workaround, cf. kuzu/ic7).
