// Smoke test: open the committed examples/movies.gdb, run a simple query,
// assert the row shape matches the documented contract.
// Run with: node --test __test__/smoke.mjs

import test from "node:test";
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const examplesDir = resolve(__dirname, "..", "..", "examples");
const moviesDb = resolve(examplesDir, "movies.gdb");

// Skip the whole suite if the binding hasn't been built yet. CI will fail
// at the build step before reaching here.
const { open, importJson, importCsv } = await import("../index.js");

test("movies.gdb opens and reports node/edge counts", () => {
  assert.ok(existsSync(moviesDb), `expected ${moviesDb} to exist`);
  const conn = open(moviesDb);
  assert.ok(typeof conn.nodeCount === "number" && conn.nodeCount > 0);
  assert.ok(typeof conn.edgeCount === "number" && conn.edgeCount > 0);
});

test("schema() returns label sets and counts", () => {
  const conn = open(moviesDb);
  const s = conn.schema();
  assert.ok(Array.isArray(s.nodeLabels));
  assert.ok(Array.isArray(s.edgeLabels));
  assert.equal(typeof s.nodeCount, "number");
  assert.equal(typeof s.edgeCount, "number");
});

test("execute() returns rows for a RETURN query", () => {
  const conn = open(moviesDb);
  const rows = conn.execute("MATCH (n) RETURN n LIMIT 3", 3);
  assert.ok(Array.isArray(rows));
  assert.ok(rows.length > 0 && rows.length <= 3);
  // First row should be a dict keyed by `n` containing a node reference.
  const first = rows[0];
  assert.equal(typeof first, "object");
  assert.ok("n" in first);
  assert.equal(first.n.kind, "node");
  assert.equal(typeof first.n.id, "number");
});

test("graphTypes() returns an array", () => {
  // examples/movies.gdb predates the catalog system, so the list is
  // empty. After `USE GRAPH TYPE DEFAULT` (which mutates the catalog)
  // it would contain DEFAULT — but that requires `save()` to persist
  // and we don't want the smoke test to touch the committed fixture.
  const conn = open(moviesDb);
  const types = conn.graphTypes();
  assert.ok(Array.isArray(types));
});

test("execute() runs a DDL SHOW statement", () => {
  const conn = open(moviesDb);
  const out = conn.execute("SHOW GRAPH TYPES");
  assert.ok(Array.isArray(out));
});

test("schema query: labels surface in node refs", () => {
  const conn = open(moviesDb);
  const rows = conn.execute(
    "MATCH (m:Movie) RETURN m.title AS title LIMIT 5",
    5
  );
  assert.ok(Array.isArray(rows));
  assert.ok(rows.length > 0);
  assert.equal(typeof rows[0].title, "string");
});

test("edges expose props in _paths and RETURN e (symmetric with nodes)", () => {
  const conn = open(moviesDb);
  // Top-level binding (no RETURN): row has `_paths` + every pattern var.
  const noReturn = conn.execute("MATCH ()-[e]->() LIMIT 1", 1);
  assert.ok(noReturn.length > 0);
  const row = noReturn[0];
  assert.ok("e" in row && "_paths" in row);
  assert.equal(row.e.kind, "edge");
  assert.ok(row.e.props && typeof row.e.props === "object");
  // The same edge inside _paths must also carry props.
  const pathEdge = row._paths[0].find((pv) => pv && pv.kind === "edge");
  assert.ok(pathEdge);
  assert.ok(pathEdge.props && typeof pathEdge.props === "object");

  // RETURN e: top-level Value::Edge route also includes props.
  const ret = conn.execute("MATCH ()-[e]->() RETURN e LIMIT 1", 1);
  assert.ok(ret.length > 0);
  assert.equal(ret[0].e.kind, "edge");
  assert.ok(ret[0].e.props && typeof ret[0].e.props === "object");
});

test("module exports import helpers", () => {
  assert.equal(typeof importJson, "function");
  assert.equal(typeof importCsv, "function");
});
