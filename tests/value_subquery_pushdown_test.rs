//! A materialised `VALUE { ... }` body restricted to the rows being
//! projected (`Runtime::value_body_pins`).
//!
//! A body the per-row pinned regime refuses — a shortest search, an
//! aggregate — used to run once over the whole graph, so `MATCH (c) WHERE
//! EXISTS {...} RETURN VALUE { MATCH p = ANY SHORTEST (c)~*... }` searched
//! from every node to answer the few the filter kept. These tests pin that
//! the rows are identical with and without the restriction
//! (`FROGQL_DISABLE_VALUE_SUBQUERY_PIN`).

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// A 4×4 grid of streets `N` (ids 0..16) joined by undirected `R` edges,
/// plus street 16 with no `R` edge at all. `Reclamo` nodes hang off
/// streets 1, 11 and 16; `Lugar` nodes off streets 12 and 6.
fn graph() -> MemoryGraphStore {
    let n = 4;
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let id = |i: usize, j: usize| format!("s{}", i * n + j);
    let mut e = 0;
    let mut edge = |a: String, b: String, label: &str, dir: &str| {
        e += 1;
        format!(
            r#"{{"id":"e{e}","labels":["{label}"],"props":{{}},"endpoints":["{a}","{b}"],"directionality":"{dir}"}}"#
        )
    };
    for i in 0..n {
        for j in 0..n {
            nodes.push(format!(
                r#"{{"id":"{}","labels":["N"],"props":{{"id":{}}}}}"#,
                id(i, j),
                i * n + j
            ));
            if i + 1 < n {
                edges.push(edge(id(i, j), id(i + 1, j), "R", "~~"));
            }
            if j + 1 < n {
                edges.push(edge(id(i, j), id(i, j + 1), "R", "~~"));
            }
        }
    }
    nodes.push(r#"{"id":"s16","labels":["N"],"props":{"id":16}}"#.to_string());
    for (k, s) in [1, 11, 16].into_iter().enumerate() {
        nodes.push(format!(
            r#"{{"id":"r{k}","labels":["Reclamo"],"props":{{"k":{k}}}}}"#
        ));
        edges.push(edge(format!("r{k}"), format!("s{s}"), "EN", "->"));
    }
    for (k, s) in [12, 6].into_iter().enumerate() {
        nodes.push(format!(
            r#"{{"id":"l{k}","labels":["Lugar"],"props":{{"k":{k}}}}}"#
        ));
        edges.push(edge(format!("l{k}"), format!("s{s}"), "EN", "->"));
    }
    let json = format!(
        r#"{{"nodes":[{}],"edges":[{}]}}"#,
        nodes.join(","),
        edges.join(",")
    );
    MemoryGraphStore::from_json_str(&json).unwrap()
}

/// The toggle is process-global, so the tests serialise on it.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run(g: &MemoryGraphStore, q: &str, restricted: bool) -> Vec<String> {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if restricted {
        std::env::remove_var("FROGQL_DISABLE_VALUE_SUBQUERY_PIN");
    } else {
        std::env::set_var("FROGQL_DISABLE_VALUE_SUBQUERY_PIN", "1");
    }
    let query = compile_query(q).unwrap_or_else(|e| panic!("{q}: {e}"));
    let rows: Vec<Vec<Value>> = match Runtime::new(g).run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected rows for {q}"),
    };
    std::env::remove_var("FROGQL_DISABLE_VALUE_SUBQUERY_PIN");
    let mut out: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
    out.sort();
    out
}

/// Same rows either way, and not trivially empty.
fn assert_same(q: &str) -> Vec<String> {
    let g = graph();
    let on = run(&g, q, true);
    let off = run(&g, q, false);
    assert!(
        !off.is_empty(),
        "query returned no rows, test is vacuous: {q}"
    );
    assert_eq!(on, off, "the restriction changed the answer of {q}");
    on
}

#[test]
fn sosafe_shape_exists_filter_then_value_shortest() {
    let rows = assert_same(
        "MATCH (c:N) \
         WHERE EXISTS { MATCH (r:Reclamo)-[:EN]->(c) } \
         RETURN c.id, \
                VALUE { MATCH p = ANY SHORTEST (c)~[:R]~*(:N)<-[:EN]-(l:Lugar) \
                        RETURN MIN(path_length(p)) } AS largo",
    );
    // Streets 1, 11 and 16; 16 reaches no Lugar and must come back null.
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(rows.iter().any(|r| r.contains("Int(16), Null")), "{rows:?}");
}

#[test]
fn an_empty_bucket_inside_the_restriction_is_the_empty_aggregate() {
    // COUNT over no rows is 0, not null: the covered-but-empty bucket.
    let rows = assert_same(
        "MATCH (r:Reclamo)-[:EN]->(c:N) \
         RETURN r.k, \
                VALUE { MATCH p = ANY SHORTEST (c)~[:R]~*(b:N {id: 15}) \
                        RETURN COUNT(b) } AS n",
    );
    assert!(rows.iter().any(|r| r.contains("Int(0)")), "{rows:?}");
}

#[test]
fn grouped_projection() {
    // The hint is recorded on the aggregated path too.
    assert_same(
        "MATCH (r:Reclamo)-[:EN]->(c:N) \
         RETURN c.id, COUNT(r) AS n, \
                VALUE { MATCH p = ANY SHORTEST (c)~[:R]~*(:N)<-[:EN]-(:Lugar) \
                        RETURN MIN(path_length(p)) } AS largo \
         GROUP BY c",
    );
}

#[test]
fn two_correlation_variables() {
    // The restriction is per variable, so it covers pairs no row holds;
    // those buckets are computed and never probed.
    assert_same(
        "MATCH (r:Reclamo)-[:EN]->(c:N), (l:Lugar)-[:EN]->(d:N) \
         RETURN r.k, l.k, \
                VALUE { MATCH p = ANY SHORTEST (c)~[:R]~*(d) \
                        RETURN path_length(p) } AS largo",
    );
}

#[test]
fn value_in_where_and_in_return() {
    // The WHERE copy is probed with no hint and runs unrestricted; the
    // RETURN copy is a different body and runs restricted.
    assert_same(
        "MATCH (c:N) \
         WHERE VALUE { MATCH p = ANY SHORTEST (c)~[:R]~*(:N)<-[:EN]-(:Lugar) \
                       RETURN MIN(path_length(p)) } < 3 \
         RETURN c.id, \
                VALUE { MATCH p = ANY SHORTEST (c)~[:R]~*(:N)<-[:EN]-(:Lugar) \
                        RETURN MIN(path_length(p)) } AS largo",
    );
}

#[test]
fn ltj_pinnable_body_is_unchanged() {
    // A plain arg-max over a directed edge takes the per-row pinned
    // regime, which the restriction does not touch.
    assert_same(
        "MATCH (c:N) \
         RETURN c.id, \
                VALUE { MATCH (c)<-[:EN]-(x) RETURN x.k AS k ORDER BY k DESC LIMIT 1 } AS top",
    );
}
