//! Secondary indexes on node properties.
//!
//! Today: hash indexes on (label, prop) pairs whose values are unique within
//! the label, auto-inferred at store open. The LDBC IC workload starts every
//! query with `Person {id = $personId}` (and similar constant-resolved name
//! lookups for Tag/Country/TagClass), all of which become O(1) hash lookups
//! once the index is in place — without these, the LTJ runner would scan
//! every node of the label and read its `id` from the page cache to compare.
//!
//! Roadmap (separate commit):
//! - `CREATE / DROP / SHOW INDEX` DDL so the user can declare hash or btree
//!   indexes on arbitrary (label, prop) pairs.
//! - BTree variant + range lookups for IC2 / IC3 / IC4 / IC9 temporal filters.
//! - Persist declared indexes in the .gdb file header chain (auto-inferred
//!   ones can keep being rebuilt on open).

use std::collections::HashMap;

use crate::model::graph::Graph;
use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, Value};

/// Subset of `Value` that is `Hash + Eq + Ord`. Floats, lists, records, and
/// nulls are deliberately not indexable: floats need `NotNan` wrappers to be
/// `Hash`, and the rest do not have an obvious total order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IndexKey {
    Int(i64),
    Str(String),
    Bool(bool),
}

impl IndexKey {
    pub fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::Int(n) => Some(IndexKey::Int(*n)),
            Value::Str(s) => Some(IndexKey::Str(s.clone())),
            Value::Bool(b) => Some(IndexKey::Bool(*b)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexKind {
    Hash,
}

#[derive(Debug, Clone)]
pub struct IndexSpec {
    pub name: String,
    pub label: String,
    pub prop: String,
    pub kind: IndexKind,
    /// True if auto-inferred at open; false if declared via DDL.
    pub auto: bool,
    /// Number of indexed entries (distinct values).
    pub entries: usize,
}

/// In-memory collection of secondary indexes, keyed by (label, prop).
#[derive(Debug, Default)]
pub struct SecondaryIndex {
    hashes: HashMap<(String, String), HashMap<IndexKey, Vec<Id>>>,
    specs: Vec<IndexSpec>,
}

impl SecondaryIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn list(&self) -> &[IndexSpec] {
        &self.specs
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Equality lookup. Returns Some(matching node IDs) when the (label, prop)
    /// is indexed, None otherwise. The returned vector is empty when the
    /// index exists but no node has that value.
    pub fn lookup_eq(&self, label: &str, prop: &str, value: &Value) -> Option<Vec<Id>> {
        let key = IndexKey::from_value(value)?;
        let bucket = self.hashes.get(&(label.to_string(), prop.to_string()))?;
        Some(bucket.get(&key).cloned().unwrap_or_default())
    }

    /// True when an index exists for the given (label, prop) pair.
    pub fn has(&self, label: &str, prop: &str) -> bool {
        self.hashes
            .contains_key(&(label.to_string(), prop.to_string()))
    }

    /// Auto-build hash indexes for every (label, prop) where the property is
    /// present on every node of that label and all values are distinct.
    /// Single O(N) pass over nodes; works against any GraphAccess.
    pub fn auto_build<G: GraphAccess>(&mut self, store: &G) {
        // (label, prop) → value bucket. Built in one pass.
        let mut per_label_prop: HashMap<(String, String), HashMap<IndexKey, Vec<Id>>> =
            HashMap::new();
        // (label) → node count, used to verify "every node has the prop".
        let mut per_label_count: HashMap<String, usize> = HashMap::new();

        for nid in store.nodes() {
            let labels = store.node_labels(nid);
            let label_strs = Graph::label_strings(&labels);
            let props = store.node_props(nid);
            for label in &label_strs {
                *per_label_count.entry(label.clone()).or_insert(0) += 1;
                for (k, v) in &props {
                    if let Some(idx_k) = IndexKey::from_value(v) {
                        per_label_prop
                            .entry((label.clone(), k.clone()))
                            .or_default()
                            .entry(idx_k)
                            .or_default()
                            .push(nid);
                    }
                }
            }
        }

        // Keep only the (label, prop) entries where every value bucket is a
        // singleton AND the total count equals the label count (rules out
        // properties that are absent on some nodes).
        for ((label, prop), bucket) in per_label_prop {
            let label_count = *per_label_count.get(&label).unwrap_or(&0);
            let total_present: usize = bucket.values().map(|v| v.len()).sum();
            let unique = bucket.values().all(|v| v.len() == 1);
            if unique && total_present == label_count && label_count > 0 {
                let entries = bucket.len();
                let name = format!("{}_{}_auto", label, prop);
                self.specs.push(IndexSpec {
                    name,
                    label: label.clone(),
                    prop: prop.clone(),
                    kind: IndexKind::Hash,
                    auto: true,
                    entries,
                });
                self.hashes.insert((label, prop), bucket);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::graph::Graph;
    use crate::model::value::Value;

    fn small_graph() -> Graph {
        let json = r#"{
            "nodes": [
                {"id":"p1","labels":["Person"],"props":{"id":1,"firstName":"Alice"}},
                {"id":"p2","labels":["Person"],"props":{"id":2,"firstName":"Bob"}},
                {"id":"p3","labels":["Person"],"props":{"id":3,"firstName":"Alice"}},
                {"id":"c1","labels":["Country"],"props":{"id":10,"name":"Chile"}},
                {"id":"c2","labels":["Country"],"props":{"id":11,"name":"Peru"}}
            ],
            "edges": []
        }"#;
        Graph::from_json_str(json).expect("parse")
    }

    #[test]
    fn auto_indexes_unique_props_only() {
        let g = small_graph();
        let mut idx = SecondaryIndex::new();
        idx.auto_build(&g);

        // Person.id is unique → indexed
        assert!(idx.has("Person", "id"));
        // Country.id and Country.name are unique → indexed
        assert!(idx.has("Country", "id"));
        assert!(idx.has("Country", "name"));
        // Person.firstName is NOT unique (Alice repeats) → not indexed
        assert!(!idx.has("Person", "firstName"));
    }

    #[test]
    fn lookup_eq_returns_node_id() {
        let g = small_graph();
        let mut idx = SecondaryIndex::new();
        idx.auto_build(&g);

        let hits = idx.lookup_eq("Person", "id", &Value::Int(2)).unwrap();
        assert_eq!(hits.len(), 1);
        // The node id is internal — we can't assert a specific value, only that
        // there is exactly one and it's reachable.

        let miss = idx.lookup_eq("Person", "id", &Value::Int(999)).unwrap();
        assert!(miss.is_empty());

        // Non-indexed prop returns None (caller must fall back).
        assert!(idx.lookup_eq("Person", "firstName", &Value::Str("Alice".into())).is_none());
    }
}
