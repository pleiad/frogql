# frogql-wasm

froGQL compiled to WebAssembly: an in-browser, in-RAM graph engine.

Wraps the `MemoryGraphStore` backend plus the shared compiler/runtime and
exposes a `Connection` to JavaScript via `wasm-bindgen`. There is no
filesystem in the browser, so the binding works entirely with the JSON
shape `MemoryGraphStore` understands.

## Install & use (from any app)

Published to npm as **`frogql-wasm`** (unscoped). The package is built with
the wasm-pack `web` target, so it works in Vite / Rollup / esbuild / plain
browsers with no extra bundler plugin — call `await init()` once, then use
the API.

```bash
npm install frogql-wasm
```

```js
import init, { open_json } from "frogql-wasm";

await init();                              // load the .wasm (once, up front)

const conn = open_json(JSON.stringify({
  nodes: [{ id: "a", labels: ["Person"], props: { name: "Alice" } }],
  edges: [],
}));

conn.execute("MATCH (n:Person) RETURN n.name AS name");  // [{ name: "Alice" }]
conn.execute("INSERT (b:Person {name: 'Bob'})");          // { nodes_inserted: 1, ... }
conn.node_count;                                          // 2

const snapshot = conn.to_json();           // persist this string (e.g. IndexedDB)
const restored = open_json(snapshot);      // reload later
```

Verified: a fresh project that `npm install`s the package and runs
`vite build` bundles the `.wasm` (≈300 kB gzip) with no plugin.

- `open_json(json)` → `Connection`. Parses `{ "nodes": [...], "edges": [...] }`.
- `open_bytes(gdb, ltj?)` → `Connection`. Opens a real `.gdb` image fetched
  over the network, paged out of RAM. `ltj` is the matching
  `<db>.gdb.ltj` sidecar, or `null` to rebuild the index at open.

  ```js
  const [gdb, ltj] = await Promise.all([
    fetch("/santiago.gdb").then(r => r.arrayBuffer()),
    fetch("/santiago.gdb.ltj").then(r => r.arrayBuffer()),
  ]);
  const conn = open_bytes(new Uint8Array(gdb), new Uint8Array(ltj));
  ```

  Prefer it over `open_json` for anything large: a JSON document is parsed
  and rebuilt node by node and carries no index, while a `.gdb` is already
  in the engine's layout and its catalog supplies the schema instead of
  it being re-inferred. Measured on a 459 127-node / 2 244 154-edge graph
  (309 MB `.gdb`): `open_bytes` in 3.7 s, first query 13 ms.

  **Whether to fetch the sidecar depends on size.** It exists so the six
  LTJ trie orderings are not rebuilt, which is `O(E log E)` — 252 s
  measured on a 617 M-edge graph. But decoding it is not free either, and
  on the 2.2 M-edge graph above the 70 MB sidecar *cost* ~0.8 s against
  rebuilding in ~1.7 s. Fetch it for a graph big enough that the rebuild
  hurts; skip it otherwise and save the download. A sidecar that does not
  describe the database is refused and the index rebuilt, so a mismatched
  pair costs time and never correctness.

  Read-only as to storage: DML works through the same overlay as every
  other backend, but there is nowhere to write pages back to, so the
  durable copy stays whatever the server serves. `to_json()` gives a
  snapshot of the merged view. Two things a file gives that bytes do not,
  and which are skipped rather than faked: the legacy-format upgrade
  (re-save such a database with a native build before serving it) and
  vector sidecars.
- `Connection.execute(query, limit?)` → rows array (read queries) or a
  counters object (INSERT / SET / REMOVE / DELETE). `limit` defaults to 100.
- `Connection.to_json()` → JSON string of the live merged view (base +
  any mutations). Round-trips through `open_json`.
- `Connection.schema()` → `{ node_labels, edge_labels, node_count, edge_count }`.
- `Connection.node_count` / `edge_count` (getters).

Not supported in this backend (no catalog / secondary index in memory):
`CREATE/USE/DROP GRAPH TYPE`, `CREATE INDEX`. Queries typecheck against the
inferred DEFAULT schema.

## Build

```bash
# one-time toolchain
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.122   # match the wasm-bindgen crate

# generate the JS package (web target)
cargo build -p frogql-wasm --target wasm32-unknown-unknown --release
wasm-bindgen target/wasm32-unknown-unknown/release/frogql_wasm.wasm \
  --out-dir <out> --target web
```

`wasm-pack build wasm --target web` (after `cargo install wasm-pack`) is the
all-in-one alternative and additionally runs `wasm-opt` to shrink the binary
(~855 kB vs ~1 MB). This is exactly what the npm release job
(`.github/workflows/release-wasm.yml`) runs to produce the published
`frogql-wasm` package.

The playground frontend wires this up: `playground/frontend/scripts/build-wasm.sh`
(run via `npm run build:wasm`) regenerates the bindings into
`playground/frontend/src/frogql-wasm/` (gitignored), which the React app imports.

### Marshaling note

Results are returned via `serde_wasm_bindgen::Serializer::json_compatible()`,
not `serde_wasm_bindgen::to_value`. The default serializes `serde_json` maps as
JS `Map` objects, which surface as empty `{}` under `JSON.stringify` / bracket
access. `json_compatible()` serializes them as plain objects.

## Test

The engine core (`query_json` / `dm_json`) is unit-tested on the host
target — no browser needed:

```bash
cargo test -p frogql-wasm
```

The JavaScript marshaling layer on top (`JsValue` conversion) only runs in
a JS environment; exercise it with `wasm-pack test --headless --firefox`
once `wasm-pack` is installed.

## Persistence

Phase 1 persists the graph as the `to_json()` string in IndexedDB.
Persisting the binary `.gdb` format (and on-demand paging for graphs that
do not fit in RAM) is Phase 2 — it requires abstracting the `Pager` off
`std::fs::File` onto OPFS. See `docs/internals/wasm-browser-plan.md`.
