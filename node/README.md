# frogql

[![npm](https://img.shields.io/npm/v/frogql?color=blue)](https://www.npmjs.com/package/frogql)
[![PyPI](https://img.shields.io/pypi/v/frogql?label=pypi)](https://pypi.org/project/frogql/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Embedded GQL graph database for Node.js. ISO GQL path-pattern queries, single-file `.gdb` storage, Leapfrog-Triejoin runtime, ships as a native addon. Designed to drop into a VSCode extension, an Electron app, or any Node service that needs an in-process graph DB without spinning up Neo4j.

Rust core compiled to a native `.node` binary via [napi-rs](https://napi.rs). Sibling Python package on [PyPI](https://pypi.org/project/frogql/) shares the same engine + on-disk format.

## Install

```bash
npm install frogql
```

Prebuilt binaries for macOS x64 + arm64, Linux x64 + arm64 (glibc), Windows x64. npm picks the right one for the host at install time via `optionalDependencies` — no compilation step, no build toolchain needed.

## Quick start

```ts
import { open } from "frogql";

const conn = open("examples/movies.gdb");

// 1. Count nodes / edges
console.log(conn.nodeCount, conn.edgeCount);

// 2. Run a query — alias-keyed rows.
const rows = conn.execute(
  `MATCH (m:Movie {released: 1999})<-[:ACTED_IN]-(p:Person)
   RETURN p.name AS actor, m.title AS film`,
  20
) as Array<{ actor: string; film: string }>;

console.log(rows);
// [
//   { actor: "Keanu Reeves",      film: "The Matrix" },
//   { actor: "Carrie-Anne Moss",  film: "The Matrix" },
//   ...
// ]
```

## Why frogql

- **ISO/IEC 39075:2024 path patterns.** Real Graph Query Language, not Cypher-flavoured pseudocode. Union (`|`), concat, repetition (`{n,m}`), `OPTIONAL MATCH`, `EXISTS`, `WHERE`, aggregates, `ORDER BY ... LIMIT`. Type system with subtyping, label algebra, and structural row types.
- **Worst-case-optimal join.** Leapfrog Triejoin over six sorted orderings of the edge set. 14×–4000× faster than pairwise hash-join on shape-heavy LDBC queries (numbers in [`docs/internals/JOIN_STRATEGY_NOTES.md`](https://github.com/pleiad/frogql/blob/main/docs/internals/JOIN_STRATEGY_NOTES.md)).
- **Single-file storage** (`.gdb`). 4 KB pages, slotted layout, CSR adjacency, page-cached property store. One file moves with your app.
- **Embeddable.** No server, no daemon, no HTTP. Open a path, run queries, close. Lives in the same process as your extension / Electron app / CLI.
- **Secondary indexes.** Hash for equality, B-tree for ranges. Auto-built per `(label, property)` pair where values are unique within a label. Optional DDL: `CREATE INDEX ... ON :Label(prop)`.
- **Data modification.** ISO §13 `INSERT`, `SET`, `REMOVE`, `DELETE`, `DETACH DELETE`. Overlay-on-disk model: mutations live in RAM until you call `conn.save()`, SQLite-style.

## API

### `open(path: string): Connection`

Open or create a `.gdb`. Eagerly warms the LTJ TripleIndex so the first query runs at warm-cache speed.

### `importJson(dbPath: string, jsonPath: string): void`

Build a fresh `.gdb` from a JSON file shaped `{ nodes: [...], edges: [...] }`. Overwrites the destination.

### `importCsv(dbPath: string, csvDir: string): void`

Build a fresh `.gdb` from a directory of CSVs configured via `spanner_import_config.json`. Used by the LDBC ingest pipeline; see [Cloud Spanner import format docs](https://cloud.google.com/spanner/docs/import).

### `class Connection`

| Member | Returns | Notes |
|---|---|---|
| `nodeCount` | `number` | Live count (base + overlay). |
| `edgeCount` | `number` | Live count (base + overlay). |
| `execute(query, limit?)` | `unknown` | Polymorphic — see [Return shapes](#executequery-limit-return-shapes). |
| `save()` | `void` | Persist base + overlay to the file the connection was opened from. |
| `schema()` | `SchemaSummary` | Sorted node / edge label sets + counts. |
| `graphTypes()` | `GraphTypeSummary[]` | Catalog entries with active markers. |

### `execute(query, limit?)` return shapes

`execute` returns `unknown` because the shape depends on the statement kind. Cast at the call site:

```ts
import type {
  NodeRef, EdgeRef, DmCounters, DdlOk, IndexResult, IndexSummary,
  GraphTypeSummary, SchemaSummary,
} from "frogql";
```

| Statement | Cast target |
|---|---|
| Query with `RETURN` | `Array<Record<string, unknown>>` keyed by alias |
| Query without `RETURN` | `Array<{ _paths: unknown[]; [v: string]: unknown }>` |
| `CREATE / USE / DROP GRAPH TYPE` | `DdlOk` |
| `SHOW GRAPH TYPES` | `GraphTypeSummary[]` |
| `SHOW GRAPH TYPE <name>` | object with `name`, `active`, `nodes`, `edges`, `formatted`, optional `validation` |
| `CREATE / DROP INDEX` | `IndexResult` |
| `SHOW INDEXES` | `IndexSummary[]` |
| `INSERT / SET / REMOVE / DELETE / DETACH DELETE` | `DmCounters` |

Node and edge references inside row values use `NodeRef` / `EdgeRef`. Both shapes are symmetric — `kind`, `id`, `labels`, and `props` are always populated, whether the element arrives via `RETURN x` at the top level or inside a `_paths` entry. (Pre-`0.2.3` versions omitted `props` on edges in `_paths`; that gap is closed.)

## Examples

### Pattern matching with repetition

```ts
const friends = conn.execute(
  `MATCH (p:Person {id: 12345})~[:knows]~{1,2}(f:Person)
   WHERE p <> f
   RETURN f.firstName AS name, f.lastName AS surname
   LIMIT 20`,
  20
) as Array<{ name: string; surname: string }>;
```

Bounded repetition `{1,2}` unrolls to a worst-case-optimal join per length. See [perf series](https://github.com/pleiad/frogql/blob/main/docs/internals/implemented-optimizations.md) for the optimizer pipeline.

### Aggregation + ordering

```ts
const top = conn.execute(
  `MATCH (m:Movie)<-[:ACTED_IN]-(p:Person)
   RETURN m.title AS film, COUNT(p) AS cast
   GROUP BY m.title
   ORDER BY cast DESC, film ASC
   LIMIT 10`,
  10
) as Array<{ film: string; cast: number }>;
```

`ORDER BY ... LIMIT` drives through a btree-backed top-k path when an index exists on the sort key; otherwise pdqsort.

### Optional match

```ts
const rows = conn.execute(
  `MATCH (p:Person)
   OPTIONAL MATCH (p)-[:ACTED_IN]->(m:Movie)
   RETURN p.name AS actor, m.title AS film
   LIMIT 50`,
  50
) as Array<{ actor: string; film: string | null }>;
```

Left-outer join with bind-pushdown: per outer row, pin shared variables and pin-execute the inner. SQLite-style nested-loop. ~93× speedup vs the global-evaluate-and-join baseline on LDBC IS5.

### Incremental writes (insert / upsert / delete)

The full ISO §13 DML surface is reachable through `execute()` — there is no dedicated `connection.insert()` / `connection.delete()` method, the same way SQLite doesn't have one. Writes go in as GQL statements.

```ts
import type { DmCounters } from "frogql";

// Insert.
const a = conn.execute("INSERT (:Person {id: 'u-42', name: 'Alice', age: 30})") as DmCounters;
console.log(a.nodesInserted); // 1

// Update a property.
conn.execute("MATCH (p:Person {id: 'u-42'}) SET p.age = 31");

// Remove a property without dropping the node.
conn.execute("MATCH (p:Person {id: 'u-42'}) REMOVE p.age");

// Delete by id. DETACH first removes incident edges; NODETACH errors out
// if the node still has neighbours.
conn.execute("MATCH (p:Person {id: 'u-42'}) DETACH DELETE p");

// Mutations live in an in-memory overlay until you call save().
conn.save();
```

#### Upsert pattern

There is no `MERGE` yet (deferred per the ISO §13 MVP). Upsert is a two-step probe + branch:

```ts
function upsertPerson(conn: Connection, id: string, name: string) {
  const hits = conn.execute(
    `MATCH (p:Person {id: '${id}'}) RETURN p.id AS id LIMIT 1`,
    1
  ) as Array<{ id: string }>;
  if (hits.length === 0) {
    conn.execute(`INSERT (:Person {id: '${id}', name: '${name}'})`);
  } else {
    conn.execute(`MATCH (p:Person {id: '${id}'}) SET p.name = '${name}'`);
  }
}
```

A unique secondary index on `(Person, id)` makes the probe a point lookup:

```ts
conn.execute("CREATE INDEX ON :Person(id) USING HASH");
```

`execute()` takes a raw query string — there is no parameter-binding API. **Escape user input yourself** (`String#replace(/'/g, "\\'")` at minimum) or you'll have a query-injection vector.

#### Operational notes for sync / streaming workloads

- **Auto-commit is off.** Mutations stay in the RAM overlay until `conn.save()` writes a fresh `.gdb` atomically. A crashed process loses the unsaved overlay. Batch writes and save on a cadence (every N statements / every T seconds) rather than per-mutation.
- **LTJ cache invalidation per mutation.** Each successful DML drops the cached six-ordering TripleIndex; the next read rebuilds it (~670 ms on an LDBC SF0.1-shaped graph). Tight write-read loops pay this per mutation. If your workload is write-heavy with occasional reads, that's fine; if it interleaves writes and reads, batch reads after a batch of writes.
- **Transactions are per-statement.** A failed statement rolls back its own overlay delta. There's no multi-statement transaction boundary yet (deferred until WAL).
- **No `MERGE`, no multi-DML chains** (`MATCH … INSERT … SET …` in one statement). One DML op per `execute()` call.
- **`importJson` is not incremental.** It builds a fresh `.gdb` from scratch and overwrites the destination. Use `execute("INSERT …")` against an open `Connection` for incremental ingest.

### Schema introspection

```ts
const s = conn.schema();
// { nodeLabels: ['Movie', 'Person'], edgeLabels: ['ACTED_IN', ...], nodeCount: 171, edgeCount: 253 }

const types = conn.graphTypes();
// [{ name: 'DEFAULT', active: true, nodes: 2, edges: 6 }]
```

### Indexes

```ts
const r = conn.execute("CREATE INDEX ON :Person(name) USING HASH") as IndexResult;
const r2 = conn.execute("CREATE INDEX ON :Person(age) USING BTREE") as IndexResult;
const idx = conn.execute("SHOW INDEXES") as IndexSummary[];
// auto-built indexes appear here too (auto: true).
```

`HASH` accelerates equality lookups (`x.name = 'Alice'`); `BTREE` powers range scans (`x.age >= 18`) and ORDER BY top-k.

## Use in a VSCode extension

The Node addon runs in-process inside the extension host (which is Node.js). No subprocess, no IPC, no serialisation overhead.

```ts
import * as vscode from "vscode";
import { open } from "frogql";
import type { NodeRef } from "frogql";

export function activate(ctx: vscode.ExtensionContext) {
  const dbPath = ctx.asAbsolutePath("data/movies.gdb");
  const conn = open(dbPath);

  ctx.subscriptions.push(
    vscode.commands.registerCommand("frogql.queryActors", async () => {
      const film = await vscode.window.showInputBox({ prompt: "Movie title" });
      if (!film) return;
      const rows = conn.execute(
        `MATCH (m:Movie {title: '${film.replace(/'/g, "\\'")}'})<-[:ACTED_IN]-(p:Person)
         RETURN p.name AS actor`,
        50
      ) as Array<{ actor: string }>;
      vscode.window.showInformationMessage(rows.map(r => r.actor).join(", "));
    })
  );
}
```

Add `frogql` to `dependencies` in `package.json`. `vsce package` bundles the platform `.node` into the `.vsix`.

**Caveats**

- Desktop only. Native addons don't load in `vscode.dev` / github.dev / Codespaces web. Use a WASM build for web targets.
- Per-platform packaging. A `.vsix` carries one platform binary. To ship for every OS, run `vsce package --target darwin-arm64`, `--target win32-x64`, etc. and publish per-platform on the marketplace.

## Platforms

| OS | Arch | npm package |
|---|---|---|
| macOS | x64 | `frogql-darwin-x64` |
| macOS | arm64 (Apple Silicon) | `frogql-darwin-arm64` |
| Linux | x64 (glibc) | `frogql-linux-x64-gnu` |
| Linux | arm64 (glibc) | `frogql-linux-arm64-gnu` |
| Windows | x64 | `frogql-win32-x64-msvc` |

Other targets (musl Linux, FreeBSD, Windows arm64) aren't built today. If you need one, open an issue at [pleiad/frogql](https://github.com/pleiad/frogql/issues).

## Performance

Numbers from `bench/data/ldbc-sf0.1.gdb` (LDBC Social Network scale-factor 0.1: 327K nodes, 1.5M edges) on an Apple M-series laptop, lazy backend, three iterations, warm cache:

| Query | gqlrust (with indexes) | GraphQLite (Cypher + SQLite reference) | Speedup |
|---|---|---|---|
| LDBC IC2 (recent messages) | **8.7 ms** | 32.8 ms | 3.8× |
| LDBC IS5 (forum on creator) | varies, indexed lookup + bind-pushdown | — | — |

Open time on the same DB: ~570 ms warm (string table 80 ms + topology 70 ms + secondary index auto-build 420 ms + TripleIndex 670 ms — the last two are memory-only and rebuild each open).

For the full benchmark suite see [`docs/internals/JOIN_STRATEGY_NOTES.md`](https://github.com/pleiad/frogql/blob/main/docs/internals/JOIN_STRATEGY_NOTES.md) and [`bench/cross-system/`](https://github.com/pleiad/frogql/tree/main/bench/cross-system).

## Versioning

Released in lock-step with the PyPI `frogql` package: a single `v*` git tag fires both release workflows. Same Rust core, same `.gdb` format, fully interoperable.

Pre-releases (`0.2.0-rc.1`, `-rc.2`, …) publish to dist-tag `next` so `npm install frogql` keeps pointing at the latest stable. Install a pre-release explicitly with `npm install frogql@next`.

## Develop locally

```bash
git clone https://github.com/pleiad/frogql.git
cd frogql/node
npm install
npm run build       # produces frogql.<platform>.node + index.js + index.d.ts
npm test            # runtime smoke tests (node --test)
npm run typecheck   # tsc --noEmit against __test__/types.test.ts
```

Requires Node ≥ 16 and a stable Rust toolchain.

## License

MIT. See [LICENSE](LICENSE).

## Related

- [`frogql` on PyPI](https://pypi.org/project/frogql/) — Python bindings, same engine.
- [`pleiad/frogql`](https://github.com/pleiad/frogql) — main repository, Rust core, CLI (`frogql` binary modelled on `sqlite3`), benchmark suite, architecture docs.
