//! SPARQL frontend (`src/sparql/`): each SPARQL query is checked against
//! the answer SPARQL defines, and where a GQL query says the same thing,
//! against that query run on the same graph. Design and scope:
//! `docs/internals/sparql-frontend.md`.

use std::collections::BTreeMap;

use frogql::compile_query;
use frogql::compile_sparql_with;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::sparql::{NodeKey, PredicateNaming, SparqlMapping};
use frogql::typing::variable_type::Schema;
use frogql::Connection;

const EX: &str = "http://ex.org/";

/// alice → bob → carol → dave → erin over `knows` (plus alice → carol),
/// and who likes or hates which food.
const EDGES: &[(&str, &str, &str)] = &[
    ("alice", "knows", "bob"),
    ("alice", "knows", "carol"),
    ("bob", "knows", "carol"),
    ("carol", "knows", "dave"),
    ("dave", "knows", "erin"),
    ("alice", "likes", "pizza"),
    ("bob", "likes", "pizza"),
    ("carol", "likes", "pasta"),
    ("erin", "hates", "pizza"),
];
const NODES: &[&str] = &["alice", "bob", "carol", "dave", "erin", "pizza", "pasta"];

/// The social graph with every node keyed by its full IRI in `iri` and
/// labelled `R`; edge labels are `knows` (`full_labels = false`) or the
/// full predicate IRI.
fn social_json(full_labels: bool) -> String {
    let nodes: Vec<String> = NODES
        .iter()
        .map(|n| format!(r#"{{"id": "{n}", "labels": ["R"], "props": {{"iri": "{EX}{n}"}}}}"#))
        .collect();
    let edges: Vec<String> = EDGES
        .iter()
        .enumerate()
        .map(|(i, (s, p, o))| {
            let label = if full_labels {
                format!("{EX}{p}")
            } else {
                p.to_string()
            };
            format!(
                r#"{{"id": "e{i}", "labels": ["{label}"], "props": {{}}, "endpoints": ["{s}", "{o}"], "directionality": "->"}}"#
            )
        })
        .collect();
    format!(
        r#"{{"nodes": [{}], "edges": [{}]}}"#,
        nodes.join(","),
        edges.join(",")
    )
}

fn social() -> MemoryGraphStore {
    MemoryGraphStore::from_json_str(&social_json(false)).unwrap()
}

/// Local-name edge labels, whole IRIs in `iri`, every node labelled `R`.
fn local() -> SparqlMapping {
    SparqlMapping {
        predicates: PredicateNaming::LocalName,
        node_key: NodeKey::Iri { prop: "iri".into() },
        node_label: Some("R".into()),
    }
}

fn rows_of(result: QueryResult) -> Vec<Vec<Value>> {
    match result {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected rows, got {other:?}"),
    }
}

fn sparql_with(g: &MemoryGraphStore, m: &SparqlMapping, q: &str) -> Vec<Vec<Value>> {
    let compiled = compile_sparql_with(&Schema::star(), &format!("PREFIX ex: <{EX}> {q}"), m)
        .unwrap_or_else(|e| panic!("{q}\n  -> {e}"));
    rows_of(Runtime::new(g).run_query(&compiled.query, 0))
}

fn sparql(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    sparql_with(g, &local(), q)
}

fn gql(g: &MemoryGraphStore, q: &str) -> Vec<Vec<Value>> {
    let query = compile_query(q).unwrap_or_else(|e| panic!("{q}\n  -> {e}"));
    rows_of(Runtime::new(g).run_query(&query, 0))
}

/// Rows as a multiset, so two evaluations compare regardless of order.
fn bag(rows: Vec<Vec<Value>>) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for r in rows {
        *out.entry(format!("{r:?}")).or_default() += 1;
    }
    out
}

fn iri(name: &str) -> Value {
    Value::Str(format!("{EX}{name}"))
}

/// One-column rows, as a sorted list of local names (`-` for null).
fn names(rows: Vec<Vec<Value>>) -> Vec<String> {
    let mut out: Vec<String> = rows
        .into_iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.trim_start_matches(EX).to_string(),
            Value::Null => "-".to_string(),
            other => format!("{other:?}"),
        })
        .collect();
    out.sort();
    out
}

