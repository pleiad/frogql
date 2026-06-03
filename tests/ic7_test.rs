//! LDBC SNB Interactive Complex Read 7 ("recent likers") end-to-end.
//!
//! Exercises all six primitives added to unblock IC7: division `/`,
//! `FLOOR`, `CAST`, the `RECORD { ... }` constructor, the `VALUE { ... }`
//! value subquery (arg-max per liker), and `GROUP BY <binding variable>`.
//! `$personId` is replaced with a literal (the engine has no bind params).

use gqlrust::compile_query;
use gqlrust::model::graph::MemoryGraphStore;
use gqlrust::model::value::Value;
use gqlrust::runtime::engine::Runtime;
use gqlrust::runtime::result::QueryResult;

/// Target person P (prop id=5000) created a Comment c1 and a Post po1.
/// Three likers L1/L2/L3 like those messages at different times; L1 also
/// knows P (so isNew(L1)=false). Messages have creationDate 0 so the
/// minutesLatency is the like time in whole minutes.
fn graph() -> MemoryGraphStore {
    let json = r#"{
      "nodes": [
        {"id": "P",  "labels": ["Person"],  "props": {"id": 5000, "firstName": "Target", "lastName": "T"}},
        {"id": "L1", "labels": ["Person"],  "props": {"id": 11, "firstName": "Una", "lastName": "One"}},
        {"id": "L2", "labels": ["Person"],  "props": {"id": 12, "firstName": "Dos", "lastName": "Two"}},
        {"id": "L3", "labels": ["Person"],  "props": {"id": 13, "firstName": "Tri", "lastName": "Three"}},
        {"id": "c1", "labels": ["Comment"], "props": {"id": 1001, "content": "hi", "creationDate": 0}},
        {"id": "po1","labels": ["Post"],    "props": {"id": 1002, "imageFile": "img.png", "creationDate": 0}}
      ],
      "edges": [
        {"id": "h1", "labels": ["hasCreator"], "props": {}, "endpoints": ["c1", "P"], "directionality": "->"},
        {"id": "h2", "labels": ["hasCreator"], "props": {}, "endpoints": ["po1", "P"], "directionality": "->"},
        {"id": "k1", "labels": ["knows"], "props": {}, "endpoints": ["L1", "P"], "directionality": "~~"},
        {"id": "lk1", "labels": ["likes"], "props": {"creationDate": 60000},  "endpoints": ["L1", "c1"],  "directionality": "->"},
        {"id": "lk2", "labels": ["likes"], "props": {"creationDate": 180000}, "endpoints": ["L1", "po1"], "directionality": "->"},
        {"id": "lk3", "labels": ["likes"], "props": {"creationDate": 120000}, "endpoints": ["L2", "c1"],  "directionality": "->"},
        {"id": "lk4", "labels": ["likes"], "props": {"creationDate": 60000},  "endpoints": ["L3", "po1"], "directionality": "->"}
      ]
    }"#;
    MemoryGraphStore::from_json_str(json).unwrap()
}

const IC7: &str = "\
MATCH (person:Person {id: 5000})<-[:hasCreator]-(:Comment|Post)<-[:likes]-(liker:Person)
RETURN
    liker.id AS personId,
    liker.firstName AS personFirstName,
    liker.lastName AS personLastName,
    VALUE {
        MATCH (person)<-[:hasCreator]-(m:Comment|Post)<-[l:likes]-(liker)
        RETURN RECORD {
            likeCreationDate: l.creationDate,
            commentOrPostId: m.id,
            commentOrPostContent: COALESCE(m.content, m.imageFile),
            minutesLatency: CAST(FLOOR(CAST(l.creationDate - m.creationDate AS FLOAT) / 60000.0) AS INTEGER)
        } AS latest
        ORDER BY latest.likeCreationDate DESC, latest.commentOrPostId ASC
        LIMIT 1
    } AS latestLike,
    NOT EXISTS { MATCH (liker)~[:knows]~(person) } AS isNew
GROUP BY liker, person
ORDER BY latestLike.likeCreationDate DESC, personId ASC
LIMIT 20";

fn latest_record(ts: i64, msg_id: i64, content: &str, minutes: i64) -> Value {
    let mut m = std::collections::BTreeMap::new();
    m.insert("likeCreationDate".to_string(), Value::Int(ts));
    m.insert("commentOrPostId".to_string(), Value::Int(msg_id));
    m.insert(
        "commentOrPostContent".to_string(),
        Value::Str(content.to_string()),
    );
    m.insert("minutesLatency".to_string(), Value::Int(minutes));
    Value::Record(m)
}

#[test]
fn test_ic7_recent_likers_end_to_end() {
    let g = graph();
    let rt = Runtime::new(&g);
    let query = compile_query(IC7).unwrap_or_else(|e| panic!("IC7 failed to compile: {e}"));
    let rows = match rt.run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected rows, got {other:?}"),
    };

    // One row per liker, ordered by latest like date DESC:
    //   L1 (180000) → latest is po1 (img.png), 3 min, isNew=false (knows P)
    //   L2 (120000) → latest is c1 (hi),       2 min, isNew=true
    //   L3 (60000)  → latest is po1 (img.png), 1 min, isNew=true
    let expected = vec![
        vec![
            Value::Int(11),
            Value::Str("Una".into()),
            Value::Str("One".into()),
            latest_record(180000, 1002, "img.png", 3),
            Value::Bool(false),
        ],
        vec![
            Value::Int(12),
            Value::Str("Dos".into()),
            Value::Str("Two".into()),
            latest_record(120000, 1001, "hi", 2),
            Value::Bool(true),
        ],
        vec![
            Value::Int(13),
            Value::Str("Tri".into()),
            Value::Str("Three".into()),
            latest_record(60000, 1002, "img.png", 1),
            Value::Bool(true),
        ],
    ];
    assert_eq!(rows, expected);
}
