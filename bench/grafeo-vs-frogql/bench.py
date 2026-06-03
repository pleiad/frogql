#!/usr/bin/env python3
"""Head-to-head micro-benchmark: froGQL vs Grafeo, same data, same GQL queries.

Both engines expose a Python binding with an ISO-GQL `execute`, so we load the
*identical* graph into each, verify the two engines return the *same rows* for
every query, then time warm executions. Wall-clock is measured the same way for
both. This is a reproducible harness, not a marketing number: it prints the
versions, the dataset, and flags any query where results diverge or a feature is
unsupported.
"""
import gc
import json
import os
import random
import statistics
import tempfile
import time
from importlib import metadata

import frogql
import grafeo

ITERS = 9          # timed iterations per query (after 1 warmup)
WARMUP = 1
LIMIT = 10_000_000  # froGQL needs an explicit cap; keep it effectively unbounded


# ----------------------------- data generation -----------------------------
def gen_social(n_nodes, avg_deg, seed=42):
    """A directed Person-[:KNOWS]->Person graph with an `age` property."""
    rnd = random.Random(seed)
    nodes = [{"id": str(i), "labels": ["Person"],
              "props": {"name": f"p{i}", "age": i % 90}} for i in range(n_nodes)]
    # Dedup (s,t) pairs and skip self-loops so the *logical* graph is identical
    # for both engines: froGQL models edges as a set of (src,label,tgt) triples
    # (no parallel edges), Grafeo is a multigraph. Without dedup the row counts
    # legitimately differ and the comparison would be apples-to-oranges.
    pairs = set()
    for s in range(n_nodes):
        for _ in range(avg_deg):
            t = rnd.randrange(n_nodes)
            if t != s:
                pairs.add((s, t))
    edges = [{"id": f"e{i}", "labels": ["KNOWS"],
              "endpoints": [str(s), str(t)],
              "directionality": "->", "props": {}}
             for i, (s, t) in enumerate(sorted(pairs))]
    return {"nodes": nodes, "edges": edges}


def load_movies(path):
    with open(path) as f:
        return json.load(f)


# ----------------------------- engine adapters -----------------------------
class FrogqlEngine:
    name = "froGQL"

    def __init__(self, graph):
        self.dir = tempfile.mkdtemp()
        self.tmp = os.path.join(self.dir, "bench.gdb")  # must not pre-exist
        jpath = os.path.join(self.dir, "graph.json")
        with open(jpath, "w") as f:
            json.dump(graph, f)
        frogql.import_json(self.tmp, jpath)
        self.conn = frogql.open(self.tmp)  # warms the LTJ TripleIndex

    def counts(self):
        return self.conn.node_count, self.conn.edge_count

    def run(self, q):
        return self.conn.execute(q, LIMIT)

    def close(self):
        import shutil
        shutil.rmtree(self.dir, ignore_errors=True)


class GrafeoEngine:
    name = "Grafeo"

    def __init__(self, graph):
        self.db = grafeo.GrafeoDB.open_in_memory(":memory:")
        # nodes: one batch per label, preserving generator order -> id map
        by_label = {}
        order = []
        for nd in graph["nodes"]:
            lab = nd["labels"][0]
            by_label.setdefault(lab, []).append(nd)
            order.append(nd["id"])
        idmap = {}
        for lab, nds in by_label.items():
            ids = self.db.batch_create_nodes_with_props(lab, [n["props"] for n in nds])
            for n, gid in zip(nds, ids):
                idmap[n["id"]] = gid
        for e in graph["edges"]:
            s, t = e["endpoints"]
            self.db.create_edge(idmap[s], idmap[t], e["labels"][0], e.get("props", {}))

    def counts(self):
        nc = self.db.node_count() if callable(self.db.node_count) else self.db.node_count
        ec = self.db.edge_count() if callable(self.db.edge_count) else self.db.edge_count
        return nc, ec

    def run(self, q):
        return self.db.execute(q).to_list()

    def close(self):
        pass


# ----------------------------- comparison core -----------------------------
def normalize(rows, cols):
    """Turn list[dict] into a sorted multiset of value-tuples, column-order fixed.

    froGQL and Grafeo key rows by the RETURN alias, so aliased queries align.
    Scalars are stringified to dodge int/float formatting noise."""
    out = []
    for r in rows:
        out.append(tuple(str(r.get(c)) for c in cols))
    out.sort()
    return out


def time_query(engine, q):
    for _ in range(WARMUP):
        engine.run(q)
    samples = []
    for _ in range(ITERS):
        gc.disable()
        t0 = time.perf_counter()
        rows = engine.run(q)
        dt = (time.perf_counter() - t0) * 1000.0
        gc.enable()
        samples.append(dt)
    return statistics.median(samples), rows


# ----------------------------- query suite -----------------------------
# cols = the RETURN aliases, in order, used to align results across engines.
QUERIES = [
    ("label_scan",   "MATCH (p:Person) RETURN p.name AS name",                                          ["name"]),
    ("filter_attr",  "MATCH (p:Person) WHERE p.age > 80 RETURN p.name AS name",                          ["name"]),
    ("count_nodes",  "MATCH (p:Person) RETURN count(p) AS c",                                            ["c"]),
    ("one_hop_cnt",  "MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN count(b) AS c",                       ["c"]),
    ("two_hop_cnt",  "MATCH (a:Person)-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person) RETURN count(c) AS c",  ["c"]),
    ("three_path_cnt", "MATCH (a:Person)-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person)-[:KNOWS]->(d:Person) RETURN count(d) AS c", ["c"]),
    ("one_hop_rows", "MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN a.name AS a, b.name AS b",            ["a", "b"]),
]

