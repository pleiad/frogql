# CLTJ Benchmark Replication Plan for gqlrust

## Context
The CompactLTJ paper (vldbj25.pdf in cltj/) benchmarks Leapfrog Triejoin on RDF graphs.
We want to replicate their benchmarks using gqlrust to compare performance.

## Dataset
- **soc-LiveJournal1** from SNAP (4.8M nodes, 69M directed unlabeled edges)
- Download script: `bench/scripts/download_livejournal.sh`
- Zenodo mirror (CLTJ's copy): https://zenodo.org/records/15117967

## What's Done

### 1. Folder structure: `bench/{data,queries,scripts,results}`

### 2. Converter: `src/bin/convert_edgelist.rs`
- Reads SNAP tab-separated edge list format
- Writes directly to .gql pager format (no in-memory Graph needed)
- Supports `--limit N` for testing with subsets
- Tested and compiles

### 3. Query generator: `bench/scripts/generate_queries.py`
- Generates 11 shape query files in GQL syntax
- Shapes: 2-comb, 3-path, 4-path, 1-tree, 2-tree, 3-cycle, 4-cycle,
  3-clique, 4-clique, 2-3-lollipop, 3-4-lollipop

### 4. Generated queries: `bench/queries/*.gql`
All-variable queries (no constant anchoring).

## What's Remaining

### 5. Comma-join support (Q1, Q2 syntax)
**REQUIRED before benchmarking.** The CLTJ BGP queries use dot-separated triple
patterns like `?v0 206 ?v2 . ?v0 289 ?v3`. These are multi-pattern joins.
In GQL, this corresponds to the comma operator: `Q1, Q2`.

Semantics: cross-product of paths from Q1 and Q2, keeping only rows where
environments (variable assignments) unify.

Once comma-join works, we can express CLTJ-style queries directly:
- Triangle: `(a) -[]-> (b), (b) -[]-> (c), (c) -[]-> (a)`
- Star: `(a) -[]-> (b), (a) -[]-> (c)`
- Path with constant: needs constant node ID support too

### 6. Constant node IDs in queries
The GQL parser doesn't support `(42)` as a constant node match.
Options: add WHERE filter on node id, or extend syntax.

### 7. Benchmark runner binary
- Load .gql file (LazyGraphStore or DiskGraphStore for scale)
- Read query files, compile and run each query
- Measure time in nanoseconds
- Output: `query_id;result_count;elapsed_ns`
- Support result limit and timeout

### 8. Orchestration script
- Shell script analogous to cltj/src/bench/bench-socLJ.sh
- Builds index, runs all shapes, collects results

## Key Differences from CLTJ

| Aspect | CLTJ BGP | GQL |
|--------|----------|-----|
| Data model | RDF triples (s,p,o) | Property graph |
| Query model | Set of triple patterns (BGP) | Path patterns + comma-join |
| Join algo | Leapfrog Triejoin (WCO) | Adjacency-driven + hash join |
| Index | 6 tries (SPO,SOP,...) | Adjacency lists + label index |
| Stars | Native (multi-triple BGP) | Via comma-join: `(a)-[]->(b), (a)-[]->(c)` |
| Cycles | Via shared variables | Via variable binding in path |

## CLTJ Benchmark Reference
- Script: `cltj/src/bench/bench-socLJ.sh`
- Queries: tab-separated BGP files (not in repo, generated externally)
- Output format: `query_id;result_count;elapsed_ns`
- Limits: 0 (unlimited) or 1000, timeout 1800s