fn same(sparql_q: &str, gql_q: &str) {
    let g = social();
    let a = bag(sparql(&g, sparql_q));
    let b = bag(gql(&g, gql_q));
    assert!(!b.is_empty(), "the GQL twin returned nothing: {gql_q}");
    assert_eq!(a, b, "\nSPARQL: {sparql_q}\nGQL:    {gql_q}");
}

#[test]
fn single_triple_matches_gql() {
    same(
        "SELECT ?a ?b WHERE { ?a ex:knows ?b }",
        "MATCH (a)-[:knows]->(b) RETURN a.iri AS a, b.iri AS b",
    );
}

#[test]
fn bgp_join_matches_gql() {
    same(
        "SELECT ?a ?b ?f WHERE { ?a ex:knows ?b . ?b ex:likes ?f }",
        "MATCH (a)-[:knows]->(b), (b)-[:likes]->(f) RETURN a.iri AS a, b.iri AS b, f.iri AS f",
    );
}

#[test]
fn constant_subject_is_a_key_lookup() {
    let g = social();
    let rows = sparql(&g, "SELECT ?b WHERE { ex:alice ex:knows ?b }");
    assert_eq!(names(rows), ["bob", "carol"]);
}

#[test]
fn a_constant_mentioned_twice_is_one_node() {
    let g = social();
    let rows = sparql(
        &g,
        "SELECT ?b ?f WHERE { ex:alice ex:knows ?b . ex:alice ex:likes ?f }",
    );
    assert_eq!(
        bag(rows),
        bag(vec![
            vec![iri("bob"), iri("pizza")],
            vec![iri("carol"), iri("pizza")],
        ])
    );
}

#[test]
fn filter_matches_gql() {
    same(
        "SELECT ?a ?b WHERE { ?a ex:knows ?b FILTER(?b != ex:dave && ?a != ex:bob) }",
        "MATCH (a)-[:knows]->(b) WHERE b.iri <> 'http://ex.org/dave' AND a.iri <> 'http://ex.org/bob' \
         RETURN a.iri AS a, b.iri AS b",
    );
}

#[test]
fn filter_in_is_a_disjunction() {
    let g = social();
    let rows = sparql(
        &g,
        "SELECT ?b WHERE { ?a ex:knows ?b FILTER(?b IN (ex:bob, ex:dave)) }",
    );
    assert_eq!(names(rows), ["bob", "dave"]);
}

#[test]
fn optional_matches_gql() {
    same(
        "SELECT ?a ?b ?f WHERE { ?a ex:knows ?b OPTIONAL { ?b ex:likes ?f } }",
        "MATCH (a)-[:knows]->(b) OPTIONAL MATCH (b)-[:likes]->(f) \
         RETURN a.iri AS a, b.iri AS b, f.iri AS f",
    );
    // dave and erin like nothing: their rows survive with an unbound ?f.
    let g = social();
    let rows = sparql(
        &g,
        "SELECT ?f WHERE { ?a ex:knows ?b OPTIONAL { ?b ex:likes ?f } }",
    );
    assert_eq!(names(rows), ["-", "-", "pasta", "pasta", "pizza"]);
}

#[test]
fn filter_on_the_required_part_of_an_optional_group() {
    let g = social();
    let rows = sparql(
        &g,
        "SELECT ?b ?f WHERE { ?a ex:knows ?b OPTIONAL { ?b ex:likes ?f } FILTER(?a = ex:alice) }",
    );
    assert_eq!(
        bag(rows),
        bag(vec![
            vec![iri("bob"), iri("pizza")],
            vec![iri("carol"), iri("pasta")],
        ])
    );
}

