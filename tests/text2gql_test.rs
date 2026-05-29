//! Integration tests using the Text2GQL Movies dataset.
//! Tests MATCH/WHERE/RETURN queries on a real property graph
//! with labels (Person, Movie) and typed properties.

use std::path::Path;

use gqlrust::model::csv_loader;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

fn movies_graph() -> MemoryGraphStore {
    // Load directly from CSV files (no JSON conversion needed)
    let csv_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("bench/data/text2gql/dataset/dataset/train/movies/gql/Spanner_Instance");
    if csv_dir.exists() {
        csv_loader::load_from_csv_dir(&csv_dir).unwrap()
    } else {
        // Fallback to JSON for CI/environments without the dataset
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_data/movies.json");
        MemoryGraphStore::from_file(&p).unwrap()
    }
}

fn run_query(g: &MemoryGraphStore, q: &str) -> QueryResult {
    let rt = Runtime::new(g);
    let query = gqlrust::compile_query(q).unwrap();
    rt.run_query(&query, 0)
}

fn projected_rows(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    match run_query(g, q) {
        QueryResult::Projected(rows) => rows,
        QueryResult::Raw(_) => panic!("expected Projected result"),
    }
}

fn row_count(g: &MemoryGraphStore, q: &str) -> usize {
    run_query(g, q).row_count()
}

// ==================== CSV loader tests ====================

#[test]
fn test_csv_loader_node_types() {
    let g = movies_graph();
    let rt = Runtime::new(&g);
    // Verify votes is Int, not String
    let q = gqlrust::compile_query("MATCH (m: Movie) WHERE m.votes > 0 RETURN m.votes").unwrap();
    let result = rt.run_query(&q, 1);
    match result {
        QueryResult::Projected(rows) => {
            assert!(!rows.is_empty());
            assert!(
                matches!(rows[0][0], Value::Int(_)),
                "votes should be Int, got {:?}",
                rows[0][0]
            );
        }
        _ => panic!("expected Projected"),
    }
}

#[test]
fn test_csv_loader_string_props() {
    let g = movies_graph();
    let rt = Runtime::new(&g);
    let q = gqlrust::compile_query("MATCH (p: Person) WHERE p.name = 'Keanu Reeves' RETURN p.born")
        .unwrap();
    let result = rt.run_query(&q, 0);
    match result {
        QueryResult::Projected(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][0], Value::Int(1964));
        }
        _ => panic!("expected Projected"),
    }
}

#[test]
fn test_csv_loader_edge_labels() {
    let g = movies_graph();
    // Verify edge labels are correctly parsed from filenames
    use gqlrust::model::graph_access::GraphAccess;
    let acted_in = g.directed_edges_with_label("ACTED_IN");
    assert!(acted_in.is_some());
    assert_eq!(acted_in.unwrap().len(), 172);
}

// ==================== MemoryGraphStore structure tests ====================

#[test]
fn test_movies_graph_loads() {
    let g = movies_graph();
    assert_eq!(g.node_count(), 171);
    assert_eq!(g.edge_count(), 253);
}

#[test]
fn test_movies_node_labels() {
    let g = movies_graph();
    // 38 Movie nodes + 133 Person nodes = 171
    let movies = row_count(&g, "MATCH (m: Movie) RETURN m.title");
    let persons = row_count(&g, "MATCH (p: Person) RETURN p.name");
    assert_eq!(movies, 38);
    assert_eq!(persons, 133);
    assert_eq!(movies + persons, g.node_count());
}

// ==================== Simple traversal queries ====================

#[test]
fn test_acted_in_traversal() {
    let g = movies_graph();
    // All ACTED_IN edges: 172
    let count = row_count(
        &g,
        "MATCH (p: Person) -[:ACTED_IN]-> (m: Movie) RETURN p.name, m.title",
    );
    assert_eq!(count, 172);
}

#[test]
fn test_directed_traversal() {
    let g = movies_graph();
    let count = row_count(
        &g,
        "MATCH (p: Person) -[:DIRECTED]-> (m: Movie) RETURN p.name, m.title",
    );
    assert_eq!(count, 44);
}

