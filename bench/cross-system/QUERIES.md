# How we wrote the IC queries

This is the methodology behind every implemented IC in the
cross-system bench. Two principles shape it; the rest is process.

## The two principles

**1. Fairness across systems.** No system gets a query shape that
arbitrarily helps it. If gqlite uses an explicit `GROUP BY` while
graphqlite/Kuzu use the canonical Cypher `WITH ... ORDER BY ... LIMIT
... RETURN` arg-max idiom, both forms must compute the same logical
group key on the same logical input — neither form can sneak in a
selectivity advantage. Same MATCH shape, same WHERE predicates, same
sort key, same LIMIT, same projected columns in the same order.

**2. Spec faithfulness, in two dimensions.**
   - **Primary: the spec's high-level description** at
     `https://github.com/ldbc/ldbc_snb_docs/blob/main/query-specifications/interactive-complex-read-N.yaml`.
     This is the source of truth: "Recent messages by friends or
     friends of friends, before maxDate, ordered by creationDate
     DESC then id ASC, top 20." Whatever query we ship, it has to
     compute that description. Adversarial reading: a reviewer
     should be able to read the description and confirm the query
     does it without trusting our naming choices.
   - **Secondary: the reference Cypher** at
     `https://github.com/ldbc/ldbc_snb_interactive_v1_impls/blob/main/cypher/queries/interactive-complex-N.cypher`.
     Grounds the description in concrete pattern syntax + alias
     names. Useful for resolving ambiguity ("is the friend node the
     same as the message author?") and for naming conventions. Not
     binding when our parser can't express something the reference
     uses, but our query has to be semantically equivalent.

The first dimension catches structural drift; the second catches
naming drift. Both matter — IC5 was returning 0 rows on every params
row because the toml said `hm.creationDate > $minDate` while the
LDBC CSV column is `joinDate`. The reference Cypher caught that
ambiguity ("which date column?"); the description didn't.

## The translation pipeline

```
LDBC spec yaml  ──►  bench/ldbc-queries/icN.toml  ──►  bench/cross-system/<system>/icN.cypher
   (the spec)        (canonical for gqlite)              (per-system translation)
```

The toml is **gqlite's** canonical. Per-system Cypher files are
*translations of the toml*, not retranslations of the spec. This
matters: if the toml drifts from the spec, the per-system files
drift too — but they remain consistent with each other (the cross-
system fairness invariant). When the spec changes, we update the
toml first; the per-system files follow.

Each per-system Cypher's first comment-block points back to the toml
(`// Source-of-truth: bench/ldbc-queries/icN.toml.`) so a reader
isn't surprised by an alias drift.

## What kinds of divergences are OK

Not all divergences are spec-violations. We taxonomize them:

**Loader-level divergences** — same data, different surface naming.
Always documented in the toml's `[divergences]` table.
- Edge labels lowercase (`:knows` instead of spec's `:KNOWS`) —
  filename-stem convention.
- `(:Comment | Post)` instead of spec's `(:Message)` — our LDBC
  loader doesn't synthesize a `:Message` superlabel; both Comment
  and Post are first-class.
- `:Company` / `:Country` sub-labels (synthesized by the loader from
  the `type` column on Organisation/Place).
- graphqlite's `.ldbcId` instead of `.id` (graphqlite reserves `.id`
  for the loader's external_id).

**Dialect-level divergences** — same query, different surface syntax.
- gqlite's ISO GQL `(c:Comment | Post)` pattern alternation vs
  Cypher's `WHERE c:Comment OR c:Post` predicate vs Kuzu's
  `WHERE label(c) IN ["Comment", "Post"]` builtin. All three are
  query-level predicates, none encoded at the schema/loader level.
- gqlite's `~[:knows]~{1,2}` undirected variable-length hop vs
  Cypher's `-[:knows*1..2]-` any-direction (graphqlite/Kuzu both
  materialize the reverse `:knows` edge at load time, so the same
  set of (Person, friend) pairs matches).

**Parser-gap divergences** — the spec uses a feature gqlite's parser
doesn't have, and we work around with a semantically-equivalent
rewrite.
- `CAST(<id> AS INTEGER)` in ORDER BY → drop (LDBC IDs are already
  int; same sort).
- `WITH forum, count(post) AS postCount` → explicit `GROUP BY
  forum.title, forum.id` (no WITH in our parser; same group).
- `GROUP BY <RETURN-alias>` → `GROUP BY <raw expression>` (parser
  doesn't visit RETURN aliases when resolving GROUP BY items).

**Semantic divergences** — these are the dangerous ones. We *avoid*
them. If we can't avoid one, the IC stays `status = "blocked"` until
the parser feature lands.
- Substituting `WHERE node1.id <> node2.id` for `WHERE node1 <>
  node2` was tempting before bare-node compare landed — semantically
  equivalent on LDBC data but a different query. We waited for the
  parser feature instead.
- Dropping `ORDER BY` to make `LIMIT N` cheaper — different query.
- Replacing `count(c)` with `count(c.id)` — same on LDBC, but
  different query (the spec's `count(c)` works since
  `c` is a property-bag of which `id` is just one). We waited for
  bare-node aggregates instead.

The line: a divergence is fine if a reviewer reading the spec and
our query agrees they compute the same thing. A divergence is *not*
fine if it requires understanding the LDBC dataset's specific
structure to convince yourself they're equivalent.

## The cross-system fairness check

`compare_results.py` section "Row-content equivalence" hashes each
runner's iter-0 result rows (canonical sha256 over a `\x1f`-separated
encoding) and flags any (IC, params_row) where the three systems
disagree. With `ORDER BY` in every IC's toml the results are
deterministic, so byte-equal canonical text → identical hashes.

A hash mismatch is a **real** finding: either a translation bug in
one system, or a system-level disagreement on something we treated as
equivalent (e.g. NULL handling, COALESCE evaluation order). Either
way, it's a query-fairness violation that the bench would otherwise
hide behind matching result counts. Diff the sibling
`<system>.icN.rows.jsonl` files to localize.

This is **the** answer to "are we sure the queries are fair across
systems?" — at every overnight bench run, they're checked.

## The audit process

Parser features land asynchronously. Every blocked IC's toml has a
`blocked_reason` field listing the specific gaps. When a feature
lands, we re-audit:

1. Test each construct from the still-blocked tomls against current
   `main` via the REPL or a tiny test query. Don't trust the
   blocked_reason — it might be stale.
2. For every toml we can now flip from `blocked` to `implemented`:
   - Update the `query` field with our gqlite-friendly form
     (translating gaps that remain via the divergence taxonomy
     above).
   - Refresh the `[divergences]` table.
   - Drop satisfied features from `required_features`.
   - Smoke-run on at least one params row, verify non-empty result
     and matching shape.
3. For every toml that's still blocked, refresh the `blocked_reason`
   to drop already-resolved gaps and re-list what's truly missing.
4. Add per-system Cypher translations (`graphqlite/icN.cypher`,
   `kuzu/icN.cypher`).
5. Re-run the cross-system bench; the row-equivalence oracle catches
   any per-system drift introduced by the new translation.

The audit log lives in commit messages, not per-toml history. Each
audit pass is one or two commits with `bench:` or `bench(ldbc):`
prefixes.

## Per-IC details

Live in:
- `bench/ldbc-queries/icN.toml` — the canonical query for gqlite +
  the divergence table.
- `bench/cross-system/<system>/icN.cypher` — the per-system
  translation, with a comment-block summarizing system-specific
  divergences.
- `bench/cross-system/<system>/DIVERGENCES.md` — system-level
  divergences that span multiple ICs (e.g. graphqlite's `.ldbcId`,
  Kuzu's `label()` predicate).

When in doubt, the toml is the source-of-truth for what gqlite runs.
The reference Cypher (linked from the toml's spec URL) grounds the
toml. The cross-system Cypher files mirror the toml.
