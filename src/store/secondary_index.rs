//! Secondary indexes on node properties.
//!
//! Two kinds:
//! - **Hash** for equality (`x.attr = literal`). Auto-inferred at store open
//!   for `(label, prop)` pairs whose values are unique within the label —
//!   captures the LDBC IC start lookups (`Person.id`, `Tag.name`, etc.)
//!   without any DDL.
//! - **BTree** for range filters (`<`, `<=`, `>`, `>=`). Declared by the user
//!   via `CREATE BTREE INDEX foo ON :Label(prop)`. Targets the LDBC IC2/3/4/9
//!   `Message.creationDate <= $maxDate` style predicates.
//!
//! Both kinds are in-memory (rebuilt every open). Persistence in the .gdb
//! file header chain is on the roadmap.

use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;

use crate::model::graph::MemoryGraphStore;
use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, Value};

/// Order-preserving `u64` image of an `f64`: flip the sign bit for
/// non-negatives, invert every bit for negatives. The natural `u64` order of
/// the result is the float order, which is what lets a `BTreeMap` range-scan
/// floats. NaN lands above `+inf` (Postgres orders it greatest too) and every
/// NaN payload is normalized to one bit pattern so a key is deterministic.
fn encode_f64(f: f64) -> u64 {
    let bits = if f.is_nan() {
        f64::NAN.to_bits()
    } else {
        f.to_bits()
    };
    if bits & (1 << 63) != 0 {
        !bits
    } else {
        bits ^ (1 << 63)
    }
}

fn decode_f64(bits: u64) -> f64 {
    let raw = if bits & (1 << 63) != 0 {
        bits ^ (1 << 63)
    } else {
        !bits
    };
    f64::from_bits(raw)
}

/// Subset of `Value` a secondary index can key on. Lists, records and nulls
/// stay out: they have no obvious total order, and a null property is absent
/// from the record anyway, so excluding it is what the query semantics want.
///
/// Numbers are one domain, not two (issue #96). A float whose value is an
/// exact integer is stored as `Int`, so `3` and `3.0` are the *same* key —
/// which they must be, since `3 = 3.0` is true everywhere else in the engine
/// (`eq_verdict`, `cmp_values`). Everything else becomes `Float`, and `Ord`
/// compares an `Int` against a `Float` by widening to `f64`, exactly as
/// `cmp_values` does. That agreement is the point: whether a predicate is
/// answered from an index or from a scan must not change the answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IndexKey {
    Int(i64),
    /// A non-integral (or out-of-`i64`-range) float, held as its
    /// order-preserving bit image so the key is `Hash + Eq + Ord`.
    Float(u64),
    Str(String),
    Bool(bool),
}

impl IndexKey {
    pub fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::Int(n) => Some(IndexKey::Int(*n)),
            Value::Float(f) => Some(IndexKey::from_f64(*f)),
            Value::Str(s) => Some(IndexKey::Str(s.clone())),
            Value::Bool(b) => Some(IndexKey::Bool(*b)),
            _ => None,
        }
    }

    /// Canonicalize a float: an exact integer within `i64` becomes `Int` so it
    /// shares its key with the integer of the same value. `-0.0` normalizes to
    /// `Int(0)` by the same rule.
    fn from_f64(f: f64) -> Self {
        // `i64::MAX as f64` rounds *up* to 2^63, so compare against the power
        // of two directly rather than the rounded bound.
        const LIMIT: f64 = 9_223_372_036_854_775_808.0; // 2^63
        if f.is_finite() && f.fract() == 0.0 && (-LIMIT..LIMIT).contains(&f) {
            IndexKey::Int(f as i64)
        } else {
            IndexKey::Float(encode_f64(f))
        }
    }

    /// Where this key sits among the key categories, so unrelated types keep a
    /// stable total order in one `BTreeMap`. Numbers share a rank because they
    /// share a domain.
    fn rank(&self) -> u8 {
        match self {
            IndexKey::Int(_) | IndexKey::Float(_) => 0,
            IndexKey::Str(_) => 1,
            IndexKey::Bool(_) => 2,
        }
    }
}

