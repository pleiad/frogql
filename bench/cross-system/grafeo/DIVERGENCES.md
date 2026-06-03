# Grafeo divergences from the canonical toml

Per-system translations diverge from `bench/ldbc-queries/ic<n>.toml` for
dialect or loader reasons. Each divergence below preserves the logical
query; the cross-system row-content hash oracle is the proof — IC2's
15/15 param rows are byte-identical to gqlite despite these.

## Dialect (query-level)

- **Parameters.** Grafeo uses `$personId`; the toml uses gqlite's
  `{personId}`. Pure syntax.
- **Label alternation.** Grafeo's parser rejects `(message:Comment|Post)`
  in a pattern (syntax error: `Expected RParen`). The union moves to a
  WHERE predicate: `WHERE (message:Comment OR message:Post)`. Same logical
  set. (graphqlite makes the same move for the same reason.)
- **GROUP BY is supported** — but the grouping key must be the property
  *expression* (`GROUP BY forum.id`), not the RETURN alias
  (`GROUP BY forumId` → `Undefined variable 'fid'`). The canonical toml
  already groups by expressions, so IC5/IC6 keep `GROUP BY` verbatim.
- **Multi-clause MATCH.** Grafeo 0.5.34 rejects a second top-level
  `MATCH` (`Expected RETURN, FINISH, or SELECT`). IC5 and IC6 fold their
  two MATCH clauses into one comma-joined MATCH. Same logical query.
- **WHERE ordering around OPTIONAL MATCH.** A `WHERE` *between* MATCH and
  OPTIONAL MATCH is rejected; the filter moves after the OPTIONAL MATCH
  (IC5). It still constrains the mandatory-bound `person`/`hm`, and the
  optional `post` is independent, so the result is identical.
- **ORDER BY after aggregation** must reference the RETURN alias, not the
  pre-aggregation variable: IC6 sorts by `tagName`, not `otherTag.name`
  (`otherTag` is out of scope once the group collapses).

## Loader-level

- **knows is stored forward-only** (1 row per pair), matching gqlite's
  undirected `~[:knows]~` over single-direction storage. The GQL uses
  any-direction `~[:knows]~`, so each stored edge matches in either
  direction. This is why the edge total (1 477 965) matches gqlite
  exactly rather than doubling.
- **No Company/Country sub-labels.** gqlite's LDBC loader synthesizes
  `:Company` (Organisation with `type=company`) and `:Country` (Place
  with `type=country`) sub-labels. The Grafeo loader stores only the base
  `:Organisation` / `:Place` labels. IC11's `(company:Company)` /
  `(country:Country)` must therefore be expressed as
  `(o:Organisation) WHERE o.type = 'company'` and
  `(pl:Place) WHERE pl.type = 'country'`. Logically equivalent on this
  dataset (every company IS an Organisation with that type).
- **Person MVAs not loaded.** The `email` and `language` multi-valued
  attributes are skipped — none of the implemented ICs (2,5,6,8,9,11)
  reference them. Adding them would mean a per-Person list-property
  update pass after the node import.
- **Empty `content`/`imageFile` → NULL.** Empty CSV cells in these
  columns load as NULL (not `""`), so `COALESCE(content, imageFile)` and
  `IS NULL` behave like gqlite's absent-property semantics. Verified by
  IC2's byte-exact parity (COALESCE is in IC2's projection).

## Engine note (out of scope for these ICs, but observed)

On a synthetic directed-triangle probe, Grafeo 0.5.34 over-counts a
cyclic (closed) join — it returns a match whose closing edge is absent.
None of the implemented LDBC ICs are cyclic joins, so this does not
affect the cross-system row-equivalence here. Documented in
`bench/grafeo-vs-frogql/README.md`.
