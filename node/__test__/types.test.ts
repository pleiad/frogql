// Type-level smoke. Compiled with `tsc --noEmit` via `npm run typecheck`.
// Asserts the public API exports and the documented cast patterns
// type-check under strict mode.

import {
  open,
  importJson,
  importCsv,
  Connection,
} from "../index.js";

import type {
  SchemaSummary,
  GraphTypeSummary,
  NodeRef,
  EdgeRef,
  DmCounters,
  DdlOk,
  IndexResult,
  IndexSummary,
} from "../index.js";

// --- module functions ------------------------------------------------------

const _open: (p: string) => Connection = open;
const _importJson: (db: string, json: string) => void = importJson;
const _importCsv: (db: string, dir: string) => void = importCsv;

// --- Connection getters & methods ------------------------------------------

declare const conn: Connection;
const _nodeCount: number = conn.nodeCount;
const _edgeCount: number = conn.edgeCount;
const _save: () => void = () => conn.save();

// schema() is strongly typed.
const sch: SchemaSummary = conn.schema();
const _nl: string[] = sch.nodeLabels;
const _el: string[] = sch.edgeLabels;
const _nc: number = sch.nodeCount;
const _ec: number = sch.edgeCount;

// graphTypes() returns an array of typed entries.
const gts: GraphTypeSummary[] = conn.graphTypes();
gts.forEach((g) => {
  const _name: string = g.name;
  const _active: boolean = g.active;
  const _nodes: number | undefined = g.nodes;
  const _edges: number | undefined = g.edges;
});

// --- execute(): polymorphic, cast to the documented shape ------------------

// Projected query: Array<Record<string, unknown>>.
type ProjectedRow = Record<string, unknown>;
const _rows1 = conn.execute("MATCH (n:Movie) RETURN n.title AS title", 10) as ProjectedRow[];
const _title = _rows1[0]?.title as string | undefined;

// Query returning a node ref.
const _rows2 = conn.execute("MATCH (n) RETURN n LIMIT 5", 5) as Array<{ n: NodeRef }>;
const _firstNode: NodeRef | undefined = _rows2[0]?.n;
if (_firstNode) {
  const _kind: string = _firstNode.kind;
  const _id: number = _firstNode.id;
  const _labels: string[] = _firstNode.labels;
  const _props: unknown = _firstNode.props;
}

// Query returning an edge ref.
const _rows3 = conn.execute("MATCH ()-[e]-() RETURN e LIMIT 1", 1) as Array<{ e: EdgeRef }>;
const _e: EdgeRef | undefined = _rows3[0]?.e;
if (_e) {
  const _maybeProps: unknown = _e.props; // optional
}

// DDL.
const _ddl = conn.execute("USE GRAPH TYPE DEFAULT") as DdlOk;
const _ok: boolean = _ddl.ok;
const _msg: string = _ddl.message;

// SHOW INDEXES.
const _idx = conn.execute("SHOW INDEXES") as IndexSummary[];
_idx.forEach((i) => {
  const _: string = i.name;
});

// CREATE INDEX.
const _ci = conn.execute(
  "CREATE INDEX ON :Movie(title)"
) as IndexResult;
const _ciOk: boolean = _ci.ok;

// DML counters.
const _dml = conn.execute("INSERT (:Tag {name: 'foo'})") as DmCounters;
const _ni: number = _dml.nodesInserted;

console.log("type-check ok");
