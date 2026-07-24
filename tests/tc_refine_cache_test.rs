//! Differential suite for the typechecker's refine memo cache
//! (`Schema::refine_cache`): with the cache enabled (default) and disabled
//! (`GQLITE_DISABLE_TC_REFINE_CACHE=1`), `check_query` must produce
//! identical verdicts, errors, and warnings — cold and warm.
//!
//! Kept as a single `#[test]` because the kill switch is a process-global
//! env var and cargo runs tests in threads.

use std::path::Path;

use frogql::store::lazy::LazyGraphStore;
use frogql::typing::checker::Typechecker;
use frogql::typing::variable_type::Schema;

type Verdict = (bool, bool, Vec<String>, Vec<String>);

fn verdict(schema: &Schema, q: &str) -> Verdict {
    let parsed = frogql::parser::parse_query(q).expect("parse");
    let elab = frogql::elaborate::elaborate_query(parsed);
    let mut tc = Typechecker::new(schema.clone());
    let r = tc.check_query(&elab);
    (r.ok, r.empty, tc.errors.clone(), tc.warnings.clone())
}

const QUERIES: &[&str] = &[
    // Unlabeled / star shapes (wide refinements, exercise Union results).
    "(a)",
    "()-[]->()",
    "()-[]->()-[]->()",
    "(a)-[e]-(b)",
    "(a)~[]~(b)~[]~(c)",
    // Labeled shapes — valid or empty depending on the schema; either way
    // both cache modes must agree.
    "(p: Person)",
    "(p: Person)-[]->(m: Movie)",
    "(p: Person)-[:ACTED_IN]->(m: Movie)",
    "(x: NoSuchLabel)",
    "(a: Person)-[:NoSuchEdge]->(b: Person)",
    // Repeats, unions, filters.
    "(a)-[]->{1,3}(b)",
    "(p: Person)-[]->{2,4}(m: Movie)",
    "(a: Person) | (a: Movie)",
    "(p: Person WHERE p.name = 'Keanu Reeves')",
    "(p WHERE p.name = 'x')-[]->(q WHERE q.title = 'y')",
    // Full-query surface: RETURN + subqueries + OPTIONAL.
    "MATCH (a)-[]->(b) RETURN a",
    "MATCH (a) WHERE EXISTS { MATCH (a)-[]->(b) } RETURN a",
    "MATCH (a)-[]->(b) OPTIONAL MATCH (b)-[]->(c) RETURN a",
];

fn assert_cache_transparent(schema: &Schema, label: &str) {
    for q in QUERIES {
        // Cold + warm with both caches on (second run hits the memos).
        std::env::remove_var("GQLITE_DISABLE_TC_REFINE_CACHE");
        std::env::remove_var("GQLITE_DISABLE_TC_JUNCTION_CACHE");
        let cold = verdict(schema, q);
        let warm = verdict(schema, q);
        // Each cache off individually, then both off.
        std::env::set_var("GQLITE_DISABLE_TC_REFINE_CACHE", "1");
        let refine_off = verdict(schema, q);
        std::env::remove_var("GQLITE_DISABLE_TC_REFINE_CACHE");
        std::env::set_var("GQLITE_DISABLE_TC_JUNCTION_CACHE", "1");
        let junction_off = verdict(schema, q);
        std::env::set_var("GQLITE_DISABLE_TC_REFINE_CACHE", "1");
        let both_off = verdict(schema, q);
        std::env::remove_var("GQLITE_DISABLE_TC_REFINE_CACHE");
        std::env::remove_var("GQLITE_DISABLE_TC_JUNCTION_CACHE");
        assert_eq!(
            cold, both_off,
            "[{label}] caches-on (cold) != caches-off for: {q}"
        );
        assert_eq!(
            warm, both_off,
            "[{label}] caches-on (warm) != caches-off for: {q}"
        );
        assert_eq!(
            refine_off, both_off,
            "[{label}] refine-off != both-off for: {q}"
        );
        assert_eq!(
            junction_off, both_off,
            "[{label}] junction-off != both-off for: {q}"
        );
    }
}

#[test]
fn refine_cache_is_transparent() {
    // Star schema: everything satisfiable, exercises the Node/Edge arms
    // with permissive entries.
    assert_cache_transparent(&Schema::star(), "star");

    // A data-derived schema from a committed example DB: multiple node and
    // edge entries, so refinements produce real unions and real empties.
    let lazy =
        LazyGraphStore::open(Path::new("examples/movies.gdb")).expect("open examples/movies.gdb");
    let schema = lazy.catalog().active_schema();
    assert!(
        !schema.nodes.is_empty() && !schema.edges.is_empty(),
        "movies.gdb should yield a non-trivial inferred schema"
    );
    assert_cache_transparent(&schema, "movies");
}