#[test]
fn filter_over_an_optional_variable_is_refused() {
    let err = compile_sparql_with(
        &Schema::star(),
        "PREFIX ex: <http://ex.org/> SELECT ?a WHERE { ?a ex:knows ?b OPTIONAL { ?b ex:likes ?f } FILTER(!BOUND(?f)) }",
        &local(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("OPTIONAL"), "{err}");
}

#[test]
fn union_matches_label_disjunction() {
    same(
        "SELECT ?p ?f WHERE { { ?p ex:likes ?f } UNION { ?p ex:hates ?f } }",
        "MATCH (p)-[:likes|hates]->(f) RETURN p.iri AS p, f.iri AS f",
    );
}

#[test]
fn path_alternative_matches_label_disjunction() {
    same(
        "SELECT ?p ?f WHERE { ?p ex:likes|ex:hates ?f }",
        "MATCH (p)-[:likes|hates]->(f) RETURN p.iri AS p, f.iri AS f",
    );
}

#[test]
fn path_sequence_matches_gql() {
    same(
        "SELECT ?a ?f WHERE { ?a ex:knows/ex:likes ?f }",
        "MATCH (a)-[:knows]->()-[:likes]->(f) RETURN a.iri AS a, f.iri AS f",
    );
}

#[test]
fn path_inverse_matches_gql() {
    same(
        "SELECT ?a ?b WHERE { ?a ^ex:knows ?b }",
        "MATCH (a)<-[:knows]-(b) RETURN a.iri AS a, b.iri AS b",
    );
    same(
        "SELECT ?f ?a WHERE { ?f ^ex:likes/^ex:knows ?a }",
        "MATCH (f)<-[:likes]-()<-[:knows]-(a) RETURN f.iri AS f, a.iri AS a",
    );
}

#[test]
fn path_negated_set() {
    let g = social();
    let rows = sparql(&g, "SELECT ?x WHERE { ex:bob !ex:knows ?x }");
    assert_eq!(names(rows), ["pizza"]);
}

#[test]
fn one_or_more_is_reachability_once_per_pair() {
    // carol is reachable from alice along two paths; SPARQL reports the
    // pair once.
    let g = social();
    let rows = sparql(&g, "SELECT ?x WHERE { ex:alice ex:knows+ ?x }");
    assert_eq!(names(rows), ["bob", "carol", "dave", "erin"]);
}

#[test]
fn zero_or_more_and_zero_or_one_include_the_start() {
    let g = social();
    let star = sparql(&g, "SELECT ?x WHERE { ex:alice ex:knows* ?x }");
    assert_eq!(names(star), ["alice", "bob", "carol", "dave", "erin"]);
    let opt = sparql(&g, "SELECT ?x WHERE { ex:alice ex:knows? ?x }");
    assert_eq!(names(opt), ["alice", "bob", "carol"]);
}

#[test]
fn a_sequence_keeps_the_multiplicity_of_its_plain_steps() {
    // `knows/knows*` is a join at a hidden middle node: each of alice's two
    // friends contributes its own reachable set, so carol, dave and erin
    // appear twice. Only the closure is per pair.
    let g = social();
    let rows = sparql(&g, "SELECT ?x WHERE { ex:alice ex:knows/ex:knows* ?x }");
    assert_eq!(
        names(rows),
        ["bob", "carol", "carol", "dave", "dave", "erin", "erin"]
    );
}

#[test]
fn group_by_count_with_order() {
    let g = social();
    let rows = sparql(
        &g,
        "SELECT ?a (COUNT(?b) AS ?n) WHERE { ?a ex:knows ?b } GROUP BY ?a ORDER BY DESC(?n) ?a",
    );
    assert_eq!(
        rows,
        vec![
            vec![iri("alice"), Value::Int(2)],
            vec![iri("bob"), Value::Int(1)],
            vec![iri("carol"), Value::Int(1)],
            vec![iri("dave"), Value::Int(1)],
        ]
    );
}

#[test]
fn count_star_over_a_wildcard_predicate() {
    let g = social();
    let rows = sparql(&g, "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }");
    assert_eq!(rows, vec![vec![Value::Int(EDGES.len() as i64)]]);
}

#[test]
fn distinct_order_limit() {
    let g = social();
    let rows = sparql(
        &g,
        "SELECT DISTINCT ?f WHERE { ?p ex:likes ?f } ORDER BY ?f LIMIT 1",
    );
    assert_eq!(rows, vec![vec![iri("pasta")]]);
}

#[test]
fn exists_and_not_exists() {
    let g = social();
    let likers = sparql(
        &g,
        "SELECT DISTINCT ?p WHERE { ?p ex:knows ?q FILTER EXISTS { ?p ex:likes ?f } }",
    );
    assert_eq!(names(likers), ["alice", "bob", "carol"]);
    let others = sparql(
        &g,
        "SELECT DISTINCT ?p WHERE { ?p ex:knows ?q FILTER NOT EXISTS { ?p ex:likes ?f } }",
    );
    assert_eq!(names(others), ["dave"]);
}

#[test]
fn an_unbound_projected_variable_is_a_null_column() {
    let g = social();
    let rows = sparql(&g, "SELECT ?zz WHERE { ex:alice ex:likes ?f }");
    assert_eq!(rows, vec![vec![Value::Null]]);
}

#[test]
fn full_iri_labels() {
    let g = MemoryGraphStore::from_json_str(&social_json(true)).unwrap();
    let rows = sparql_with(
        &g,
        &SparqlMapping::default(),
        "SELECT ?b WHERE { <http://ex.org/alice> ex:knows ?b }",
    );
    assert_eq!(names(rows), ["bob", "carol"]);
}

#[test]
fn import_ttl_layout() {
    // What `import_ttl` writes: label `img`, an integer `id` holding the
    // local name, edges labelled by the predicate's local name.
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["img"], "props": {"id": 1}},
        {"id": "b", "labels": ["img"], "props": {"id": 2}},
        {"id": "c", "labels": ["img"], "props": {"id": 3}}
      ],
      "edges": [
        {"id": "e1", "labels": ["P121"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "e2", "labels": ["P121"], "props": {}, "endpoints": ["a", "c"], "directionality": "->"},
        {"id": "e3", "labels": ["P7"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"}
      ]
    }"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let m = SparqlMapping::import_ttl("img", "id");
    let rows = sparql_with(
        &g,
        &m,
        "PREFIX img: <http://imgpedia.dcc.uchile.cl/resource/> \
         SELECT ?x WHERE { img:1 img:P121 ?x } ORDER BY ?x",
    );
    assert_eq!(rows, vec![vec![Value::Int(2)], vec![Value::Int(3)]]);
    // An IRI with no integer local name names no node.
    let none = sparql_with(&g, &m, "SELECT ?x WHERE { ex:alice ex:P121 ?x }");
    assert!(none.is_empty());
}

#[test]
fn through_a_connection_with_the_inferred_schema() {
    let dir = std::env::temp_dir().join(format!("frogql_sparql_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.gdb");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(dir.join("db.gdb.ltj"));
    frogql::connection::import_json_str(&path, &social_json(false)).unwrap();
    let conn = Connection::open(&path).unwrap();
    let rows = conn
        .query_sparql(
            "PREFIX ex: <http://ex.org/> SELECT ?b ?f WHERE { ex:alice ex:knows ?b OPTIONAL { ?b ex:likes ?f } }",
            &local(),
            0,
        )
        .unwrap();
    assert_eq!(rows.columns, vec!["b", "f"]);
    let got: Vec<Vec<Value>> = rows.iter().map(|r| r.values().to_vec()).collect();
    assert_eq!(
        bag(got),
        bag(vec![
            vec![iri("bob"), iri("pizza")],
            vec![iri("carol"), iri("pasta")],
        ])
    );
}
