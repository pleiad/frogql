use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["Person"], "props": {"id": 10}},
        {"id": "b", "labels": ["Person"], "props": {"id": 20}},
        {"id": "c", "labels": ["Person"], "props": {"id": 30}}
      ],
      "edges": [
        {"id": "e1", "labels": ["knows"], "props": {}, "endpoints": ["a", "b"], "directionality": "->"},
        {"id": "e2", "labels": ["knows"], "props": {}, "endpoints": ["b", "c"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

#[test]
fn test_unused_edge_variable_in_repeat_is_unrolled() {
    let q_str = "MATCH (n1:Person)-[e]-{1,2}(n2:Person) WHERE n1.id = 10 RETURN n1.id, n2.id";
    let query = compile_query(q_str).unwrap();
    let pattern_str = query.matches[0].pattern().to_string();
    
    // Because `e` is unused, the pass should strip its name, enabling unroll_repeat.
    // Therefore, the compiled query pattern should NOT contain the repetition `{1,2}` anymore.
    assert!(!pattern_str.contains("{1,2}"), "Unused repetition should be unrolled: {}", pattern_str);
    assert!(pattern_str.contains("|"), "Unrolled repetition should be a Union: {}", pattern_str);

    // Run the query and verify correctness of results
    let g = graph();
    let rt = Runtime::new(&g);
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rows) => {
            // Paths of length 1: (a)-[e1]-(b) -> (10, 20)
            // Paths of length 2: (a)-[e1]-(b)-[e2]-(c) -> (10, 30)
            // Plus reverse paths since they are undirected:
            // Since the graph is undirected for -[e]-:
            // We should get at least [10, 20] and [10, 30].
            let mut ids: Vec<i64> = rows
                .iter()
                .map(|row| match &row[1] {
                    Value::Int(val) => *val,
                    other => panic!("expected Int, got {:?}", other),
                })
                .collect();
            ids.sort();
            assert_eq!(ids, vec![10, 20, 30]);
        }
        other => panic!("expected projected rows, got {other:?}"),
    }
}

#[test]
fn test_used_edge_variable_in_repeat_is_not_unrolled() {
    // If the edge variable `e` is returned, it is used, so it must not be unrolled.
    let q_str = "MATCH (n1:Person)-[e]-{1,2}(n2:Person) WHERE n1.id = 10 RETURN n1.id, e, n2.id";
    let query = compile_query(q_str).unwrap();
    let pattern_str = query.matches[0].pattern().to_string();

    assert!(pattern_str.contains("{1,2}"), "Used repetition should NOT be unrolled: {}", pattern_str);
    assert!(!pattern_str.contains("|"), "Used repetition should NOT be unrolled into a Union: {}", pattern_str);
}

#[test]
fn test_no_returns_clause_preserves_all_variables() {
    // If there is no returns clause, all variables are preserved because the raw intermediate result is returned.
    let q_str = "MATCH (n1:Person)-[e]-{1,2}(n2:Person)";
    let query = compile_query(q_str).unwrap();
    let pattern_str = query.matches[0].pattern().to_string();

    assert!(pattern_str.contains("{1,2}"), "Repetition without returns clause should NOT be unrolled: {}", pattern_str);
}

#[test]
fn test_used_edge_variable_in_repeat_results_correctness() {
    let q_str = "MATCH (n1:Person)-[e]-{1,2}(n2:Person) WHERE n1.id = 10 RETURN n1.id, e, n2.id";
    let query = compile_query(q_str).unwrap();
    let g = graph();
    let rt = Runtime::new(&g);
    match rt.run_query(&query, 0) {
        QueryResult::Projected(rows) => {
            let mut results = Vec::new();
            for r in rows {
                let n1_id = match &r[0] { Value::Int(v) => *v, _ => panic!() };
                let n2_id = match &r[2] { Value::Int(v) => *v, _ => panic!() };
                let e_list = match &r[1] {
                    Value::List(l) => l.iter().map(|item| match item {
                        Value::Edge(eid) => *eid,
                        other => panic!("expected Edge, got {:?}", other),
                    }).collect::<Vec<u32>>(),
                    other => panic!("expected List, got {:?}", other),
                };
                results.push((n1_id, e_list, n2_id));
            }
            results.sort_by_key(|r| (r.2, r.1.len(), r.1.clone()));
            assert_eq!(results.len(), 3);
            
            // Result 1: to node 10 (length 2 path traversing e1 twice)
            assert_eq!(results[0].0, 10);
            assert_eq!(results[0].2, 10);
            assert_eq!(results[0].1.len(), 2);
            assert_eq!(results[0].1[0], results[0].1[1]); // same edge traversed twice
            
            // Result 2: to node 20 (length 1 path)
            assert_eq!(results[1].0, 10);
            assert_eq!(results[1].2, 20);
            assert_eq!(results[1].1.len(), 1);
            
            // Result 3: to node 30 (length 2 path traversing e1 then e2)
            assert_eq!(results[2].0, 10);
            assert_eq!(results[2].2, 30);
            assert_eq!(results[2].1.len(), 2);
            assert_ne!(results[2].1[0], results[2].1[1]); // e1 and e2 are different edges
        }
        other => panic!("expected projected rows, got {other:?}"),
    }
}