# A directed triangle: a cyclic (closed) multi-way join. Reported separately
# because the two engines DISAGREE on the result, so timing it would compare
# different computations. See correctness_probe().
TRIANGLE = "MATCH (a:Person)-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person)-[:KNOWS]->(a:Person) RETURN count(a) AS c"


def correctness_probe():
    """Minimal reproducible check of the cyclic-join divergence.

    Graph: a single directed triangle 0->1->2->0, plus a dangling edge 2->3.
    The only directed triangle is (0,1,2); a homomorphic count yields its 3
    rotations. Any larger count includes a match whose closing edge is absent."""
    graph = {
        "nodes": [{"id": str(i), "labels": ["Person"], "props": {"name": f"p{i}"}} for i in range(4)],
        "edges": [{"id": f"e{i}", "labels": ["KNOWS"], "endpoints": [str(s), str(t)],
                   "directionality": "->", "props": {}}
                  for i, (s, t) in enumerate([(0, 1), (1, 2), (2, 0), (2, 3)])],
    }
    fe, ge = FrogqlEngine(graph), GrafeoEngine(graph)
    fc = fe.run(TRIANGLE)[0]["c"]
    gc = ge.run(TRIANGLE)[0]["c"]
    rows = "MATCH (a:Person)-[:KNOWS]->(b:Person)-[:KNOWS]->(c:Person)-[:KNOWS]->(a:Person) RETURN a.name AS a, b.name AS b, c.name AS c"
    fr = sorted(tuple(r.values()) for r in fe.run(rows))
    gr = sorted(tuple(r.values()) for r in ge.run(rows))
    print("\n" + "=" * 72)
    print("CORRECTNESS: directed triangle 0->1->2->0 (+ dangling 2->3)")
    print("=" * 72)
    print("  the only triangle is (0,1,2); correct homomorphic count = 3 rotations")
    print(f"  froGQL count = {fc}   rows = {fr}")
    print(f"  Grafeo count = {gc}   rows = {gr}")
    extra = [r for r in gr if r not in fr]
    if extra:
        print(f"  -> Grafeo returns {extra}, whose closing edge does not exist (spurious)")
    fe.close(); ge.close()


def run_dataset(title, graph):
    print(f"\n{'='*72}\nDATASET: {title}  ({len(graph['nodes'])} nodes, {len(graph['edges'])} edges)\n{'='*72}")
    engines = [FrogqlEngine(graph), GrafeoEngine(graph)]
    for e in engines:
        print(f"  {e.name:8} loaded -> nodes={e.counts()[0]} edges={e.counts()[1]}")

    hdr = f"  {'query':14} {'froGQL ms':>11} {'Grafeo ms':>11} {'ratio':>8} {'rows':>8}  match"
    print("\n" + hdr + "\n  " + "-" * (len(hdr) - 2))
    results = []
    for qname, q, cols in QUERIES:
        cell = {}
        rowsets = {}
        for e in engines:
            try:
                ms, rows = time_query(e, q)
                cell[e.name] = ms
                rowsets[e.name] = normalize(rows, cols)
            except Exception as ex:
                cell[e.name] = None
                rowsets[e.name] = ("ERR", str(ex)[:60])
        f_ms, g_ms = cell.get("froGQL"), cell.get("Grafeo")
        # correctness
        rf, rg = rowsets["froGQL"], rowsets["Grafeo"]
        if isinstance(rf, tuple) and rf and rf[0] == "ERR":
            match = f"froGQL✗ {rf[1]}"
            nrows = "-"
        elif isinstance(rg, tuple) and rg and rg[0] == "ERR":
            match = f"Grafeo✗ {rg[1]}"
            nrows = len(rf)
        else:
            match = "yes" if rf == rg else f"NO (f={len(rf)} g={len(rg)})"
            nrows = len(rf)
        ratio = (f"{g_ms / f_ms:.2f}x" if (f_ms and g_ms) else "-")
        fs = f"{f_ms:11.3f}" if f_ms is not None else f"{'n/a':>11}"
        gs = f"{g_ms:11.3f}" if g_ms is not None else f"{'n/a':>11}"
        print(f"  {qname:14} {fs} {gs} {ratio:>8} {str(nrows):>8}  {match}")
        results.append((qname, f_ms, g_ms, ratio, nrows, match))
    for e in engines:
        e.close()
    return results


def main():
    print("froGQL", metadata.version("frogql"), "  vs   Grafeo", metadata.version("grafeo"))
    print(f"iters={ITERS} (median, +{WARMUP} warmup)")
    datasets = []
    here = os.path.dirname(os.path.abspath(__file__))
    movies = os.path.join(here, "..", "..", "test_data", "movies.json")
    if os.path.exists(movies):
        datasets.append(("movies", load_movies(movies)))
    datasets.append(("synthetic-2k", gen_social(2_000, 8)))
    datasets.append(("synthetic-10k", gen_social(10_000, 8)))
    for title, g in datasets:
        run_dataset(title, g)
    correctness_probe()


if __name__ == "__main__":
    main()
