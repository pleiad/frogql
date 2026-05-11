# frogql — Node.js bindings

Native Node.js addon for [froGQL](https://github.com/pleiad/frogql), an embedded GQL graph database with ISO GQL path patterns. Built via [napi-rs](https://napi.rs); ships per-platform prebuilds (Linux x64+arm64, macOS x64+arm64, Windows x64).

## Use case: VSCode extension

The Node addon runs in-process inside the VSCode extension host. Open a `.gdb` once, execute queries, get JS objects back. No subprocess, no IPC, no serialisation overhead.

## Install

```bash
npm install frogql
```

## Quick start

```ts
import { open } from "frogql";

const conn = open("movies.gdb");
console.log(conn.nodeCount, conn.edgeCount);

const rows = conn.execute(
  "MATCH (m:Movie {released: 1999})<-[:ACTED_IN]-(p:Person) RETURN p.name AS actor, m.title AS film",
  20
);
// [{ actor: "Keanu Reeves", film: "The Matrix" }, ...]
```

## API surface

Mirrors the Python `frogql` package on PyPI.

### Module functions

- `open(path: string): Connection` — open or create a `.gdb`. Eagerly warms the LTJ TripleIndex.
- `importJson(dbPath: string, jsonPath: string): void` — write a fresh `.gdb` from a JSON graph file.
- `importCsv(dbPath: string, csvDir: string): void` — write a fresh `.gdb` from a directory of CSVs configured via `spanner_import_config.json`.

### `Connection`

Properties:

- `nodeCount: number` — live node count (base + overlay).
- `edgeCount: number` — live edge count (base + overlay).

Methods:

- `execute(query: string, limit?: number)` — run any GQL statement. Return shape depends on the statement (see below). `limit` defaults to 100; ignored for DDL / DML.
- `save(): void` — persist the merged base + overlay back to the file the connection was opened from. Until you call this, DML mutations and `CREATE INDEX` declarations live only in memory.
- `schema()` — `{ nodeLabels, edgeLabels, nodeCount, edgeCount }` derived from live graph.
- `graphTypes()` — list of `{ name, active, nodes?, edges? }` from the catalog.

### `execute()` return shapes

| Statement | Return |
|---|---|
| Query with `RETURN` | `Array<Record<string, JsonValue>>` keyed by alias (or `colN` if no alias) |
| Query without `RETURN` | `Array<{ _paths, ...vars }>` — `_paths` is the list of comma-join paths; vars are the pattern bindings |
| `CREATE / USE / DROP GRAPH TYPE` | `{ ok: true, kind: "ddl", message }` |
| `SHOW GRAPH TYPES` | `Array<{ name, active, nodes?, edges? }>` |
| `SHOW GRAPH TYPE <name>` | `{ name, active, nodes, edges, formatted, validation? }` |
| `SHOW CURRENT GRAPH TYPE` | `{ active, ... }` |
| `VALIDATE GRAPH TYPE` | `{ ok, name, nodesChecked, edgesChecked, nodeViolations, edgeViolations, samples }` |
| `CREATE INDEX` | `{ ok, kind: "index", name, label, prop, indexKind, entries }` |
| `DROP INDEX` | `{ ok, kind: "index", name, error? }` |
| `SHOW INDEXES` | `Array<{ name, label, prop, kind, auto, entries }>` |
| `INSERT / SET / REMOVE / DELETE / DETACH DELETE` | counters dict |

Node and edge references inside row values are dicts: `{ kind: "node"|"edge", id, labels, props? }`. The `props` field is included for `Value::Node`/`Value::Edge` (returned via `RETURN n`) but omitted for path-context edges to keep payloads small.

## Build from source

```bash
cd node
npm install
npm run build     # produces frogql.<platform>.node + index.js + index.d.ts
npm test          # node --test __test__/smoke.mjs
```

## Versioning

Tracks the PyPI `frogql` version (`0.2.0`). Same `gqlrust` core, same on-disk format, fully interoperable.
