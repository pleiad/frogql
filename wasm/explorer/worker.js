// The engine, off the UI thread.
//
// froGQL runs synchronously: `open_bytes` on a 309 MB image is seconds of
// straight-line work, and `execute` on a bad pattern can be more. On the
// UI thread that is a frozen tab — no spinner, no cancel, no scroll —
// because nothing can paint while wasm holds the thread. A worker has no
// UI to block, so the page stays responsive and can say what it is doing.
//
// The buffers live here rather than in the page. A file is posted once
// and kept, so picking the `.ltj` after the `.gdb` re-opens without
// shipping 300 MB across again.
import init, { open_bytes } from "./pkg/frogql_wasm.js";

let ready = init();
let gdb = null, ltj = null, conn = null;

const send = (m) => postMessage(m);
const fail = (e) => send({ ev: "error", msg: String((e && e.message) || e) });

onmessage = async (ev) => {
  const m = ev.data;
  try {
    await ready;
    if (m.op === "file") {
      if (m.which === "gdb") gdb = new Uint8Array(m.bytes);
      else ltj = new Uint8Array(m.bytes);
      if (gdb) open();
    } else if (m.op === "query") {
      query(m.q, m.limit);
    }
  } catch (e) { fail(e); }
};

function open() {
  send({ ev: "status", text: "abriendo…" });
  const t = performance.now();
  try {
    conn = open_bytes(gdb, ltj);
  } catch (e) { return fail(e); }
  const s = conn.schema();
  send({
    ev: "opened",
    ms: Math.round(performance.now() - t),
    node_count: conn.node_count,
    edge_count: conn.edge_count,
    node_labels: s.node_labels,
    edge_labels: s.edge_labels,
    // Both renderings of the active GRAPH TYPE, sent once at open: the
    // text the REPL's `.schema` prints, and the same thing as data so
    // the page can draw it. Neither is large and both are wanted the
    // moment a database is open.
    graph_type: conn.graph_type(),
    graph_json: conn.graph_type_json(),
  });
}

function query(q, limit) {
  if (!conn) return fail("no hay base abierta");
  send({ ev: "status", text: "ejecutando…" });
  const t = performance.now();
  let rows;
  try {
    rows = conn.execute(q, limit);
  } catch (e) { return fail(e); }
  send({ ev: "rows", rows, ms: Math.round(performance.now() - t) });
}