#[test]
fn test_wrote_traversal() {
    let g = movies_graph();
    let count = row_count(&g, "MATCH (p: Person) -[:WROTE]-> (m: Movie) RETURN p.name");
    assert_eq!(count, 10);
}

#[test]
fn test_produced_traversal() {
    let g = movies_graph();
    let count = row_count(
        &g,
        "MATCH (p: Person) -[:PRODUCED]-> (m: Movie) RETURN p.name",
    );
    assert_eq!(count, 15);
}

#[test]
fn test_reviewed_traversal() {
    let g = movies_graph();
    let count = row_count(
        &g,
        "MATCH (p: Person) -[:REVIEWED]-> (m: Movie) RETURN p.name",
    );
    assert_eq!(count, 9);
}

#[test]
fn test_follows_traversal() {
    let g = movies_graph();
    let count = row_count(
        &g,
        "MATCH (p1: Person) -[:FOLLOWS]-> (p2: Person) RETURN p1.name, p2.name",
    );
    assert_eq!(count, 3);
}

// ==================== WHERE filter tests ====================

#[test]
fn test_where_property_filter() {
    let g = movies_graph();
    // Movies released in 1999
    let rows = projected_rows(
        &g,
        "MATCH (m: Movie) WHERE m.released = 1999 RETURN m.title",
    );
    // The Matrix was released in 1999
    let titles: Vec<&Value> = rows.iter().map(|r| &r[0]).collect();
    assert!(titles.contains(&&Value::Str("The Matrix".into())));
}

#[test]
fn test_where_comparison() {
    let g = movies_graph();
    // Movies with more than 1000 votes
    let rows = projected_rows(&g, "MATCH (m: Movie) WHERE m.votes > 1000 RETURN m.title");
    // The Matrix Reloaded (1906) and others
    assert_eq!(rows.len(), 2);
}

// ==================== Multi-hop traversals ====================

#[test]
fn test_two_hop_traversal() {
    let g = movies_graph();
    // People who acted in movies that someone directed
    let count = row_count(&g,
        "MATCH (actor: Person) -[:ACTED_IN]-> (m: Movie) <-[:DIRECTED]- (director: Person) RETURN actor.name, director.name"
    );
    // Should have results — actors and directors of same movies
    assert!(count > 0);
}

// ==================== Comma-join (multi-pattern) queries ====================

#[test]
fn test_join_same_movie() {
    let g = movies_graph();
    // Find actor-director pairs for the same movie using comma-join
    let count = row_count(&g,
        "MATCH (a: Person) -[:ACTED_IN]-> (m: Movie), (d: Person) -[:DIRECTED]-> (m) RETURN a.name, d.name, m.title"
    );
    assert!(count > 0);
}

#[test]
fn test_join_coactors() {
    let g = movies_graph();
    // Find pairs of actors in the same movie
    let count = row_count(&g,
        "MATCH (a1: Person) -[:ACTED_IN]-> (m: Movie), (a2: Person) -[:ACTED_IN]-> (m) RETURN a1.name, a2.name"
    );
    // Includes self-pairs (a1 == a2), so count >= number of ACTED_IN edges
    assert!(count >= 172);
}

// ==================== Return projection tests ====================

#[test]
fn test_return_alias() {
    let _g = movies_graph();
    let q =
        gqlrust::compile_query("MATCH (m: Movie) WHERE m.released = 1999 RETURN m.title AS title")
            .unwrap();
    let returns = q.returns.as_ref().unwrap();
    assert_eq!(returns[0].alias(), Some("title"));
}

#[test]
fn test_return_distinct() {
    let g = movies_graph();
    // Without DISTINCT: multiple actors per movie → duplicate movie titles
    let all = row_count(
        &g,
        "MATCH (p: Person) -[:ACTED_IN]-> (m: Movie) RETURN m.title",
    );
    // With DISTINCT: each unique title only once
    let distinct = row_count(
        &g,
        "MATCH (p: Person) -[:ACTED_IN]-> (m: Movie) RETURN DISTINCT m.title",
    );
    assert!(distinct < all);
    assert!(distinct <= 38); // at most 38 movies
}
