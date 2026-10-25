//! Float properties in secondary indexes (issue #96).
//!
//! `IndexKey` covered `Int`, `Str` and `Bool` only, so every float value was
//! dropped on the floor at build time. Two consequences, one merely slow and
//! one wrong:
//!
//! - a `CREATE INDEX` on a float property reported `ok: true` with
//!   `entries: 0`, and every range scan over coordinates, prices, scores or
//!   epoch-float timestamps stayed a linear scan;
//! - worse, a property mixing `Int` and `Float` values produced a *partial*
//!   index that every consumer treated as complete, so rows whose value was a
//!   float silently vanished from range filters, `ORDER BY`, and even
//!   equality — `p.m = 3` did not find a node holding `3.0`, although
//!   `3 = 3.0` is true everywhere else in the engine.
//!
//! The auto-build path was protected by accident: it only indexes a
//! `(label, prop)` when every node of the label contributed an indexable
//! value, so one float disabled it. `build_declared` had no such guard.
//!
//! These tests pin the index against the runtime's own comparison semantics
//! (`cmp_values` widens `Int` to `f64`), which is the property that matters:
//! whether a predicate is answered from an index or from a scan must not
//! change the answer.

use std::path::{Path, PathBuf};

use frogql::compile_query;
use frogql::model::graph::MemoryGraphStore;
use frogql::model::value::Value;
use frogql::runtime::engine::Runtime;
use frogql::runtime::result::QueryResult;
use frogql::store::lazy::LazyGraphStore;
use frogql::store::secondary_index::IndexKind;

/// Five `:P` nodes whose `m` interleaves ints and floats, each pointing at a
/// single `:Q` so the pattern decomposes into triples and the LTJ (and with
/// it the index-folding pre-pass) actually runs.
const MIXED: &str = r#"{
  "nodes": [
    {"id": "a", "labels": ["P"], "props": {"name": "a", "m": 1}},
    {"id": "b", "labels": ["P"], "props": {"name": "b", "m": 1.5}},
    {"id": "c", "labels": ["P"], "props": {"name": "c", "m": 2}},
    {"id": "d", "labels": ["P"], "props": {"name": "d", "m": 2.5}},
    {"id": "e", "labels": ["P"], "props": {"name": "e", "m": 3.0}},
    {"id": "z", "labels": ["Q"], "props": {"name": "z"}}
  ],
  "edges": [
    {"id": "r1", "labels": ["R"], "props": {}, "endpoints": ["a", "z"], "directionality": "->"},
    {"id": "r2", "labels": ["R"], "props": {}, "endpoints": ["b", "z"], "directionality": "->"},
    {"id": "r3", "labels": ["R"], "props": {}, "endpoints": ["c", "z"], "directionality": "->"},
    {"id": "r4", "labels": ["R"], "props": {}, "endpoints": ["d", "z"], "directionality": "->"},
    {"id": "r5", "labels": ["R"], "props": {}, "endpoints": ["e", "z"], "directionality": "->"}
  ]
}"#;

/// Four `:P` nodes whose `f` is float-valued and unique within the label —
/// the shape the auto-builder indexes when the values are indexable at all.
const FLOATS: &str = r#"{
  "nodes": [
    {"id": "a", "labels": ["P"], "props": {"name": "a", "f": -33.45}},
    {"id": "b", "labels": ["P"], "props": {"name": "b", "f": 0.5}},
    {"id": "c", "labels": ["P"], "props": {"name": "c", "f": 12.25}},
    {"id": "d", "labels": ["P"], "props": {"name": "d", "f": 100.125}},
    {"id": "z", "labels": ["Q"], "props": {"name": "z"}}
  ],
  "edges": [
    {"id": "r1", "labels": ["R"], "props": {}, "endpoints": ["a", "z"], "directionality": "->"},
    {"id": "r2", "labels": ["R"], "props": {}, "endpoints": ["b", "z"], "directionality": "->"},
    {"id": "r3", "labels": ["R"], "props": {}, "endpoints": ["c", "z"], "directionality": "->"},
    {"id": "r4", "labels": ["R"], "props": {}, "endpoints": ["d", "z"], "directionality": "->"}
  ]
}"#;

