# frogql — Node.js bindings

Native Node.js addon for [froGQL](https://github.com/pleiad/frogql), an embedded GQL graph database with ISO GQL path patterns. Built via [napi-rs](https://napi.rs); ships per-platform prebuilds (Linux x64+arm64, macOS x64+arm64, Windows x64).

## Use case: VSCode extension

The Node addon runs in-process inside the VSCode extension host. Open a `.gdb` once, execute queries, get JS objects back. No subprocess, no IPC, no serialisation overhead.

## Use in another project

Until the package is published to npm, install from the local checkout. Three options.

### Option A: `file:` dependency (recommended for monorepos / VSCode extensions)

In your consumer's `package.json`:

```json
{
  "dependencies": {
    "frogql": "file:../path/to/gqlrust/node"
  }
}
```

Then `npm install`. The consumer picks up the `index.js`, `index.d.ts`, and the platform `.node` from the linked directory. Re-run `npm install` (or `npm run build` inside `node/`) when the binding changes — `file:` deps don't auto-rebuild.

### Option B: `npm link` (fast iteration)

```bash
# inside this repo:
cd node
npm run build
npm link

# inside the consumer project:
npm link frogql
```

`npm link` symlinks the package into the consumer's `node_modules/`. Edit, rebuild, and the consumer sees the new binary on next require — no reinstall needed.

### Option C: `npm pack` (sandboxed install, mirrors what npm publish would ship)

```bash
cd node
npm run build
npm pack            # produces frogql-0.2.0.tgz

# inside the consumer:
npm install /absolute/path/to/frogql-0.2.0.tgz
```

The `.tgz` includes only what's listed in `package.json`'s `files` array (`index.js`, `index.d.ts`) plus the platform `.node` binary built locally. Useful when you want to test exactly what consumers would get from the registry.

### VSCode extension specifics

Add `frogql` to the extension's `dependencies` (any of A/B/C). On `vsce package`, the `.vsix` bundles `node_modules/frogql/` including the platform `.node`. Caveats:

- **Desktop only.** Native `.node` files don't load in the web extension host (`vscode.dev`, github.dev, Codespaces web). For web targets, use a WASM build path instead.
- **Per-platform packaging.** A `.vsix` built on macOS-arm64 only carries the arm64 binary. To ship a single extension that runs on every OS, either publish per-platform `.vsix` (`vsce package --target darwin-arm64`, etc., per the [marketplace platform-specific extensions guide](https://code.visualstudio.com/api/working-with-extensions/publishing-extension#platformspecific-extensions)), or ship one `.vsix` per platform and gate by `os` / `cpu` in `engines`.

```ts
// extension.ts
import { open } from "frogql";

export function activate(ctx: vscode.ExtensionContext) {
  const conn = open(ctx.asAbsolutePath("examples/movies.gdb"));
  vscode.commands.registerCommand("frogql.runQuery", async () => {
    const q = await vscode.window.showInputBox({ prompt: "GQL query" });
    if (!q) return;
    const rows = conn.execute(q, 100);
    vscode.window.showInformationMessage(JSON.stringify(rows));
  });
}
```

### Once published to npm

```bash
npm install frogql
```

The registry serves prebuilt platform binaries via `optionalDependencies` (`frogql-darwin-arm64`, `frogql-linux-x64-gnu`, etc.). `npm install` picks the right one for the host. No build step on the consumer side.

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

Methods (all strongly typed in `index.d.ts`):

- `execute(query: string, limit?: number): unknown` — run any GQL statement. The return type is `unknown` because the shape depends on the statement; cast to the expected interface (see table). `limit` defaults to 100; ignored for DDL / DML.
- `save(): void` — persist the merged base + overlay back to the file the connection was opened from. Until you call this, DML mutations and `CREATE INDEX` declarations live only in memory.
- `schema(): SchemaSummary` — `{ nodeLabels, edgeLabels, nodeCount, edgeCount }` derived from live graph.
- `graphTypes(): GraphTypeSummary[]` — list of `{ name, active, nodes?, edges? }` from the catalog.

### `execute()` return shapes

Each row uses the named interface from `index.d.ts`. Cast at the call site:

```ts
import type { NodeRef, DdlOk, IndexSummary, DmCounters } from "frogql";

const rows  = conn.execute("MATCH (n) RETURN n LIMIT 10") as Array<{ n: NodeRef }>;
const ddl   = conn.execute("USE GRAPH TYPE DEFAULT") as DdlOk;
const idx   = conn.execute("SHOW INDEXES") as IndexSummary[];
const count = conn.execute("INSERT (:Tag {name: 'foo'})") as DmCounters;
```

| Statement | Cast target |
|---|---|
| Query with `RETURN` | `Array<Record<string, unknown>>` (alias-keyed; refine per-query as `Array<{ alias: T }>`) |
| Query without `RETURN` | `Array<{ _paths: unknown[]; [var: string]: unknown }>` |
| `CREATE / USE / DROP GRAPH TYPE` | `DdlOk` |
| `SHOW GRAPH TYPES` | `GraphTypeSummary[]` |
| `SHOW GRAPH TYPE <name>` / `SHOW CURRENT GRAPH TYPE` | object with `name`, `active`, `nodes`, `edges`, `formatted`, optional `validation` |
| `VALIDATE GRAPH TYPE` | `{ ok, name, nodesChecked, edgesChecked, nodeViolations, edgeViolations, samples }` |
| `CREATE / DROP INDEX` | `IndexResult` |
| `SHOW INDEXES` | `IndexSummary[]` |
| `INSERT / SET / REMOVE / DELETE / DETACH DELETE` | `DmCounters` |

Node and edge references inside row values use `NodeRef` / `EdgeRef`. The `props` field on `EdgeRef` is optional: present when returned via `RETURN e` (top-level), omitted in path context to keep payloads small.

## Build from source

```bash
cd node
npm install
npm run build       # produces frogql.<platform>.node + index.js + index.d.ts
npm test            # runtime smoke tests (node --test)
npm run typecheck   # tsc --noEmit against __test__/types.test.ts
```

## Versioning

Tracks the PyPI `frogql` version (`0.2.0`). Same `gqlrust` core, same on-disk format, fully interoperable.
