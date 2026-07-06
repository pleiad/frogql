//! Pushdown / 3VL consistency.
//!
//! The interpreter (`eval_binop`) yields `Value::Null` for a null-operand
//! comparison; the pushdown path (`cmp_values`, used by the LTJ `NodeAttrCmp`
//! filter and the standard scan) yields `false`. These *look* divergent but
//! must produce identical rows, because the value-predicate pushdown only lifts
//! top-level positive `attr op literal` AND-conjuncts — where a null/missing
//! operand means "drop the row" under both (`false`→drop; `Null`→get_bool
//! false→drop). The keep-despite-null cases (`… OR true`, `NOT (…)`) are never
//! pushed and stay on the 3VL interpreter.
//!
//! This suite pins that invariant. It fails if the optimizer ever pushes a
//! non-conjunct (out of an OR / under a NOT — a bug for non-null rows too), or
//! if `cmp_values` and `eval_binop` drift apart on the keep/drop decision.

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;

/// Three N-nodes: a=5, a=6, and one with `a` absent (reads as null).
fn nodes() -> MemoryGraphStore {
    MemoryGraphStore::from_json_str(
        r#"{"nodes":[
            {"id":"p5","labels":["N"],"props":{"name":"p5","a":5}},
            {"id":"p6","labels":["N"],"props":{"name":"p6","a":6}},
            {"id":"pN","labels":["N"],"props":{"name":"pN"}}
        ],"edges":[]}"#,
    )
    .unwrap()
}

/// Same three nodes, each with one outgoing edge to a shared sink — so a pushed
/// `x.a <op> literal` filter runs inside the LTJ `NodeAttrCmp` path, not the
/// plain node scan.
fn nodes_with_edges() -> MemoryGraphStore {
    MemoryGraphStore::from_json_str(
        r#"{"nodes":[
            {"id":"p5","labels":["N"],"props":{"name":"p5","a":5}},
            {"id":"p6","labels":["N"],"props":{"name":"p6","a":6}},
            {"id":"pN","labels":["N"],"props":{"name":"pN"}},
            {"id":"s","labels":["S"],"props":{"name":"s"}}
        ],"edges":[
            {"id":"e5","labels":["E"],"props":{},"endpoints":["p5","s"],"directionality":"->"},
            {"id":"e6","labels":["E"],"props":{},"endpoints":["p6","s"],"directionality":"->"},
            {"id":"eN","labels":["E"],"props":{},"endpoints":["pN","s"],"directionality":"->"}
        ]}"#,
    )
    .unwrap()
}

fn names(g: &MemoryGraphStore, q: &str) -> Vec<String> {
    let query = compile_query(q).expect("compile");
    let mut ns: Vec<String> = match Runtime::new(g).run_query(&query, 0) {
        QueryResult::Projected(rs) => rs
            .into_iter()
            .map(|r| match &r[0] {
                Value::Str(s) => s.clone(),
                other => panic!("expected Str, got {other:?}"),
            })
            .collect(),
        other => panic!("expected projected rows, got {other:?}"),
    };
    ns.sort();
    ns
}

fn v(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_pushed_scan_filters_are_3vl_consistent() {
    let g = nodes();
    // pushed vs reversed (same logical comparison, possibly different path)
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a = 5 RETURN x.name"),
        v(&["p5"])
    );
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE 5 = x.a RETURN x.name"),
        v(&["p5"])
    );
    // <> drops the missing-property row (null <> 5 = unknown -> drop)
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a <> 5 RETURN x.name"),
        v(&["p6"])
    );
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE 5 <> x.a RETURN x.name"),
        v(&["p6"])
    );
    // ordering + range-fold (btree) path — nulls not indexable -> excluded
    assert!(names(&g, "MATCH (x:N) WHERE x.a > 100 RETURN x.name").is_empty());
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a >= 5 RETURN x.name"),
        v(&["p5", "p6"])
    );
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a IS NULL RETURN x.name"),
        v(&["pN"])
    );
}

#[test]
fn test_keep_despite_null_stays_residual_not_pushed() {
    let g = nodes();
    // `= 999` must NOT be lifted out of the OR: `_ OR true` keeps every row.
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a = 999 OR true RETURN x.name"),
        v(&["p5", "p6", "pN"]),
    );
    // NOT over a comparison stays residual: missing -> NOT(null=5)=null -> drop.
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE NOT (x.a = 5) RETURN x.name"),
        v(&["p6"])
    );
    // OR of two pushable conjuncts is still a residual OR, not two pushed filters.
    assert_eq!(
        names(&g, "MATCH (x:N) WHERE x.a = 5 OR x.a = 6 RETURN x.name"),
        v(&["p5", "p6"]),
    );
}

#[test]
fn test_pushed_ltj_filters_are_3vl_consistent() {
    // Same predicates, but with an edge so the filter runs in the LTJ
    // `NodeAttrCmp` path (which calls the same `cmp_values`).
    let g = nodes_with_edges();
    assert_eq!(
        names(&g, "MATCH (x:N)-[:E]->(s) WHERE x.a = 5 RETURN x.name"),
        v(&["p5"]),
    );
    assert_eq!(
        names(&g, "MATCH (x:N)-[:E]->(s) WHERE x.a <> 5 RETURN x.name"),
        v(&["p6"]), // missing-a row dropped, exactly as the scan path
    );
    assert!(names(&g, "MATCH (x:N)-[:E]->(s) WHERE x.a > 100 RETURN x.name").is_empty(),);
}