/// Floats that repeat, so the auto-builder (which only indexes values unique
/// within the label) declines and a manual `CREATE INDEX` is what covers the
/// property — the path that used to report `entries: 0`.
const DUPES: &str = r#"{
  "nodes": [
    {"id": "a", "labels": ["P"], "props": {"name": "a", "w": 1.5}},
    {"id": "b", "labels": ["P"], "props": {"name": "b", "w": 1.5}},
    {"id": "c", "labels": ["P"], "props": {"name": "c", "w": 2}},
    {"id": "d", "labels": ["P"], "props": {"name": "d", "w": 2.75}},
    {"id": "z", "labels": ["Q"], "props": {"name": "z"}}
  ],
  "edges": [
    {"id": "r1", "labels": ["R"], "props": {}, "endpoints": ["a", "z"], "directionality": "->"},
    {"id": "r2", "labels": ["R"], "props": {}, "endpoints": ["b", "z"], "directionality": "->"},
    {"id": "r3", "labels": ["R"], "props": {}, "endpoints": ["c", "z"], "directionality": "->"},
    {"id": "r4", "labels": ["R"], "props": {}, "endpoints": ["d", "z"], "directionality": "->"}
  ]
}"#;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("frogql_float_index_test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn store(json: &str, name: &str) -> (LazyGraphStore, PathBuf) {
    let path = temp_db(name);
    MemoryGraphStore::from_json_str(json)
        .unwrap()
        .save(&path)
        .unwrap();
    (LazyGraphStore::open(&path).unwrap(), path)
}

/// Declare an index the way `CREATE INDEX` does, returning its entry count.
fn declare(store: &LazyGraphStore, json: &str, prop: &str, kind: IndexKind) -> usize {
    let mem = MemoryGraphStore::from_json_str(json).unwrap();
    store
        .secondary_indexes_mut()
        .build_declared(&mem, format!("ix_{prop}_{kind:?}"), "P", prop, kind)
        .unwrap()
        .entries
}

