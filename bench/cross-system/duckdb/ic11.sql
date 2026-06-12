-- SQL translation of bench/ldbc-queries/ic11.toml for DuckDB in-situ
-- (views over the raw LDBC CSVs; see run.py for the view DDL).
--
-- Source-of-truth: bench/ldbc-queries/ic11.toml.
--
-- Divergences vs the GQL toml (see DIVERGENCES.md):
--   * ~[:knows]~{1,2}: `fof` is the WALK multiset (UNION ALL of 1-hop
--     and 2-hop, no dedup) — same flat-chain (no-WITH-DISTINCT)
--     semantics as the toml; duplicate (otherPerson, company, workAt)
--     rows are intentional and shared by every engine in this bench.
--   * :Company / :Country sub-labels -> `type` column filters on the
--     organisation / place views ('company' / 'country'), same
--     encoding the Kuzu translation uses; gqlite synthesizes compound
--     labels from the same column at load time.
--   * `CAST(personId AS INTEGER)` tie-break: personId is already
--     BIGINT here, so the bare column sorts identically.
WITH fof AS (
    SELECT dst AS pid FROM knows WHERE src = $personId
    UNION ALL
    SELECT k2.dst AS pid
    FROM knows k1
    JOIN knows k2 ON k2.src = k1.dst
    WHERE k1.src = $personId
)
SELECT p.id        AS personId,
       p.firstName AS personFirstName,
       p.lastName  AS personLastName,
       o.name      AS organizationName,
       w.workFrom  AS organizationWorkFromYear
FROM fof
JOIN person p ON p.id = fof.pid
JOIN workAt w ON w.personId = fof.pid
JOIN organisation o ON o.id = w.organisationId AND o.type = 'company'
JOIN organisation_isLocatedIn oil ON oil.organisationId = o.id
JOIN place pl ON pl.id = oil.placeId AND pl.type = 'country'
WHERE fof.pid <> $personId
  AND pl.name = $countryName
  AND w.workFrom < $workFromYear
ORDER BY organizationWorkFromYear ASC, personId ASC, organizationName DESC
LIMIT 10
