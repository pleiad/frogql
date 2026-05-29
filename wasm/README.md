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
