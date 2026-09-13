//! The secondary index must survive a mutation — correctly, then quickly.
//!
//! The base `SecondaryIndex` is built from the on-disk records at open and
//! never updated, so anything staged in the overlay makes it lie. Three
//! separate bugs came out of that single fact, and all three were silent:
//!
//! 1. `lookup_node_eq` / `lookup_node_range` declined the index once a node
//!    was inserted or deleted, which was *correct* and sent every later
//!    lookup of the session to a full label scan with a per-node property
//!    decode — permanently, since the overlay lives until `.save`. That is
//!    what made loading a graph by `INSERT` quadratic.
//! 2. Neither guard mentioned `mod_node_props`, so after
//!    `SET a.k = 'new'` a lookup of `'new'` returned **nothing** while the
//!    node sat right there — and inserting any unrelated node made it
//!    appear, because that tripped the other half of the guard.
//! 3. `lookup_node_ordered` had no guard at all, so a btree-driven
//!    `ORDER BY … LIMIT k` silently dropped every node inserted this
//!    session.
//!
//! The fix keeps the base index and maintains a delta beside it
//! (`store::overlay_index`), merged at read time. So the oracle here is a
//! plain scan of the merged view: whatever the index answers must equal
//! what looking at every node would have answered. `FROGQL_DISABLE_OVERLAY_
//! INDEX=1` is the baseline the differential cases A/B against — it
//! restores the decline-and-scan behaviour, which is correct and slow, so
//! the two must agree on every answer.

use std::collections::BTreeSet;
use std::path::PathBuf;

use frogql::model::graph::MemoryGraphStore;
use frogql::model::graph_access::{GraphAccess, GraphAccessMut};
use frogql::model::value::{Id, Value};
use frogql::store::lazy::LazyGraphStore;
use frogql::typing::label_type::LabelType;

const NODES: i64 = 200;

