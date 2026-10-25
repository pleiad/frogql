//! Graph-type statements persist with `save`, like data (issue #103).
//!
//! `CREATE` / `USE` / `DROP` / `VALIDATE GRAPH TYPE` used to write the
//! catalog to the file at once while the data waited for `save`, and the
//! two came apart:
//!
//! - `USE GRAPH TYPE DEFAULT` after an unsaved `DELETE` wrote a DEFAULT
//!   without the deleted label; reopened without `save`, the file still
//!   held the node and the typechecker declared a query over it empty;
//! - after a `save`, which replaces the opened file, a `CREATE GRAPH TYPE`
//!   was written into the replaced file and lost, though it said `created`.

use std::path::PathBuf;

use frogql::{Connection, Value};

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "frogql_catalog_persist_{}_{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.gdb");
    for ext in ["", ".ltj", ".tmp"] {
        let mut p = path.as_os_str().to_os_string();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
    path
}

fn seeded(name: &str) -> PathBuf {
    let path = temp_db(name);
    frogql::connection::import_json_str(
        &path,
        r#"{"nodes":[
            {"id":"a","labels":["Person"],"props":{"name":"Ana"}},
            {"id":"m","labels":["Movie"],"props":{"title":"T"}}],"edges":[]}"#,
    )
    .unwrap();
    path
}

fn type_names(conn: &Connection) -> Vec<(String, bool)> {
    conn.query("SHOW GRAPH TYPES")
        .unwrap()
        .iter()
        .map(|r| {
            let name = match r.get("name") {
                Some(Value::Str(s)) => s.clone(),
                other => panic!("name: {other:?}"),
            };
            let active = r.get("active") == Some(&Value::Bool(true));
            (name, active)
        })
        .collect()
}

#[test]
fn graph_type_statements_wait_for_save() {
    let path = seeded("wait_for_save");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("CREATE GRAPH TYPE t AS { (:Person {name STRING}), (:Movie {title STRING}) }")
            .unwrap();
        conn.execute("USE GRAPH TYPE t").unwrap();
        assert!(type_names(&conn).contains(&("t".into(), true)));
    } // closed without save
    let conn = Connection::open(&path).unwrap();
    assert_eq!(type_names(&conn), vec![("DEFAULT".into(), true)]);

    conn.execute("CREATE GRAPH TYPE t AS { (:Person {name STRING}), (:Movie {title STRING}) }")
        .unwrap();
    conn.execute("USE GRAPH TYPE t").unwrap();
    conn.save().unwrap();
    drop(conn);
    let conn = Connection::open(&path).unwrap();
    assert!(type_names(&conn).contains(&("t".into(), true)));
}

#[test]
fn a_default_refreshed_over_unsaved_data_does_not_reach_the_file() {
    let path = seeded("default_unsaved");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("MATCH (x:Movie) DETACH DELETE x").unwrap();
        conn.execute("USE GRAPH TYPE DEFAULT").unwrap();
    } // closed without save
    let conn = Connection::open(&path).unwrap();
    // The Movie is still in the file, and the DEFAULT in the file still
    // says so: this answered no rows, typechecked as guaranteed empty.
    let rows = conn.query("MATCH (x:Movie) RETURN x.title AS t").unwrap();
    assert_eq!(rows.len(), 1);
}

#[test]
fn a_graph_type_created_after_a_save_survives_the_next_save() {
    let path = seeded("after_save");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("INSERT (:Person {name: 'Bea'})").unwrap();
        conn.save().unwrap();
        conn.execute("CREATE GRAPH TYPE late AS { (:Person {name STRING}) }")
            .unwrap();
        conn.save().unwrap();
    }
    let conn = Connection::open(&path).unwrap();
    assert!(type_names(&conn).iter().any(|(n, _)| n == "late"));
    assert_eq!(conn.node_count(), 3);
}
