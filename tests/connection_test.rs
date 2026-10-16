//! The embedded Rust API (`frogql::Connection`): create, write, save,
//! reopen, query, DDL, errors. The examples in `docs/rust-embedding.md`
//! follow these shapes.

use std::path::PathBuf;

use frogql::{Connection, Error, Outcome, Value};

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("frogql_conn_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.gdb");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(dir.join("db.gdb.ltj"));
    path
}

#[test]
fn create_insert_save_reopen_query() {
    let path = temp_db("roundtrip");
    {
        let conn = Connection::open(&path).unwrap();
        assert_eq!(conn.node_count(), 0);
        let Outcome::Modified(m) = conn
            .execute("INSERT (:Person {name: 'Ada', born: 1815})-[:KNOWS]->(:Person {name: 'Alan', born: 1912})")
            .unwrap()
        else {
            panic!("expected Modified");
        };
        assert_eq!((m.nodes_inserted, m.edges_inserted), (2, 1));
        // Unsaved writes are already visible to queries.
        let rows = conn
            .query("MATCH (p:Person) RETURN p.name AS name")
            .unwrap();
        assert_eq!(rows.len(), 2);
        conn.save().unwrap();
    }

    let conn = Connection::open(&path).unwrap();
    assert_eq!((conn.node_count(), conn.edge_count()), (2, 1));
    let rows = conn
        .query("MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN a.name AS a, b.name AS b")
        .unwrap();
    assert_eq!(rows.columns, vec!["a", "b"]);
    let row = rows.iter().next().unwrap();
    assert_eq!(row.get("a"), Some(&Value::Str("Ada".into())));
    assert_eq!(row.get("b"), Some(&Value::Str("Alan".into())));
}

#[test]
fn elements_resolve_and_columns_are_named() {
    let path = temp_db("elements");
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "INSERT (:Person {name: 'Ada'})-[:KNOWS {since: 1833}]->(:Person {name: 'Charles'})",
    )
    .unwrap();

    let rows = conn
        .query("MATCH (a {name: 'Ada'})-[e]->(b) RETURN a, e, b.name")
        .unwrap();
    assert_eq!(rows.columns, vec!["a", "e", "col2"]);
    let Value::Node(a) = rows.rows[0][0] else {
        panic!("expected a node")
    };
    let Value::Edge(e) = rows.rows[0][1] else {
        panic!("expected an edge")
    };
    let ada = conn.node(a);
    assert_eq!(ada.labels, vec!["Person"]);
    assert_eq!(ada.props.get("name"), Some(&Value::Str("Ada".into())));
    assert_eq!(conn.edge(e).props.get("since"), Some(&Value::Int(1833)));
    assert_eq!(conn.edge_endpoints(e).0, a);

    // Without RETURN: one column per pattern variable, sorted.
    let raw = conn.query("MATCH (x)-[r:KNOWS]->(y)").unwrap();
    assert_eq!(raw.columns, vec!["r", "x", "y"]);
    assert!(matches!(raw.rows[0][0], Value::Edge(_)));
}

#[test]
fn dml_updates_are_seen_by_the_next_query() {
    let path = temp_db("dml");
    let conn = Connection::open(&path).unwrap();
    conn.execute("INSERT (:A {k: 1}), (:A {k: 2})").unwrap();
    conn.execute("MATCH (x:A {k: 1}), (y:A {k: 2}) INSERT (x)-[:R]->(y)")
        .unwrap();
    assert_eq!(conn.query("MATCH (:A)-[:R]->(:A)").unwrap().len(), 1);
    conn.execute("MATCH (x:A {k: 2}) SET x.k = 3").unwrap();
    let rows = conn
        .query("MATCH (x:A) RETURN x.k AS k ORDER BY k")
        .unwrap();
    let ks: Vec<&Value> = rows.iter().map(|r| r.get("k").unwrap()).collect();
    assert_eq!(ks, vec![&Value::Int(1), &Value::Int(3)]);
    let Outcome::Modified(m) = conn.execute("MATCH (x:A) DETACH DELETE x").unwrap() else {
        panic!()
    };
    assert_eq!((m.nodes_deleted, m.edges_deleted), (2, 1));
    assert_eq!(conn.query("MATCH (x:A)").unwrap().len(), 0);
}

#[test]
fn ddl_and_show_statements() {
    let path = temp_db("ddl");
    let conn = Connection::open(&path).unwrap();
    conn.execute("INSERT (:Person {name: 'Ada'})").unwrap();

    let Outcome::Message(msg) = conn
        .execute("CREATE INDEX person_name ON :Person(name)")
        .unwrap()
    else {
        panic!()
    };
    assert!(msg.contains("person_name"), "{msg}");
    let idx = conn.query("SHOW INDEXES").unwrap();
    assert!(idx
        .iter()
        .any(|r| r.get("name") == Some(&Value::Str("person_name".into()))));
    conn.execute("DROP INDEX person_name").unwrap();
    assert!(matches!(
        conn.execute("DROP INDEX person_name"),
        Err(Error::Execution(_))
    ));

    let types = conn.query("SHOW GRAPH TYPES").unwrap();
    assert_eq!(types.columns, vec!["name", "active", "nodes", "edges"]);
    assert!(conn.schema().contains("Person"));
}

#[test]
fn compile_errors_are_typed() {
    let path = temp_db("errors");
    let conn = Connection::open(&path).unwrap();
    assert!(matches!(
        conn.execute("MATCH (x RETURN x"),
        Err(Error::Compile(_))
    ));
    // `query` refuses a statement that produces no rows.
    assert!(conn.query("INSERT (:A)").is_err());
}

#[test]
fn import_json_str_creates_a_database() {
    let path = temp_db("import");
    let json = r#"{
        "nodes": [
            {"id": "a", "labels": ["City"], "props": {"name": "Lima"}},
            {"id": "b", "labels": ["City"], "props": {"name": "Quito"}}
        ],
        "edges": [
            {"id": "e", "labels": ["ROAD"], "endpoints": ["a", "b"], "directionality": "->", "props": {}}
        ]
    }"#;
    frogql::connection::import_json_str(&path, json).unwrap();
    let conn = Connection::open(&path).unwrap();
    let rows = conn
        .query("MATCH (a:City)-[:ROAD]->(b:City) RETURN a.name AS from, b.name AS to")
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows.iter().next().unwrap().get("to"),
        Some(&Value::Str("Quito".into()))
    );
}