fn fixture_json() -> String {
    let nodes: Vec<String> = (0..NODES)
        .map(|i| {
            format!(
                r#"{{"id":"n{i}","labels":["P"],"props":{{"k":{i},"name":"v{i}"}}}}"#,
                i = i
            )
        })
        .collect();
    format!(r#"{{"nodes":[{}],"edges":[]}}"#, nodes.join(","))
}

fn open_db(name: &str) -> (LazyGraphStore, PathBuf) {
    let dir = std::env::temp_dir().join(format!("frogql_overlay_idx_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("t.gdb");
    MemoryGraphStore::from_json_str(&fixture_json())
        .unwrap()
        .save(&db)
        .unwrap();
    let store = LazyGraphStore::open(&db).unwrap();
    (store, db)
}

fn props_with(pairs: &[(&str, Value)]) -> frogql::model::graph::Props {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// Every live `P` node whose `prop` equals `value`, by scanning. The oracle.
fn scan_eq(store: &LazyGraphStore, prop: &str, value: &Value) -> BTreeSet<Id> {
    store
        .nodes()
        .into_iter()
        .filter(|&nid| {
            MemoryGraphStore::label_strings(&store.node_labels(nid))
                .iter()
                .any(|l| l == "P")
                && store.node_props(nid).get(prop) == Some(value)
        })
        .collect()
}

fn indexed_eq(store: &LazyGraphStore, prop: &str, value: &Value) -> Option<BTreeSet<Id>> {
    store
        .lookup_node_eq("P", prop, value)
        .map(|v| v.into_iter().collect())
}

/// The index must agree with the scan, and must actually have answered —
/// a `None` here means the delta gave up and the speed win is gone.
fn assert_index_matches_scan(store: &LazyGraphStore, prop: &str, value: &Value) {
    let scanned = scan_eq(store, prop, value);
    let indexed = indexed_eq(store, prop, value)
        .unwrap_or_else(|| panic!("index declined {prop} = {value:?}; the delta should serve it"));
    assert_eq!(indexed, scanned, "index vs scan for {prop} = {value:?}");
}

// ---------------------------------------------------------------------------
// The three bugs
// ---------------------------------------------------------------------------

/// Bug 1, the reported one: a lookup after an *unrelated* insert must still
/// come from the index, not from a scan.
#[test]
fn insert_does_not_disable_the_index() {
    let (store, _p) = open_db("insert_keeps_index");
    assert_index_matches_scan(&store, "k", &Value::Int(7));

    store.insert_node(
        LabelType::Label("Unrelated".into()),
        props_with(&[("n", Value::Int(1))]),
    );

    assert_index_matches_scan(&store, "k", &Value::Int(7));
}

/// A node inserted this session is findable through the index.
#[test]
fn inserted_node_is_indexed() {
    let (store, _p) = open_db("inserted_found");
    let id = store.insert_node(
        LabelType::Label("P".into()),
        props_with(&[("k", Value::Int(9999)), ("name", Value::Str("new".into()))]),
    );

    assert_index_matches_scan(&store, "k", &Value::Int(9999));
    assert!(indexed_eq(&store, "k", &Value::Int(9999))
        .unwrap()
        .contains(&id));
}

/// Bug 2: after `SET`, the *new* value must be findable and the old one
/// must not. This returned zero rows before the delta existed.
#[test]
fn set_moves_a_node_to_its_new_key() {
    let (store, _p) = open_db("set_moves");
    let target = indexed_eq(&store, "k", &Value::Int(5))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    store.set_node_prop(target, "k", Value::Int(50_000));

    assert_index_matches_scan(&store, "k", &Value::Int(50_000));
    assert!(indexed_eq(&store, "k", &Value::Int(50_000))
        .unwrap()
        .contains(&target));
    assert_index_matches_scan(&store, "k", &Value::Int(5));
    assert!(indexed_eq(&store, "k", &Value::Int(5)).unwrap().is_empty());
}

/// A touched node must keep answering for the properties the mutation did
/// *not* change. Shadowing drops it from every base answer, so the delta
/// has to re-file all of its indexed pairs, not only the mutated one.
#[test]
fn a_touched_node_still_answers_for_its_other_properties() {
    let (store, _p) = open_db("touch_other_props");
    let target = indexed_eq(&store, "k", &Value::Int(11))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    store.set_node_prop(target, "k", Value::Int(70_000));

    // `name` was never touched, and must still resolve to this node.
    assert_index_matches_scan(&store, "name", &Value::Str("v11".into()));
    assert!(indexed_eq(&store, "name", &Value::Str("v11".into()))
        .unwrap()
        .contains(&target));
}

/// Bug 3: a btree-driven ORDER BY must see a node inserted this session,
/// in its correct position. `ordered_ids` used to skip the overlay whole.
#[test]
fn ordered_ids_include_inserted_nodes_in_key_order() {
    let (store, _p) = open_db("ordered_includes_new");
    let id = store.insert_node(
        LabelType::Label("P".into()),
        props_with(&[("k", Value::Int(-1))]),
    );

    let asc = store.lookup_node_ordered("P", "k", true).unwrap();
    assert_eq!(asc.first(), Some(&id), "k = -1 is the minimum");

    let desc = store.lookup_node_ordered("P", "k", false).unwrap();
    assert_eq!(desc.last(), Some(&id), "and the maximum from the other end");
    assert_eq!(asc.len(), desc.len());
    assert_eq!(
        asc.iter().copied().collect::<BTreeSet<_>>(),
        desc.iter().copied().collect::<BTreeSet<_>>()
    );
}

/// The ordered merge must interleave, not concatenate: a value between two
/// base keys has to land between them.
#[test]
fn ordered_ids_interleave_delta_with_base() {
    let (store, _p) = open_db("ordered_interleaves");
    let base_asc = store.lookup_node_ordered("P", "k", true).unwrap();
    let at_3 = base_asc[3];
    let at_4 = base_asc[4];

    // `k` runs 0..NODES, so 3.5 sits strictly between the fourth and fifth.
    let mid = store.insert_node(
        LabelType::Label("P".into()),
        props_with(&[("k", Value::Float(3.5))]),
    );

    let asc = store.lookup_node_ordered("P", "k", true).unwrap();
    let pos = |x: Id| asc.iter().position(|&y| y == x).unwrap();
    assert!(
        pos(at_3) < pos(mid) && pos(mid) < pos(at_4),
        "3.5 must sort between 3 and 4, got {asc:?}"
    );
}

/// A deleted node leaves every index answer.
#[test]
fn deleted_node_leaves_the_index() {
    let (store, _p) = open_db("delete_leaves");
    let target = indexed_eq(&store, "k", &Value::Int(3))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    store.detach_delete_node(target);

    assert_index_matches_scan(&store, "k", &Value::Int(3));
    assert!(indexed_eq(&store, "k", &Value::Int(3)).unwrap().is_empty());
    assert!(!store
        .lookup_node_ordered("P", "k", true)
        .unwrap()
        .contains(&target));
}

/// Removing the label takes the node out of that label's index; adding it
/// to a fresh node puts it in.
#[test]
fn label_mutation_moves_a_node_between_indexes() {
    let (store, _p) = open_db("label_moves");
    let target = indexed_eq(&store, "k", &Value::Int(2))
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    store.remove_node_label(target, "P");

    assert_index_matches_scan(&store, "k", &Value::Int(2));
    assert!(indexed_eq(&store, "k", &Value::Int(2)).unwrap().is_empty());
}

/// A range lookup sees the overlay too.
#[test]
fn range_lookup_sees_the_overlay() {
    use std::ops::Bound;
    let (store, _p) = open_db("range_sees_overlay");
    let id = store.insert_node(
        LabelType::Label("P".into()),
        props_with(&[("k", Value::Int(100_000))]),
    );

    let hits = store
        .lookup_node_range(
            "P",
            "k",
            Bound::Included(Value::Int(99_000)),
            Bound::Unbounded,
        )
        .unwrap();
    assert_eq!(hits, vec![id]);
}

// ---------------------------------------------------------------------------
// Differential against the kill switch
// ---------------------------------------------------------------------------

/// The delta is an optimisation, so it must not change an answer. The kill
/// switch is read per process, so this case is the child of its own run.
#[test]
fn delta_agrees_with_the_scan_baseline() {
    if std::env::var("FROGQL_OVERLAY_IDX_CHILD").is_ok() {
        let disabled = std::env::var("FROGQL_DISABLE_OVERLAY_INDEX").is_ok_and(|v| v == "1");
        let (store, _p) = open_db(if disabled { "diff_off" } else { "diff_on" });

        // A mixed workload: insert, mutate, delete, relabel.
        store.insert_node(
            LabelType::Label("P".into()),
            props_with(&[("k", Value::Int(1000))]),
        );
        let a = scan_eq(&store, "k", &Value::Int(20))
            .into_iter()
            .next()
            .unwrap();
        store.set_node_prop(a, "k", Value::Int(2000));
        let b = scan_eq(&store, "k", &Value::Int(21))
            .into_iter()
            .next()
            .unwrap();
        store.detach_delete_node(b);
        let c = scan_eq(&store, "k", &Value::Int(22))
            .into_iter()
            .next()
            .unwrap();
        store.remove_node_label(c, "P");

        let mut report = String::new();
        for probe in [20i64, 21, 22, 23, 1000, 2000] {
            let v = Value::Int(probe);
            // Whatever the store answers — index or `None` — must equal the
            // scan. `None` means "caller scans", so the scan is the answer.
            let answer = indexed_eq(&store, "k", &v).unwrap_or_else(|| scan_eq(&store, "k", &v));
            assert_eq!(answer, scan_eq(&store, "k", &v), "probe k = {probe}");
            report.push_str(&format!("{probe}:{answer:?} "));
        }
        println!("REPORT {report}");
        return;
    }

    let run = |disabled: bool| -> String {
        let exe = std::env::current_exe().unwrap();
        let mut cmd = std::process::Command::new(exe);
        cmd.args([
            "delta_agrees_with_the_scan_baseline",
            "--exact",
            "--nocapture",
        ])
        .env("FROGQL_OVERLAY_IDX_CHILD", "1");
        if disabled {
            cmd.env("FROGQL_DISABLE_OVERLAY_INDEX", "1");
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "child failed (disabled={disabled}): {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| l.starts_with("REPORT "))
            .expect("child printed no REPORT")
            .to_string()
    };

    assert_eq!(
        run(false),
        run(true),
        "the overlay index delta changed an answer"
    );
}