impl Ord for IndexKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (IndexKey::Int(a), IndexKey::Int(b)) => a.cmp(b),
            // Both are order-preserving images, so the raw `u64` order is the
            // float order.
            (IndexKey::Float(a), IndexKey::Float(b)) => a.cmp(b),
            // A `Float` is never integral here, so widening the `Int` cannot
            // make them compare equal; the same widening `cmp_values` applies.
            (IndexKey::Int(a), IndexKey::Float(b)) => (*a as f64)
                .partial_cmp(&decode_f64(*b))
                .unwrap_or(Ordering::Less), // NaN sorts last
            (IndexKey::Float(a), IndexKey::Int(b)) => decode_f64(*a)
                .partial_cmp(&(*b as f64))
                .unwrap_or(Ordering::Greater),
            (IndexKey::Str(a), IndexKey::Str(b)) => a.cmp(b),
            (IndexKey::Bool(a), IndexKey::Bool(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for IndexKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Convert a `Bound<Value>` to a `Bound<IndexKey>` for BTree range queries.
/// Returns None if the bound's value is not indexable (a list, record or null).
fn bound_to_key(b: Bound<Value>) -> Option<Bound<IndexKey>> {
    match b {
        Bound::Included(v) => Some(Bound::Included(IndexKey::from_value(&v)?)),
        Bound::Excluded(v) => Some(Bound::Excluded(IndexKey::from_value(&v)?)),
        Bound::Unbounded => Some(Bound::Unbounded),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexKind {
    Hash,
    BTree,
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

/// The node ids carrying one indexed value.
///
/// Most postings hold exactly one id, and on the index that matters most
/// — the auto-built one — *every* posting does, because the auto-builder
/// only indexes a `(label, prop)` whose values are unique within the
/// label. A `Vec<Id>` for that costs 24 bytes of header plus a separate
/// heap block the allocator rounds up to 32: 56 bytes to carry four, once
/// per node, in two structures. At 160 M nodes that is ~9.6 GiB of
/// allocator padding around 1.2 GiB of ids, spread over 320 M tiny
/// allocations.
///
/// `One` stores the id where the `Vec` header would have gone. The whole
/// enum is 16 bytes and touches the heap only when a value genuinely
/// repeats, which is the case a declared index on a non-unique column
/// still has to serve.
///
/// `as_slice` is what makes the substitution invisible to callers:
/// `slice::from_ref` gives a one-element slice over the inline id without
/// allocating, so every reader keeps the `&[Id]` it had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Posting {
    One(Id),
    /// Boxed so the enum stays 16 bytes rather than 32: an inline `Vec`
    /// would put its three-word header in every `One` as well, which is
    /// the cost this type exists to avoid.
    Many(Box<Vec<Id>>),
}

impl Posting {
    pub fn as_slice(&self) -> &[Id] {
        match self {
            Posting::One(id) => std::slice::from_ref(id),
            Posting::Many(v) => v,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Posting::One(_) => 1,
            Posting::Many(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Add an id, promoting to the heap on the second one.
    pub fn push(&mut self, id: Id) {
        match self {
            Posting::One(first) => {
                *self = Posting::Many(Box::new(vec![*first, id]));
            }
            Posting::Many(v) => v.push(id),
        }
    }

    /// Resident bytes: the enum itself plus whatever it owns.
    fn heap_bytes(&self) -> usize {
        std::mem::size_of::<Posting>()
            + match self {
                Posting::One(_) => 0,
                // The `Vec` header lives behind the box, so both it and
                // the buffer are heap.
                Posting::Many(v) => 24 + v.capacity() * 4 + 16,
            }
    }
}

/// A map the posting builders can write into. Implemented for both the
/// hash and btree bucket types so the promotion rule has one definition
/// rather than one per builder.
trait PostingMap {
    fn get_posting_mut(&mut self, key: &IndexKey) -> Option<&mut Posting>;
    fn insert_posting(&mut self, key: IndexKey, value: Posting);
}

impl PostingMap for HashMap<IndexKey, Posting> {
    fn get_posting_mut(&mut self, key: &IndexKey) -> Option<&mut Posting> {
        self.get_mut(key)
    }
    fn insert_posting(&mut self, key: IndexKey, value: Posting) {
        self.insert(key, value);
    }
}

impl PostingMap for BTreeMap<IndexKey, Posting> {
    fn get_posting_mut(&mut self, key: &IndexKey) -> Option<&mut Posting> {
        self.get_mut(key)
    }
    fn insert_posting(&mut self, key: IndexKey, value: Posting) {
        self.insert(key, value);
    }
}

/// Insert `id` under `key`, starting a new single-id posting when absent.
fn add_posting<M: PostingMap>(bucket: &mut M, key: IndexKey, id: Id) {
    match bucket.get_posting_mut(&key) {
        Some(p) => p.push(id),
        None => bucket.insert_posting(key, Posting::One(id)),
    }
}

/// Which kinds the auto-builder produces, from `FROGQL_AUTO_INDEX_KINDS`.
///
/// Accepts `both` (the default), `hash`, `btree`, or `none`. The two
/// kinds answer different questions and cost the same to hold, so which
/// of them earns its memory depends on the workload rather than on the
/// data:
///
/// | kind  | serves                                    |
/// |-------|-------------------------------------------|
/// | hash  | `x.p = v` (`lookup_eq`)                   |
/// | btree | `x.p < v`, `ORDER BY x.p` (`lookup_range`, `ordered_ids`) |
///
/// A workload of pure equality predicates pays for the btree and never
/// reads it. That is invisible at LDBC SF0.1, where the pair costs a few
/// MiB; on a 160 M-node dump the btree is ~15 GiB, which is the
/// difference between opening the database and being killed by the OOM
/// reaper. `--no-auto-indexes` already existed but is all-or-nothing, and
/// dropping the hash as well turns every `x.id = v` into a full scan.
///
/// Read once per process, like every other kill switch here: which
/// indexes exist is a property of the open database, not of a statement.
pub fn auto_index_kinds() -> (bool, bool) {
    static KINDS: std::sync::OnceLock<(bool, bool)> = std::sync::OnceLock::new();
    *KINDS.get_or_init(|| {
        match std::env::var("FROGQL_AUTO_INDEX_KINDS")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "" | "both" | "all" => (true, true),
            "hash" => (true, false),
            "btree" => (false, true),
            "none" => (false, false),
            other => {
                eprintln!(
                    "warning: FROGQL_AUTO_INDEX_KINDS=`{other}` is not one of \
                     both|hash|btree|none; building both"
                );
                (true, true)
            }
        }
    })
}

/// In-memory collection of secondary indexes, keyed by (label, prop).
#[derive(Debug, Default)]
pub struct SecondaryIndex {
    hashes: HashMap<(String, String), HashMap<IndexKey, Posting>>,
    btrees: HashMap<(String, String), BTreeMap<IndexKey, Posting>>,
    specs: Vec<IndexSpec>,
}

impl SecondaryIndex {
    /// Resident bytes across every hash and btree entry. Keys are counted
    /// by their own size; the posting lists dominate on unique-valued
    /// columns, where each key maps to a single id.
    pub fn heap_bytes(&self) -> usize {
        let key = std::mem::size_of::<IndexKey>();
        let mut total = 0usize;
        for ((a, b), m) in &self.hashes {
            total += a.capacity() + b.capacity();
            total += m.values().map(|v| key + v.heap_bytes()).sum::<usize>();
        }
        for ((a, b), m) in &self.btrees {
            total += a.capacity() + b.capacity();
            total += m.values().map(|v| key + v.heap_bytes()).sum::<usize>();
        }
        total
    }

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
    /// is indexed (hash or btree both support point lookup), None otherwise.
    /// The returned vector is empty when the index exists but no node has
    /// that value.
    pub fn lookup_eq(&self, label: &str, prop: &str, value: &Value) -> Option<Vec<Id>> {
        let key = IndexKey::from_value(value)?;
        let lp = (label.to_string(), prop.to_string());
        if let Some(bucket) = self.hashes.get(&lp) {
            return Some(
                bucket
                    .get(&key)
                    .map(|p| p.as_slice().to_vec())
                    .unwrap_or_default(),
            );
        }
        if let Some(bucket) = self.btrees.get(&lp) {
            return Some(
                bucket
                    .get(&key)
                    .map(|p| p.as_slice().to_vec())
                    .unwrap_or_default(),
            );
        }
        None
    }

    /// Range lookup. Walks the BTree for `(label, prop)` between `lo` and
    /// `hi` and returns the union of matching node IDs. Returns None when
    /// no btree exists for this (label, prop) — caller must fall back to a
    /// scan + filter.
    pub fn lookup_range(
        &self,
        label: &str,
        prop: &str,
        lo: Bound<Value>,
        hi: Bound<Value>,
    ) -> Option<Vec<Id>> {
        let bucket = self.btrees.get(&(label.to_string(), prop.to_string()))?;
        let lo_k = bound_to_key(lo)?;
        let hi_k = bound_to_key(hi)?;
        let mut out = Vec::new();
        for (_, ids) in bucket.range((lo_k, hi_k)) {
            out.extend_from_slice(ids.as_slice());
        }
        Some(out)
    }

    /// True when a btree index exists for `(label, prop)`. Used by the
    /// BTree-driven ORDER BY path to detect whether it can route through
    /// `ordered_ids` instead of running a generic sort.
    pub fn has_btree(&self, label: &str, prop: &str) -> bool {
        self.btrees
            .contains_key(&(label.to_string(), prop.to_string()))
    }

    /// All node IDs carrying `(label, prop)` in btree-key order.
    /// `ascending = true` walks ASC, `false` walks DESC. None means no
    /// btree exists — caller must fall back to a generic sort.
    pub fn ordered_ids(&self, label: &str, prop: &str, ascending: bool) -> Option<Vec<Id>> {
        let bucket = self.btrees.get(&(label.to_string(), prop.to_string()))?;
        let mut out = Vec::with_capacity(bucket.values().map(|v| v.len()).sum());
        if ascending {
            for ids in bucket.values() {
                out.extend_from_slice(ids.as_slice());
            }
        } else {
            for ids in bucket.values().rev() {
                out.extend_from_slice(ids.as_slice());
            }
        }
        Some(out)
    }

    /// True when *any* index (hash or btree) exists for the given
    /// (label, prop) pair.
    pub fn has(&self, label: &str, prop: &str) -> bool {
        let lp = (label.to_string(), prop.to_string());
        self.hashes.contains_key(&lp) || self.btrees.contains_key(&lp)
    }

    /// Build a declared (DDL) hash or btree index. Scans every node carrying
    /// `label`. HASH and BTREE coexist on the same (label, prop) — they serve
    /// different query patterns (equality vs range) and the LTJ optimizer
    /// picks the right one per filter. Re-declaring the same kind is the
    /// only conflict.
    pub fn build_declared<G: GraphAccess>(
        &mut self,
        store: &G,
        name: String,
        label: &str,
        prop: &str,
        kind: IndexKind,
    ) -> Result<IndexSpec, String> {
        let lp = (label.to_string(), prop.to_string());
        let already_same_kind = match kind {
            IndexKind::Hash => self.hashes.contains_key(&lp),
            IndexKind::BTree => self.btrees.contains_key(&lp),
        };
        if already_same_kind {
            return Err(format!(
                "a {:?} index already exists on (:{label} {{{prop}}}); drop it first",
                kind
            ));
        }
        // Use the label index when available; else fall back to a full scan
        // and skip nodes that don't carry the requested label.
        let candidates: Vec<Id> = match store.nodes_with_label(label) {
            Some(v) => v,
            None => store
                .nodes()
                .into_iter()
                .filter(|&nid| {
                    MemoryGraphStore::label_strings(&store.node_labels(nid))
                        .iter()
                        .any(|l| l == label)
                })
                .collect(),
        };

        match kind {
            IndexKind::Hash => {
                let mut bucket: HashMap<IndexKey, Posting> = HashMap::new();
                for nid in candidates {
                    let props = store.node_props(nid);
                    if let Some(v) = props.get(prop) {
                        if let Some(k) = IndexKey::from_value(v) {
                            add_posting(&mut bucket, k, nid);
                        }
                    }
                }
                let entries = bucket.len();
                let spec = IndexSpec {
                    name: name.clone(),
                    label: lp.0.clone(),
                    prop: lp.1.clone(),
                    kind,
                    auto: false,
                    entries,
                };
                self.hashes.insert(lp, bucket);
                self.specs.push(spec.clone());
                Ok(spec)
            }
            IndexKind::BTree => {
                let mut bucket: BTreeMap<IndexKey, Posting> = BTreeMap::new();
                for nid in candidates {
                    let props = store.node_props(nid);
                    if let Some(v) = props.get(prop) {
                        if let Some(k) = IndexKey::from_value(v) {
                            add_posting(&mut bucket, k, nid);
                        }
                    }
                }
                let entries = bucket.len();
                let spec = IndexSpec {
                    name: name.clone(),
                    label: lp.0.clone(),
                    prop: lp.1.clone(),
                    kind,
                    auto: false,
                    entries,
                };
                self.btrees.insert(lp, bucket);
                self.specs.push(spec.clone());
                Ok(spec)
            }
        }
    }

    /// Inject a pre-built bucket without rescanning the store. Used by the
    /// fast bulk-build path on `LazyGraphStore` so a single pass over the
    /// node records can populate both the hash and btree variants without
    /// duplicating the read+decode work.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_prebuilt(
        &mut self,
        label: &str,
        prop: &str,
        kind: IndexKind,
        auto: bool,
        entries: usize,
        hash_bucket: Option<HashMap<IndexKey, Posting>>,
        btree_bucket: Option<BTreeMap<IndexKey, Posting>>,
    ) {
        let lp = (label.to_string(), prop.to_string());
        let suffix = match kind {
            IndexKind::Hash => "auto_hash",
            IndexKind::BTree => "auto_btree",
        };
        let name = if auto {
            format!("{label}_{prop}_{suffix}")
        } else {
            format!("{label}_{prop}_declared")
        };
        match kind {
            IndexKind::Hash => {
                if let Some(b) = hash_bucket {
                    self.hashes.insert(lp.clone(), b);
                }
            }
            IndexKind::BTree => {
                if let Some(b) = btree_bucket {
                    self.btrees.insert(lp.clone(), b);
                }
            }
        }
        self.specs.push(IndexSpec {
            name,
            label: lp.0,
            prop: lp.1,
            kind,
            auto,
            entries,
        });
    }

    /// Drop an index by its declared (or auto-generated) name. Returns true
    /// if the index existed and was removed, false otherwise.
    pub fn drop_named(&mut self, name: &str) -> bool {
        let Some(idx) = self.specs.iter().position(|s| s.name == name) else {
            return false;
        };
        let spec = self.specs.remove(idx);
        let lp = (spec.label, spec.prop);
        match spec.kind {
            IndexKind::Hash => {
                self.hashes.remove(&lp);
            }
            IndexKind::BTree => {
                self.btrees.remove(&lp);
            }
        }
        true
    }

    /// Auto-build hash indexes for every (label, prop) where the property is
    /// present on every node of that label and all values are distinct.
    /// Single O(N) pass over nodes; works against any GraphAccess.
    pub fn auto_build<G: GraphAccess>(&mut self, store: &G) {
        // (label, prop) → value bucket. Built in one pass.
        let mut per_label_prop: HashMap<(String, String), HashMap<IndexKey, Posting>> =
            HashMap::new();
        // (label) → node count, used to verify "every node has the prop".
        let mut per_label_count: HashMap<String, usize> = HashMap::new();

        for nid in store.nodes() {
            let labels = store.node_labels(nid);
            let label_strs = MemoryGraphStore::label_strings(&labels);
            let props = store.node_props(nid);
            for label in &label_strs {
                *per_label_count.entry(label.clone()).or_insert(0) += 1;
                for (k, v) in &props {
                    if let Some(idx_k) = IndexKey::from_value(v) {
                        add_posting(
                            per_label_prop
                                .entry((label.clone(), k.clone()))
                                .or_default(),
                            idx_k,
                            nid,
                        );
                    }
                }
            }
        }

        // Keep only the (label, prop) entries where every value bucket is a
        // singleton AND the total count equals the label count (rules out
        // properties that are absent on some nodes). Build BOTH hash and
        // btree for these — hash for `=` (1 lookup), btree for `<`/`<=`/
        // `>`/`>=` range queries (LDBC IC2/3/4/9 temporal filters). Memory
        // overhead is bounded because we only do this for unique-valued
        // columns; the BTree on Comment.creationDate at SF0.1 is ~3MB.
        for ((label, prop), bucket) in per_label_prop {
            let label_count = *per_label_count.get(&label).unwrap_or(&0);
            let total_present: usize = bucket.values().map(|v| v.len()).sum();
            let unique = bucket.values().all(|v| v.len() == 1);
            if unique && total_present == label_count && label_count > 0 {
                let entries = bucket.len();
                let (want_hash, want_btree) = auto_index_kinds();
                // Hash auto-index.
                if want_hash {
                    let hash_name = format!("{}_{}_auto_hash", label, prop);
                    self.specs.push(IndexSpec {
                        name: hash_name,
                        label: label.clone(),
                        prop: prop.clone(),
                        kind: IndexKind::Hash,
                        auto: true,
                        entries,
                    });
                }
                // Mirror the same buckets into a BTree so range filters can
                // skip the per-row property read. Same NodeIds, different
                // structure — a meaningful speedup on temporal range
                // predicates, and a full second copy of the postings, which
                // is why it is declinable.
                if want_btree {
                    let btree: BTreeMap<IndexKey, Posting> =
                        bucket.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    let btree_name = format!("{}_{}_auto_btree", label, prop);
                    self.specs.push(IndexSpec {
                        name: btree_name,
                        label: label.clone(),
                        prop: prop.clone(),
                        kind: IndexKind::BTree,
                        auto: true,
                        entries,
                    });
                    self.btrees.insert((label.clone(), prop.clone()), btree);
                }
                if want_hash {
                    self.hashes.insert((label, prop), bucket);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::graph::MemoryGraphStore;
    use crate::model::value::Value;

    fn small_graph() -> MemoryGraphStore {
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
        MemoryGraphStore::from_json_str(json).expect("parse")
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
        assert!(idx
            .lookup_eq("Person", "firstName", &Value::Str("Alice".into()))
            .is_none());
    }
}
