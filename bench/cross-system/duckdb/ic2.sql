-- SQL translation of bench/ldbc-queries/ic2.toml for DuckDB in-situ
-- (views over the raw LDBC CSVs; see run.py for the view DDL).
--
-- Source-of-truth: bench/ldbc-queries/ic2.toml. Mirrors the toml's
-- shape (anonymous start person, COALESCE(content, imageFile),
-- ORDER BY, <=, canonical column names).
--
-- Divergences vs the GQL toml (see DIVERGENCES.md):
--   * ~[:knows]~ undirected  -> the `knows` view is the symmetric
--     closure (UNION ALL of both orientations of the one-direction CSV).
--   * (message:Comment|Post) -> UNION ALL of the comment branch and
--     the post branch (relational encoding of the label union).
--   * COALESCE(content, imageFile): comments have no imageFile column,
--     so their branch projects content directly; the post branch keeps
--     the COALESCE (DuckDB reads empty CSV fields as NULL, matching
--     the gqlite loader's "empty field => property absent").
SELECT f.id            AS personId,
       f.firstName     AS personFirstName,
       f.lastName      AS personLastName,
       m.id            AS postOrCommentId,
       m.content       AS postOrCommentContent,
       m.creationDate  AS postOrCommentCreationDate
FROM knows k
JOIN person f ON f.id = k.dst
JOIN (
    SELECT hc.personId AS creatorId, c.id, c.content, c.creationDate
    FROM comment c
    JOIN comment_hasCreator hc ON hc.commentId = c.id
    UNION ALL
    SELECT hc.personId AS creatorId, p.id,
           COALESCE(p.content, p.imageFile) AS content, p.creationDate
    FROM post p
    JOIN post_hasCreator hc ON hc.postId = p.id
) m ON m.creatorId = f.id
WHERE k.src = $personId
  AND m.creationDate <= $maxDate
ORDER BY postOrCommentCreationDate DESC, postOrCommentId ASC
LIMIT 20
