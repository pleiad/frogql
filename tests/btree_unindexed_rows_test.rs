//! A declared btree skips the nodes whose property is null, so an
//! ORDER BY driven by it must still emit their rows, with the nulls.
//!
//! Two drivers walk the index: the bucket sort (a pattern without
//! edges) and btree-LTJ (one label, or a k-way merge for `A|B`). The
//! bucket sort and the merge used to drop the rows of the nodes the index
//! lacked, and single-label btree-LTJ declined such an index (and NULLS
//! FIRST) altogether. Each case runs the same query on a twin database
//! without the declared index, where the full sort answers, and expects
//! the same rows in the same key order.
//!
//! The last cases declare the index in the same session as the INSERT:
//! the session's nodes must be answered for once, not twice.

use std::path::PathBuf;

use frogql::{Connection, Value};

/// Alice→Bob→Dani→Alice and Alice→Carla→Ana; Dani has no age. Posts
/// carry a date, tags do not.
const GRAPH: &str = "INSERT (alice:Person {name: 'Alice', age: 30}), (bob:Person {name: 'Bob', age: 25}), \
     (carla:Person {name: 'Carla', age: 22}), (dani:Person {name: 'Dani'}), \
     (ana:Person {name: 'Ana', age: 41}), \
     (p1:Post {content: 'Hola', fecha: '2024-03-01'}), (p2:Post {content: 'GQL!', fecha: '2023-11-20'}), \
     (t1:Tag {name: 'GQL'}), (t2:Tag {name: 'Rust'}), \
     (alice)-[:KNOWS]->(bob), (bob)-[:KNOWS]->(dani), (dani)-[:KNOWS]->(alice), \
     (alice)-[:KNOWS]->(carla), (carla)-[:KNOWS]->(ana), \
     (alice)-[:CREATED]->(p1), (carla)-[:CREATED]->(p2), \
     (p1)-[:HAS_TAG]->(t1), (p2)-[:HAS_TAG]->(t1), (p2)-[:HAS_TAG]->(t2)";

