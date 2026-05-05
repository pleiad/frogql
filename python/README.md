# froGQL

Embedded GQL graph database with single-file storage. Rust core, Python bindings via PyO3.

froGQL implements ISO GQL path pattern matching: `MATCH`, comma-joins, unions, repetitions (`{n,m}`), `OPTIONAL MATCH`, `EXISTS / NOT EXISTS`, `WHERE`, `RETURN`, `LIMIT`. The runtime uses Leapfrog Triejoin (CompactLTJ) as its primary join strategy — worst-case-optimal for multi-way joins, with measured 14×–4000× speedups over pairwise hash-join on social-graph workloads.

## Install

```bash
pip install frogql
```

Wheels ship for CPython 3.8+ on Linux (x86_64, aarch64), macOS (x86_64, arm64), and Windows (x86_64).

## Quick start

```python
import frogql

# Open or create a .gdb database
conn = frogql.open("movies.gdb")

# Run a query — returns a list of {alias: value} dicts.
# Use `AS name` in RETURN to pick the dict key; otherwise the
# projection falls back to col0, col1, ...
rows = conn.execute(
    "MATCH (p:Person)-[:ACTED_IN]->(m:Movie) "
    "WHERE m.released = 1999 "
    "RETURN p.name AS actor, m.title AS title",
    limit=10,
)
for row in rows:
    print(row["actor"], "->", row["title"])

# Inspect the graph
print(conn.node_count, conn.edge_count)
print(conn.schema())
```

### Bare patterns (no RETURN)

A query without `RETURN` projects each row as a dict of the matched
variables plus a special `_paths` key holding the full match:

```python
rows = conn.execute("(p:Person)-[:ACTED_IN]->(m:Movie)", limit=1)
row = rows[0]
row["p"]        # {"kind": "node", "id": ..., "labels": [...], "props": {...}}
row["m"]        # the movie node
row["_paths"]   # [[node_p, edge, node_m]] — list of paths, each a
                # list of node/edge dicts in match order
```

`_paths` is a list because comma-joined patterns produce one path per
joined sub-pattern. For a single pattern, `_paths[0]` is the full path.

## Data import

```python
# From JSON
frogql.import_json("graph.gdb", "graph.json")

# From a CSV directory with spanner_import_config.json
frogql.import_csv("graph.gdb", "path/to/csv_dir/")
```

## Graph types and indexes

The catalog persists inside the `.gdb` file. DDL is plain GQL:

```python
conn.execute("CREATE GRAPH TYPE movies { (:Movie {title STRING, released INT}) }")
conn.execute("USE GRAPH TYPE movies")
conn.execute("VALIDATE GRAPH TYPE movies")
conn.execute("CREATE BTREE INDEX ON :Movie(released)")
```

A `DEFAULT` graph type is auto-inferred at import time. Auto-built secondary indexes (hash + btree) cover unique `(label, prop)` pairs and are picked up by the optimizer for constant-folding and range filters.

## API surface

| Call | Returns |
|------|---------|
| `frogql.open(path)` | `Connection` |
| `frogql.import_json(db_path, json_path)` | `None` |
| `frogql.import_csv(db_path, csv_dir)` | `None` |
| `Connection.execute(query, limit=100)` | `list[dict]` (with `RETURN`: keys = aliases or `colN`; without `RETURN`: keys = pattern variables plus `_paths`) |
| `Connection.schema()` | `dict` |
| `Connection.graph_types()` | `list[dict]` |
| `Connection.node_count` / `Connection.edge_count` | `int` |

`Connection` is not thread-safe across Python threads (PyO3 `unsendable`).

## License

MIT. See `LICENSE` in the source repository.

## Links

- Source: https://github.com/pleiad/frogql
- Issues: https://github.com/pleiad/frogql/issues
