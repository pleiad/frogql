-- SQL translation of bench/ldbc-queries/ic6.toml for DuckDB in-situ
-- (views over the raw LDBC CSVs; see run.py for the view DDL).
--
-- Source-of-truth: bench/ldbc-queries/ic6.toml.
--
-- Divergences vs the GQL toml (see DIVERGENCES.md):
--   * ~[:knows]~{1,2}: the `fof` CTE is the WALK multiset — UNION ALL
--     of the 1-hop and 2-hop expansions over the symmetric `knows`
--     view, with NO dedup. The tomls deliberately keep flat-chain
--     (no-WITH-DISTINCT) semantics: a friend reachable via multiple
--     knows paths binds multiple times and inflates COUNT(post) the
--     same way in every engine of this bench.
--   * `otherPerson:Person` label check is implied (knows only links
--     persons); `person <> otherPerson` -> pid <> $personId.
--   * `otherTag <> tag` (node inequality) -> tag-id inequality.
--   * COUNT(post) -> COUNT(*): post is never NULL in the inner join,
--     so counting rows is identical.
--   * GROUP BY otherTag.name -> GROUP BY t2.name (same key).
WITH fof AS (
    SELECT dst AS pid FROM knows WHERE src = $personId
    UNION ALL
    SELECT k2.dst AS pid
    FROM knows k1
    JOIN knows k2 ON k2.src = k1.dst
    WHERE k1.src = $personId
)
SELECT t2.name  AS tagName,
       COUNT(*) AS postCount
FROM fof
JOIN post_hasCreator phc ON phc.personId = fof.pid
JOIN post_hasTag pt1 ON pt1.postId = phc.postId
JOIN tag t1 ON t1.id = pt1.tagId AND t1.name = $tagName
JOIN post_hasTag pt2 ON pt2.postId = phc.postId
JOIN tag t2 ON t2.id = pt2.tagId
WHERE fof.pid <> $personId
  AND t2.id <> t1.id
GROUP BY t2.name
ORDER BY postCount DESC, tagName ASC
LIMIT 10