fn open(name: &str, declared: &[&str]) -> Connection {
    let dir = std::env::temp_dir().join(format!("frogql_unindexed_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("db.gdb");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(dir.join("db.gdb.ltj"));
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(GRAPH).unwrap();
        conn.save().unwrap();
    }
    let conn = Connection::open(&path).unwrap();
    for ddl in declared {
        conn.execute(ddl).unwrap();
    }
    conn
}

fn rows(conn: &Connection, q: &str) -> Vec<Vec<Value>> {
    let rs = conn.query(q).unwrap();
    rs.iter().map(|r| r.values().to_vec()).collect()
}

/// Same multiset of rows, and the same sequence in column `key` (the
/// order between rows that tie on the key is implementation-defined).
fn assert_same(got: &[Vec<Value>], want: &[Vec<Value>], key: usize, q: &str) {
    let keys = |rs: &[Vec<Value>]| {
        rs.iter()
            .map(|r| format!("{:?}", r[key]))
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(got), keys(want), "key order of {q}");
    let bag = |rs: &[Vec<Value>]| {
        let mut v: Vec<String> = rs.iter().map(|r| format!("{r:?}")).collect();
        v.sort();
        v
    };
    assert_eq!(bag(got), bag(want), "rows of {q}");
}

fn check(name: &str, declared: &[&str], q: &str, key: usize) -> Vec<Vec<Value>> {
    let got = rows(&open(&format!("{name}_idx"), declared), q);
    let want = rows(&open(&format!("{name}_ref"), &[]), q);
    assert_same(&got, &want, key, q);
    got
}

const PERSON_AGE: &[&str] = &["CREATE BTREE INDEX ON :Person(age)"];

// No edge, so no LTJ: the bucket sort answers.

#[test]
fn bucket_keeps_the_rows_of_unindexed_nodes() {
    let got = check(
        "bucket_all",
        PERSON_AGE,
        "MATCH (p:Person) RETURN p.name, p.age ORDER BY p.age",
        1,
    );
    assert_eq!(got.len(), 5);
    assert_eq!(got.last().unwrap()[0], Value::Str("Dani".into()));
}

#[test]
fn bucket_places_unindexed_nodes_first_under_nulls_first() {
    let got = check(
        "bucket_first",
        PERSON_AGE,
        "MATCH (p:Person) RETURN p.name, p.age ORDER BY p.age NULLS FIRST LIMIT 2",
        1,
    );
    assert_eq!(got[0][0], Value::Str("Dani".into()));
}

#[test]
fn bucket_reaches_the_nulls_when_the_limit_outruns_the_index() {
    let got = check(
        "bucket_limit",
        PERSON_AGE,
        "MATCH (p:Person) RETURN p.name, p.age ORDER BY p.age DESC LIMIT 5",
        1,
    );
    assert_eq!(got.len(), 5);
}

#[test]
fn bucket_limit_inside_the_index_is_unchanged() {
    let got = check(
        "bucket_short",
        PERSON_AGE,
        "MATCH (p:Person) RETURN p.name, p.age ORDER BY p.age LIMIT 2",
        1,
    );
    assert_eq!(got[0][0], Value::Str("Carla".into()));
    assert_eq!(got[1][0], Value::Str("Bob".into()));
}

// One label with an edge: btree-LTJ, although the index lacks Dani.

#[test]
fn btree_ltj_walks_unindexed_nodes_last() {
    let got = check(
        "ltj_all",
        PERSON_AGE,
        "MATCH (p:Person)-[:KNOWS]->(f) RETURN p.name, p.age, f.name ORDER BY p.age",
        1,
    );
    // One row per KNOWS edge; Dani's goes last, with the nulls.
    assert_eq!(got.len(), 5);
    assert_eq!(got.last().unwrap()[0], Value::Str("Dani".into()));
}

#[test]
fn btree_ltj_walks_unindexed_nodes_first_under_nulls_first() {
    let got = check(
        "ltj_first",
        PERSON_AGE,
        "MATCH (p:Person)-[:KNOWS]->(f) RETURN p.name, p.age ORDER BY p.age NULLS FIRST LIMIT 2",
        1,
    );
    assert_eq!(got[0][0], Value::Str("Dani".into()));
    assert_eq!(got[1][0], Value::Str("Carla".into()));
}

#[test]
fn btree_ltj_second_key_under_nulls_first() {
    // Two sort keys: the walk stops only once the cohort of the last
    // primary value is complete, here the null one.
    let got = check(
        "ltj_two_keys",
        PERSON_AGE,
        "MATCH (p:Person)-[:KNOWS]->(f) RETURN p.name, p.age, f.name \
         ORDER BY p.age NULLS FIRST, f.name LIMIT 3",
        1,
    );
    assert_eq!(got.len(), 3);
}

#[test]
fn btree_ltj_limit_inside_the_index_is_unchanged() {
    let got = check(
        "ltj_short",
        PERSON_AGE,
        "MATCH (p:Person)-[:KNOWS]->(f) RETURN p.name, p.age ORDER BY p.age DESC LIMIT 2",
        1,
    );
    // Ana knows nobody, so the two oldest with an edge are both Alice's.
    assert_eq!(got[0][0], Value::Str("Alice".into()));
    assert_eq!(got[1][0], Value::Str("Alice".into()));
}

#[test]
fn btree_ltj_merge_keeps_the_rows_of_unindexed_nodes() {
    // Post.fecha is auto-indexed; the declared Tag.fecha btree is empty.
    let got = check(
        "merge_all",
        &["CREATE BTREE INDEX ON :Tag(fecha)"],
        "MATCH (m:Post|Tag)<-[:CREATED|HAS_TAG]-(z) RETURN m.content, m.name, m.fecha ORDER BY m.fecha",
        2,
    );
    // Two posts and three tag edges: the tags come last, with no date.
    assert_eq!(got.len(), 5);
    assert_eq!(got[3][2], Value::Null);
}

#[test]
fn btree_ltj_merge_limit_inside_the_index_is_unchanged() {
    let got = check(
        "merge_short",
        &["CREATE BTREE INDEX ON :Tag(fecha)"],
        "MATCH (m:Post|Tag)<-[:CREATED|HAS_TAG]-(z) RETURN m.content, m.fecha ORDER BY m.fecha LIMIT 1",
        1,
    );
    assert_eq!(got[0][0], Value::Str("GQL!".into()));
}

/// The graph inserted and the index declared in one session, unsaved.
fn open_unsaved(name: &str, declared: &[&str]) -> Connection {
    let dir = std::env::temp_dir().join(format!("frogql_unindexed_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("db.gdb");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(dir.join("db.gdb.ltj"));
    let conn = Connection::open(&path).unwrap();
    conn.execute(GRAPH).unwrap();
    for ddl in declared {
        conn.execute(ddl).unwrap();
    }
    conn
}

#[test]
fn index_declared_in_session_answers_once() {
    let conn = open_unsaved(
        "session_eq",
        &[
            "CREATE HASH INDEX ON :Person(age)",
            "CREATE BTREE INDEX ON :Person(age)",
        ],
    );
    assert_eq!(
        rows(&conn, "MATCH (p:Person) WHERE p.age = 25 RETURN p.name").len(),
        1
    );
    assert_eq!(
        rows(&conn, "MATCH (p:Person) WHERE p.age > 24 RETURN p.name").len(),
        3
    );
    let q = "MATCH (p:Person)-[:KNOWS]->(f) RETURN p.name, p.age, f.name ORDER BY p.age";
    let want = rows(&open("session_ref", &[]), q);
    assert_same(&rows(&conn, q), &want, 1, q);
}

#[test]
fn index_declared_in_session_sees_later_writes() {
    let conn = open_unsaved("session_writes", PERSON_AGE);
    conn.execute("MATCH (p:Person {name: 'Bob'}) SET p.age = 50")
        .unwrap();
    conn.execute("INSERT (:Person {name: 'Eva', age: 20})")
        .unwrap();
    assert!(rows(&conn, "MATCH (p:Person) WHERE p.age = 25 RETURN p.name").is_empty());
    let got = rows(
        &conn,
        "MATCH (p:Person) RETURN p.name, p.age ORDER BY p.age DESC LIMIT 2",
    );
    assert_eq!(got[0][0], Value::Str("Bob".into()));
    assert_eq!(got[1][0], Value::Str("Ana".into()));
    let first = rows(
        &conn,
        "MATCH (p:Person) RETURN p.name ORDER BY p.age LIMIT 1",
    );
    assert_eq!(first[0][0], Value::Str("Eva".into()));
}
