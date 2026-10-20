//! Bind-pushdown into a later MATCH clause (`Runtime::match_pushdown_pins`).
//!
//! A later clause used to run as if nothing were bound and be joined
//! afterwards, so `MATCH (r)-[:EN]->(c) MATCH p = ANY SHORTEST (c)~*(b)`
//! searched from every node. With the pushdown the search starts only from
//! the `c` values the first clause produced. These tests pin that the rows
//! are identical with and without it (`FROGQL_DISABLE_MATCH_PUSHDOWN`).

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// A 4×4 grid of streets `N` joined by undirected `R` edges, two `Reclamo`
/// nodes and two `Lugar` nodes hanging off streets by `EN` edges.
fn graph() -> MemoryGraphStore {
    let n = 4;
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let id = |i: usize, j: usize| format!("s{i}_{j}");
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
    for (k, (i, j)) in [(0, 1), (2, 3)].into_iter().enumerate() {
        nodes.push(format!(
            r#"{{"id":"r{k}","labels":["Reclamo"],"props":{{"k":{k}}}}}"#
        ));
        edges.push(edge(format!("r{k}"), id(i, j), "EN", "->"));
    }
    for (k, (i, j)) in [(3, 0), (1, 2)].into_iter().enumerate() {
        nodes.push(format!(
            r#"{{"id":"l{k}","labels":["Lugar"],"props":{{"k":{k}}}}}"#
        ));
        edges.push(edge(format!("l{k}"), id(i, j), "EN", "->"));
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

fn run(g: &MemoryGraphStore, q: &str, pushdown: bool) -> Vec<String> {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if pushdown {
        std::env::remove_var("FROGQL_DISABLE_MATCH_PUSHDOWN");
    } else {
        std::env::set_var("FROGQL_DISABLE_MATCH_PUSHDOWN", "1");
    }
    let query = compile_query(q).unwrap_or_else(|e| panic!("{q}: {e}"));
    let rows: Vec<Vec<Value>> = match Runtime::new(g).run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        _ => panic!("expected projected rows for {q}"),
    };
    std::env::remove_var("FROGQL_DISABLE_MATCH_PUSHDOWN");
    let mut out: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
    out.sort();
    out
}

/// Same rows either way, and not trivially empty.
fn assert_same(q: &str) {
    let g = graph();
    let on = run(&g, q, true);
    let off = run(&g, q, false);
    assert!(
        !off.is_empty(),
        "query returned no rows, test is vacuous: {q}"
    );
    assert_eq!(on, off, "pushdown changed the answer of {q}");
}

#[test]
fn shortest_from_a_pinned_start() {
    // The SOSAFE shape: the BFS fast path, start pinned by the first clause.
    assert_same(
        "MATCH (r:Reclamo)-[:EN]->(c:N) \
         MATCH p = ANY SHORTEST (c)~[:R]~*(b:N) \
         RETURN r.k, b.id, path_length(p)",
    );
}

#[test]
fn shortest_from_an_unlabelled_pinned_start() {
    // `(c)` alone is "every node" to the BFS; the pin must override that.
    assert_same(
        "MATCH (r:Reclamo)-[:EN]->(c) \
         MATCH p = ANY SHORTEST (c)~[:R]~*(b:N) \
         RETURN r.k, b.id, path_length(p)",
    );
}

#[test]
fn shortest_to_a_pinned_end() {
    assert_same(
        "MATCH (r:Reclamo)-[:EN]->(c:N) \
         MATCH p = ANY SHORTEST (b:N)~[:R]~*(c) \
         RETURN r.k, b.id, path_length(p)",
    );
}

#[test]
fn all_shortest_groups() {
    assert_same(
        "MATCH (r:Reclamo)-[:EN]->(c:N) \
         MATCH p = ALL SHORTEST (c)~[:R]~*(b:N) \
         RETURN r.k, b.id, path_length(p)",
    );
}

#[test]
fn shortest_with_a_tail_off_the_generic_path() {
    // Not the BFS shape: the generic k-shortest search runs.
    assert_same(
        "MATCH (r:Reclamo)-[:EN]->(c:N) \
         MATCH p = ANY SHORTEST (c)~[:R]~*(:N)<-[:EN]-(l:Lugar) \
         RETURN r.k, l.k, path_length(p)",
    );
}

#[test]
fn shortest_to_a_target_filtered_by_exists() {
    assert_same(
        "MATCH (r:Reclamo)-[:EN]->(c:N) \
         MATCH p = ANY SHORTEST (c)~[:R]~*(b:N) \
         WHERE EXISTS { MATCH (b)<-[:EN]-(:Lugar) } \
         RETURN r.k, MIN(path_length(p)) AS d GROUP BY r",
    );
}

#[test]
fn plain_clause_between_two() {
    // A non-selective middle clause is pinned too, and it pins the next.
    assert_same(
        "MATCH (r:Reclamo)-[:EN]->(c:N) \
         MATCH (c)~[:R]~(d:N) \
         MATCH p = ANY SHORTEST (d)~[:R]~*(e:N) \
         RETURN r.k, d.id, e.id, path_length(p)",
    );
}

#[test]
fn interior_variable_of_a_mode_only_prefix() {
    // TRAIL selects nothing, so an interior variable may be shared and pinned.
    assert_same(
        "MATCH (r:Reclamo)-[:EN]->(c:N) \
         MATCH p = TRAIL (a:N)~[:R]~(c)~[:R]~(b:N) \
         RETURN r.k, a.id, b.id",
    );
}

#[test]
fn a_variable_an_optional_left_unbound_is_not_pinned() {
    // `r` is null in the rows where the optional found nothing. The later
    // clause reuses `r`, so pinning it to the bound values alone would
    // drop whatever the join does with the null rows.
    assert_same(
        "MATCH (c:N) WHERE c.id < 4 \
         OPTIONAL MATCH (r:Reclamo)-[:EN]->(c) \
         MATCH p = ANY SHORTEST (c)~[:R]~*(b:N {id: 15}) \
         MATCH (r)-[:EN]->(x:N) \
         RETURN c.id, r.k, x.id, path_length(p)",
    );
}