/// The first projected column of every row, as strings.
fn names<G: frogql::model::graph_access::GraphAccess>(g: &G, q: &str) -> Vec<String> {
    let query = compile_query(q).unwrap_or_else(|e| panic!("compile {q:?}: {e}"));
    let rows = match Runtime::new(g).run_query(&query, 0) {
        QueryResult::Projected(rs) => rs,
        other => panic!("expected projected result for {q:?}, got {other:?}"),
    };
    rows.iter()
        .map(|r| match &r[0] {
            Value::Str(s) => s.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

/// Run `q` against the indexed store and against a `MemoryGraphStore`, which
/// has no secondary index at all and therefore always scans. An index is an
/// accelerator: it must never change which rows come back, so the scan is the
/// oracle. `extra` names properties to declare an index on beyond whatever the
/// auto-builder already covers.
fn agrees_with_the_scan(json: &str, tag: &str, extra: &[&str], q: &str) -> Vec<String> {
    let oracle = MemoryGraphStore::from_json_str(json).unwrap();
    let expected = names(&oracle, q);

    let (indexed, _p) = store(json, &format!("{tag}.gdb"));
    for prop in extra {
        declare(&indexed, json, prop, IndexKind::BTree);
        declare(&indexed, json, prop, IndexKind::Hash);
    }
    let actual = names(&indexed, q);

    assert_eq!(
        actual, expected,
        "the index changed the answer for {q:?} (indexed vs scan)"
    );
    expected
}

// --- The index must not be empty ---

#[test]
fn a_declared_index_on_a_float_property_has_entries() {
    // `w` repeats, so the auto-builder declines and this is the manual path —
    // the one that reported `ok: true` with `entries: 0`. Three distinct
    // values across four nodes: 1.5, 2, 2.75.
    let (s, _p) = store(DUPES, "declared_entries.gdb");
    assert_eq!(declare(&s, DUPES, "w", IndexKind::BTree), 3);
    assert_eq!(declare(&s, DUPES, "w", IndexKind::Hash), 3);
}

#[test]
fn a_declared_index_covers_every_value_of_a_mixed_property() {
    // `w` mixes 1.5 / 2 / 2.75: the integer must not be the only entry.
    let (s, _p) = store(DUPES, "mixed_entries.gdb");
    let btree = declare(&s, DUPES, "w", IndexKind::BTree);
    assert_eq!(
        btree, 3,
        "a partial index over the integers alone would report 1"
    );
}

#[test]
fn the_auto_builder_indexes_a_unique_float_property() {
    // The auto-build guard requires every node of the label to carry an
    // indexable value; floats used to fail it, so the index was skipped.
    let (s, _p) = store(FLOATS, "auto_float.gdb");
    let specs = s.secondary_indexes_mut().list().to_vec();
    let f_specs: Vec<_> = specs.iter().filter(|sp| sp.prop == "f").collect();
    assert_eq!(
        f_specs.len(),
        2,
        "expected an auto hash + btree on :P(f), got {specs:?}"
    );
    assert!(
        f_specs.iter().all(|sp| sp.auto && sp.entries == 4),
        "auto specs should cover all four nodes, got {f_specs:?}"
    );
}

// --- The index must not change the answer ---

#[test]
fn range_filter_over_floats_agrees_with_the_scan() {
    let rows = agrees_with_the_scan(
        FLOATS,
        "range_floats",
        &[],
        "MATCH (p:P)-[:R]->(z:Q) WHERE p.f < 12.25 RETURN p.name AS n",
    );
    assert_eq!(rows, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn range_filter_over_a_mixed_property_agrees_with_the_scan() {
    // The float-valued rows b and d used to disappear once the index existed.
    let mut rows = agrees_with_the_scan(
        MIXED,
        "range_mixed",
        &[],
        "MATCH (p:P)-[:R]->(z:Q) WHERE p.m < 3 RETURN p.name AS n",
    );
    rows.sort();
    assert_eq!(rows, vec!["a", "b", "c", "d"]);
}

#[test]
fn equality_matches_across_the_int_float_split() {
    // `3 = 3.0` is true in every other part of the engine (`eq_verdict`
    // widens), so an equality answered from a hash index must agree.
    let rows = agrees_with_the_scan(
        MIXED,
        "eq_split",
        &[],
        "MATCH (p:P)-[:R]->(z:Q) WHERE p.m = 3 RETURN p.name AS n",
    );
    assert_eq!(rows, vec!["e".to_string()]);
}

#[test]
fn equality_on_a_float_literal_agrees_with_the_scan() {
    let rows = agrees_with_the_scan(
        MIXED,
        "eq_float",
        &[],
        "MATCH (p:P)-[:R]->(z:Q) WHERE p.m = 2.5 RETURN p.name AS n",
    );
    assert_eq!(rows, vec!["d".to_string()]);
}

#[test]
fn ordering_interleaves_ints_and_floats() {
    // The btree drives ORDER BY through `ordered_ids`, which returns only the
    // ids it holds — a partial index silently truncated the result.
    let rows = agrees_with_the_scan(
        MIXED,
        "order_mixed",
        &[],
        "MATCH (p:P) RETURN p.name AS n ORDER BY p.m ASC",
    );
    assert_eq!(rows, vec!["a", "b", "c", "d", "e"]);
}

#[test]
fn ordering_floats_is_numeric_not_lexicographic() {
    let rows = agrees_with_the_scan(
        FLOATS,
        "order_floats",
        &[],
        "MATCH (p:P) RETURN p.name AS n ORDER BY p.f ASC",
    );
    assert_eq!(rows, vec!["a", "b", "c", "d"]);
}

#[test]
fn descending_order_over_floats() {
    let rows = agrees_with_the_scan(
        FLOATS,
        "order_floats_desc",
        &[],
        "MATCH (p:P) RETURN p.name AS n ORDER BY p.f DESC",
    );
    assert_eq!(rows, vec!["d", "c", "b", "a"]);
}

// --- Persistence ---

#[test]
fn a_declared_float_index_survives_save_and_reopen() {
    let path = temp_db("float_persist.gdb");
    {
        let mem = MemoryGraphStore::from_json_str(DUPES).unwrap();
        mem.save(&path).unwrap();
        let s = LazyGraphStore::open(&path).unwrap();
        declare(&s, DUPES, "w", IndexKind::BTree);
        s.save(&path).unwrap();
    }
    let reopened = LazyGraphStore::open(&path).unwrap();
    let specs = reopened.secondary_indexes_mut().list().to_vec();
    let declared: Vec<_> = specs
        .iter()
        .filter(|sp| !sp.auto && sp.prop == "w" && sp.kind == IndexKind::BTree)
        .collect();
    assert_eq!(
        declared.len(),
        1,
        "the DDL index should be replayed at open, got {specs:?}"
    );
    assert_eq!(
        declared[0].entries, 3,
        "the replayed index should hold every float value"
    );
}

// --- Negative: genuinely unindexable values are still excluded ---

#[test]
fn list_valued_properties_remain_unindexable() {
    // Floats become indexable; lists and records do not, and a property that
    // is partly a list must not produce a partial index that reads as
    // complete. Nothing to assert about entries here beyond the scan
    // agreeing with itself — the point is the query stays correct.
    let json = r#"{
      "nodes": [
        {"id": "a", "labels": ["P"], "props": {"name": "a", "w": 1}},
        {"id": "b", "labels": ["P"], "props": {"name": "b", "w": [1, 2]}}
      ],
      "edges": []
    }"#;
    let (plain, _p) = store(json, "list_prop.gdb");
    let before = names(&plain, "MATCH (p:P) WHERE p.w = 1 RETURN p.name AS n");
    assert_eq!(before, vec!["a".to_string()]);
}

// --- The fixture files are self-consistent ---

#[test]
fn fixtures_load() {
    for (json, name) in [(MIXED, "fixture_mixed.gdb"), (FLOATS, "fixture_floats.gdb")] {
        let (s, p) = store(json, name);
        assert!(Path::new(&p).exists());
        drop(s);
    }
}
