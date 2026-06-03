//! Tests covering Float property values: parsing, JSON loading, mixed arithmetic,
//! comparison, type predicates, and .gdb storage roundtrip.

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;
use gqlrust::store::lazy::LazyGraphStore;

fn graph_with_floats() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "p1", "labels": ["Product"], "props": {"name": "A", "price": 9.99, "stock": 10}},
        {"id": "p2", "labels": ["Product"], "props": {"name": "B", "price": 12.5, "stock": 3}},
        {"id": "p3", "labels": ["Product"], "props": {"name": "C", "price": 7, "stock": 0}}
      ],
      "edges": []
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

fn run(g: &MemoryGraphStore, q: &str) -> QueryResult {
    let rt = Runtime::new(g);
    let query = compile_query(q).unwrap();
    rt.run_query(&query, 0)
}

#[test]
fn test_json_loads_float() {
    let g = graph_with_floats();
    assert_eq!(g.node_count(), 3);
}

#[test]
fn test_return_float() {
    let g = graph_with_floats();
    let res = run(&g, "MATCH (x: Product) WHERE x.name = 'A' RETURN x.price");
    match res {
        QueryResult::Projected(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0], vec![Value::Float(9.99)]);
        }
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_float_comparison_mixed_int_float() {
    let g = graph_with_floats();
    // Int literal compared against float property: price > 8 matches A (9.99) and B (12.5).
    let res = run(&g, "MATCH (x: Product) WHERE x.price > 8 RETURN x.name");
    match res {
        QueryResult::Projected(rows) => assert_eq!(rows.len(), 2),
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_float_literal_comparison() {
    let g = graph_with_floats();
    let res = run(&g, "MATCH (x: Product) WHERE x.price < 10.0 RETURN x.name");
    match res {
        QueryResult::Projected(rows) => assert_eq!(rows.len(), 2), // A (9.99) and C (7)
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_is_float_predicate() {
    let g = graph_with_floats();
    let res = run(&g, "MATCH (x: Product) WHERE x.price float RETURN x.name");
    match res {
        QueryResult::Projected(rows) => assert_eq!(rows.len(), 2), // A and B
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_float_arithmetic() {
    let g = graph_with_floats();
    let res = run(
        &g,
        "MATCH (x: Product) WHERE x.name = 'A' RETURN x.price + 0.01 AS total",
    );
    match res {
        QueryResult::Projected(rows) => {
            assert_eq!(rows.len(), 1);
            match &rows[0][0] {
                Value::Float(x) => assert!((x - 10.0).abs() < 1e-9, "got {x}"),
                v => panic!("expected float, got {v:?}"),
            }
        }
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_float_storage_roundtrip() {
    let g = graph_with_floats();
    let tmp = std::env::temp_dir().join("gqlite_float_roundtrip.gdb");
    let _ = std::fs::remove_file(&tmp);
    g.save(&tmp).unwrap();

    let store = LazyGraphStore::open(&tmp).unwrap();
    let rt = Runtime::new(&store);
    let q = compile_query("MATCH (x: Product) WHERE x.name = 'B' RETURN x.price").unwrap();
    match rt.run_query(&q, 0) {
        QueryResult::Projected(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0], vec![Value::Float(12.5)]);
        }
        _ => panic!("expected projected"),
    }
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_lexer_float_vs_attr_lookup() {
    // `x.price` should still lex as attribute access, `1.5` as a float literal —
    // exercised together in the same query.
    let g = graph_with_floats();
    let res = run(&g, "MATCH (x: Product) WHERE x.price > 1.5 RETURN x.name");
    match res {
        QueryResult::Projected(rows) => assert_eq!(rows.len(), 3),
        _ => panic!("expected projected"),
    }
}

// ===== Multiplication (`*`) and DURATION desugaring =====

#[test]
fn test_int_multiplication() {
    let g = graph_with_floats();
    // stock 10 * 3 = 30 → only A.
    let res = run(
        &g,
        "MATCH (x: Product) WHERE x.stock * 3 = 30 RETURN x.name",
    );
    match res {
        QueryResult::Projected(rows) => {
            assert_eq!(rows, vec![vec![Value::Str("A".into())]]);
        }
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_mixed_multiplication_widens_to_float() {
    let g = graph_with_floats();
    // price 12.5 * 2 = 25.0 (int operand widens) → only B.
    let res = run(
        &g,
        "MATCH (x: Product) WHERE x.price * 2 = 25.0 RETURN x.name",
    );
    match res {
        QueryResult::Projected(rows) => {
            assert_eq!(rows, vec![vec![Value::Str("B".into())]]);
        }
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_multiplication_binds_tighter_than_plus() {
    let g = graph_with_floats();
    // 1 + 1 * 2 = 3 (not 4) — `*` binds first; A has stock 10, so
    // stock + 1 * 2 = 12 matches only A.
    let res = run(
        &g,
        "MATCH (x: Product) WHERE x.stock + 1 * 2 = 12 RETURN x.name",
    );
    match res {
        QueryResult::Projected(rows) => {
            assert_eq!(rows, vec![vec![Value::Str("A".into())]]);
        }
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_duration_desugars_to_milliseconds() {
    let g = graph_with_floats();
    // DURATION({days: 1}) == 86_400_000 holds for every row → all 3.
    let res = run(
        &g,
        "MATCH (x: Product) WHERE DURATION({days: 1}) = 86400000 RETURN x.name",
    );
    match res {
        QueryResult::Projected(rows) => assert_eq!(rows.len(), 3),
        _ => panic!("expected projected"),
    }
}

#[test]
fn test_duration_multi_unit_and_arithmetic() {
    let g = graph_with_floats();
    // 0 + DURATION({days: 1, hours: 1}) = 86_400_000 + 3_600_000 = 90_000_000.
    let res = run(
        &g,
        "MATCH (x: Product) WHERE x.stock + DURATION({days: 1, hours: 1}) = 90000000 RETURN x.name",
    );
    match res {
        QueryResult::Projected(rows) => {
            // stock 0 (C) + 90_000_000 = 90_000_000 → only C.
            assert_eq!(rows, vec![vec![Value::Str("C".into())]]);
        }
        _ => panic!("expected projected"),
    }
}
