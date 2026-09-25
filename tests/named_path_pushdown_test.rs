//! A named path (`MATCH p = ...`) must not hide its variables from WHERE
//! pushdown.
//!
//! `pushdown::walk_kinds` had no `Named` arm, so it found no node variable
//! inside `p = ...`, and every value conjunct stayed a post-filter over
//! the fully materialised pattern. On a street graph that turned
//! `MATCH p = ANY SHORTEST (r:Reclamo)-...-(l) WHERE r.categoria = 'Ruido'`
//! into a search from every `Reclamo` — 2.2 s and 955 MB, against 0.7 s
//! and 294 MB for the same query written without `p =`.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::syntax::descriptor::Descriptor;
use frogql::syntax::path_pattern::PathPattern;

fn node_descriptors(p: &PathPattern, out: &mut Vec<Descriptor>) {
    match p {
        PathPattern::Node(Some(d)) => out.push(d.clone()),
        PathPattern::Concat(a, b) | PathPattern::Union(a, b) | PathPattern::Join(a, b) => {
            node_descriptors(a, out);
            node_descriptors(b, out);
        }
        PathPattern::Filter(inner, _)
        | PathPattern::Repeat { pattern: inner, .. }
        | PathPattern::Questioned(inner)
        | PathPattern::Selected { pattern: inner, .. }
        | PathPattern::Named { pattern: inner, .. } => node_descriptors(inner, out),
        _ => {}
    }
}

fn preds_of(q: &str, var: &str) -> usize {
    let query = compile_query(q).unwrap();
    let mut descs = Vec::new();
    node_descriptors(query.matches[0].pattern(), &mut descs);
    descs
        .iter()
        .filter(|d| d.var.as_deref() == Some(var))
        .map(|d| d.value_preds.len())
        .sum()
}

#[test]
fn named_path_gets_the_same_pushdown_as_an_unnamed_one() {
    let unnamed = "MATCH (a)-[:R]->(b) WHERE a.k = 1 AND b.k = 2 RETURN a.k AS k";
    let named = "MATCH p = (a)-[:R]->(b) WHERE a.k = 1 AND b.k = 2 RETURN a.k AS k";
    for var in ["a", "b"] {
        assert_eq!(preds_of(unnamed, var), 1, "{var} unnamed");
        assert_eq!(
            preds_of(named, var),
            1,
            "{var} lost its predicate under `p =`"
        );
    }
}

#[test]
fn named_selected_path_pushes_its_boundary_variables() {
    let q = "MATCH p = ANY SHORTEST (a)-[:R]->*(b) WHERE a.k = 1 AND b.k = 3 \
             RETURN path_length(p) AS n";
    assert_eq!(preds_of(q, "a"), 1);
    assert_eq!(preds_of(q, "b"), 1);
}

#[test]
fn named_path_answers_are_unchanged() {
    let json = r#"{"nodes":[
        {"id":"a","labels":["N"],"props":{"k":1}},
        {"id":"b","labels":["N"],"props":{"k":2}},
        {"id":"c","labels":["N"],"props":{"k":3}}
    ],"edges":[
        {"id":"ab","labels":["R"],"props":{},"endpoints":["a","b"],"directionality":"->"},
        {"id":"bc","labels":["R"],"props":{},"endpoints":["b","c"],"directionality":"->"},
        {"id":"ac","labels":["R"],"props":{},"endpoints":["a","c"],"directionality":"->"}
    ]}"#;
    let g = MemoryGraphStore::from_json_str(json).unwrap();
    let rt = Runtime::new(&g);
    let run = |q: &str| match rt.run_query(&compile_query(q).unwrap(), 0) {
        QueryResult::Projected(rows) => {
            let mut v: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
            v.sort();
            v
        }
        _ => panic!("expected projected"),
    };
    let named = run(
        "MATCH p = ANY SHORTEST (a:N)-[:R]->*(b:N) WHERE a.k = 1 AND b.k = 3 \
         RETURN path_length(p) AS n",
    );
    assert_eq!(named, vec!["[Int(1)]".to_string()]);
    assert_eq!(
        run("MATCH p = (a:N)-[:R]->(b:N) WHERE a.k = 1 RETURN b.k AS k"),
        run("MATCH (a:N)-[:R]->(b:N) WHERE a.k = 1 RETURN b.k AS k"),
    );
}
