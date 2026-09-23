// Runs the published surface of `frogql-wasm` under Node, against a
// package built from this checkout.
//
// It exists because `cargo build --target wasm32-unknown-unknown`
// compiles code that cannot run there. `wasm32-unknown-unknown` has no
// clock in std: `Instant::now()` / `SystemTime::now()` panic with "time
// not implemented on this platform", and a wasm panic unwinds as
// `RuntimeError: unreachable`, so the call never returns. That is how
// 0.5.3 shipped an `open_json` that failed on *every* input, one-node
// graphs included — a diagnostic timer added for a `FROGQL_TRACE_OPEN`
// phase sat on the index-build path, which the browser backend takes.
// Compiling proved nothing; only running does.
//
// Keep this cheap and keep it about the platform, not the engine: the
// query semantics are covered by the Rust suites. What belongs here is
// anything std might not have in a browser.
const assert = require("node:assert");
const { open_json } = require("../../pkg-smoke/frogql_wasm.js");

const GRAPH = JSON.stringify({
  nodes: [{ id: "a", labels: ["Person"], props: { name: "Alice" } }],
  edges: [],
});

// The README's one-person example, which is what a first-time user runs.
const conn = open_json(GRAPH);
assert.strictEqual(conn.node_count, 1, "open_json must load the one node");

assert.deepStrictEqual(
  conn.execute("MATCH (n:Person) RETURN n.name AS name"),
  [{ name: "Alice" }],
  "a read query must project",
);

// DML through the overlay, then the count that proves it landed.
conn.execute("INSERT (b:Person {name: 'Bob'})");
assert.strictEqual(conn.node_count, 2, "INSERT must reach node_count");

// The clock. `DATE()` and `LOCAL_DATETIME()` reach the one std facility
// wasm32 does not have; they must answer, not panic.
const [{ d }] = conn.execute("MATCH (n:Person) RETURN DATE() AS d");
assert.match(d, /^\d{4}-\d{2}-\d{2}$/, `DATE() must return a date, got ${d}`);
const [{ dt }] = conn.execute("MATCH (n:Person) RETURN LOCAL_DATETIME() AS dt");
assert.match(dt, /^\d{4}-\d{2}-\d{2}T/, `LOCAL_DATETIME() must return one, got ${dt}`);

// The persistence story the README documents: to_json round-trips.
const restored = open_json(conn.to_json());
assert.strictEqual(restored.node_count, 2, "to_json must round-trip through open_json");

console.log("frogql-wasm smoke: ok");
