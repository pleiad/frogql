//! In-memory view of the vector sidecars attached to a database.
//!
//! A `VectorSet` is one vector attribute: the rows, their graph-internal
//! node ids, the metric, precomputed row norms, and optionally the HNSW
//! proximity graph. It is immutable after load, which is why
//! `LazyGraphStore` can hold it as a plain field and hand out `&`
//! references from `&self` methods without a `RefCell` in the way.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;

use crate::model::value::Id;
use crate::vector::cursor::{BruteForceCursor, EmptyCursor, NnCursor};
use crate::vector::hnsw::Hnsw;
use crate::vector::metric::{norm, Metric};
use crate::vector::sidecar::Sidecar;

/// One vector attribute.
#[derive(Debug, Clone)]
pub struct VectorSet {
    attr: String,
    dim: usize,
    metric: Metric,
    fingerprint: u64,
    /// Graph-internal node ids, ascending. Row `i` belongs to `ids[i]`.
    ids: Vec<u32>,
    /// Row-major `count × dim`.
    data: Vec<f32>,
    /// L2 norm per row, derived at load. Only `Cosine` reads it, but it
    /// costs one O(n·d) pass and removes a square root from the inner
    /// comparison loop.
    norms: Vec<f32>,
    hnsw: Option<Hnsw>,
}

impl VectorSet {
    /// Build from raw rows. `ids` must be strictly ascending and `data`
    /// row-major `ids.len() × dim`; the sidecar decoder enforces both, so
    /// this is debug-asserted rather than validated.
    pub fn new(
        attr: String,
        dim: usize,
        metric: Metric,
        fingerprint: u64,
        ids: Vec<u32>,
        data: Vec<f32>,
    ) -> VectorSet {
        debug_assert_eq!(data.len(), ids.len() * dim, "row-major shape mismatch");
        debug_assert!(ids.windows(2).all(|w| w[0] < w[1]), "ids must ascend");
        let norms = if metric.needs_norms() {
            (0..ids.len())
                .map(|i| norm(&data[i * dim..(i + 1) * dim]))
                .collect()
        } else {
            Vec::new()
        };
        VectorSet {
            attr,
            dim,
            metric,
            fingerprint,
            ids,
            data,
            norms,
            hnsw: None,
        }
    }

    pub fn with_hnsw(mut self, hnsw: Hnsw) -> VectorSet {
        self.hnsw = Some(hnsw);
        self
    }

    pub fn from_sidecar(s: Sidecar) -> VectorSet {
        let hnsw = s.hnsw;
        let set = VectorSet::new(s.attr, s.dim, s.metric, s.fingerprint, s.ids, s.data);
        match hnsw {
            Some(h) => set.with_hnsw(h),
            None => set,
        }
    }

    /// The persistable form of this set.
    pub fn to_sidecar(&self) -> Sidecar {
        Sidecar {
            attr: self.attr.clone(),
            dim: self.dim,
            metric: self.metric,
            fingerprint: self.fingerprint,
            ids: self.ids.clone(),
            data: self.data.clone(),
            hnsw: self.hnsw.clone(),
        }
    }

    pub fn attr(&self) -> &str {
        &self.attr
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn metric(&self) -> Metric {
        self.metric
    }

    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn has_index(&self) -> bool {
        self.hnsw.is_some()
    }

    pub fn hnsw(&self) -> Option<&Hnsw> {
        self.hnsw.as_ref()
    }

    /// Every node id that carries a vector, ascending.
    pub fn ids(&self) -> &[u32] {
        &self.ids
    }

    /// Row index of `node`, or `None` when the node has no vector.
    pub fn row_index(&self, node: Id) -> Option<usize> {
        self.ids.binary_search(&node).ok()
    }

    /// The stored vector for `node`.
    pub fn row(&self, node: Id) -> Option<&[f32]> {
        self.row_index(node).map(|i| self.row_at(i))
    }

    /// The stored vector at row `i`. Panics if `i` is out of range; the
    /// only producers of row indices are `row_index` and the HNSW
    /// adjacency, both validated.
    pub fn row_at(&self, i: usize) -> &[f32] {
        &self.data[i * self.dim..(i + 1) * self.dim]
    }

    fn norm_at(&self, i: usize) -> f32 {
        match self.norms.get(i) {
            Some(n) => *n,
            None => 0.0,
        }
    }

    /// Reject a query vector whose shape does not match the attribute,
    /// with a message meant for the user.
    pub fn validate_query(&self, q: &[f32]) -> Result<(), String> {
        if q.len() != self.dim {
            return Err(format!(
                "vector attribute `{}` has dimension {}, query vector has {}",
                self.attr,
                self.dim,
                q.len()
            ));
        }
        Ok(())
    }

    /// Distance from `q` to row `i`. `q_norm` must be `norm(q)`; compute
    /// it once per query, never per comparison.
    pub fn dist_at(&self, q: &[f32], q_norm: f32, i: usize) -> f32 {
        self.metric.dist(q, q_norm, self.row_at(i), self.norm_at(i))
    }

    /// Distance between two stored rows. The HNSW build compares rows to
    /// each other rather than to a query vector, so it goes through here.
    pub fn dist_between(&self, a: usize, b: usize) -> f32 {
        self.metric.dist(
            self.row_at(a),
            self.norm_at(a),
            self.row_at(b),
            self.norm_at(b),
        )
    }

    /// Distance from `q` to `node`, or `None` when it carries no vector.
    pub fn dist_to(&self, q: &[f32], q_norm: f32, node: Id) -> Option<f32> {
        self.row_index(node).map(|i| self.dist_at(q, q_norm, i))
    }

    /// A cursor over the whole attribute.
    ///
    /// `use_index` false forces the exact brute-force baseline even when
    /// an HNSW graph is present — this is the "sin índice" sub-mode every
    /// strategy is measured in, and the oracle the approximate arms are
    /// scored against.
    pub fn cursor<'a>(&'a self, q: &'a [f32], use_index: bool) -> Box<dyn NnCursor + 'a> {
        if self.validate_query(q).is_err() {
            return Box::new(EmptyCursor);
        }
        if use_index {
            if let Some(h) = &self.hnsw {
                return Box::new(crate::vector::hnsw::HnswCursor::new(self, h, q));
            }
        }
        self.brute_force_cursor(q)
    }

    /// A cursor restricted to `candidates`.
    ///
    /// This is the in-LTJ strategy's index-free sub-mode: rather than
    /// walking the global neighbour stream and testing membership, sort
    /// the level's candidate set directly. Ids with no vector are
    /// dropped — they cannot be among the nearest to anything.
    pub fn cursor_over<'a>(&'a self, q: &'a [f32], candidates: &[Id]) -> Box<dyn NnCursor + 'a> {
        if self.validate_query(q).is_err() {
            return Box::new(EmptyCursor);
        }
        Box::new(BruteForceCursor::from_sorted(
            self.rank_candidates(q, candidates),
        ))
    }

    /// `candidates` ranked nearest first, as a plain vector.
    ///
    /// The in-LTJ strategy's local source calls this once per visit to
    /// the search level and iterates the result directly, so it must not
    /// pay for boxing a cursor each time. Candidates with no vector are
    /// dropped: they cannot be among the nearest to anything.
    pub fn rank_candidates(&self, q: &[f32], candidates: &[Id]) -> Vec<(Id, f32)> {
        if self.validate_query(q).is_err() {
            return Vec::new();
        }
        let q_norm = self.query_norm(q);
        let mut pairs: Vec<(Id, f32)> = candidates
            .iter()
            .filter_map(|&id| self.row_index(id).map(|i| (id, self.dist_at(q, q_norm, i))))
            .collect();
        pairs.sort_by(|a, b| crate::vector::metric::cmp_dist(a.1, b.1).then_with(|| a.0.cmp(&b.0)));
        pairs
    }

    /// Norm of a query vector under this attribute's metric. Zero when
    /// the metric does not consult norms, so the work is skipped.
    pub fn query_norm(&self, q: &[f32]) -> f32 {
        if self.metric.needs_norms() {
            norm(q)
        } else {
            0.0
        }
    }

    fn brute_force_cursor<'a>(&'a self, q: &'a [f32]) -> Box<dyn NnCursor + 'a> {
        let q_norm = self.query_norm(q);
        let pairs: Vec<(Id, f32)> = self
            .ids
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, self.dist_at(q, q_norm, i)))
            .collect();
        Box::new(BruteForceCursor::from_unsorted(pairs))
    }
}

/// Every vector attribute attached to one database.
#[derive(Debug, Default)]
pub struct VectorStore {
    sets: HashMap<String, VectorSet>,
    /// Vector search is switched off for the rest of the session after
    /// any DML: the sidecars key on graph-internal node ids, and the
    /// mutation overlay allocates ids above the base watermark while
    /// `save()` renumbers everything. Returning stale neighbours would be
    /// worse than returning none.
    enabled: Cell<bool>,
}

impl VectorStore {
    pub fn empty() -> VectorStore {
        VectorStore {
            sets: HashMap::new(),
            enabled: Cell::new(true),
        }
    }

    /// Load every sidecar sitting next to `db_path`.
    ///
    /// Returns the store plus one human-readable warning per sidecar that
    /// was skipped. Skipping is always the right call here: a sidecar
    /// that fails to parse, or whose fingerprint does not match the
    /// database, points at node ids that no longer mean what they meant
    /// when it was built.
    pub fn open(db_path: &Path, fingerprint: u64) -> (VectorStore, Vec<String>) {
        let mut sets = HashMap::new();
        let mut warnings = Vec::new();
        for (attr, path) in Sidecar::discover(db_path) {
            match Sidecar::read_from_path(&path) {
                Ok(s) => {
                    if s.fingerprint != fingerprint {
                        warnings.push(format!(
                            "vector sidecar `{}` was built for a different graph \
                             (fingerprint {:#x} vs {:#x}); skipping. Rebuild it with vec_build.",
                            path.display(),
                            s.fingerprint,
                            fingerprint
                        ));
                        continue;
                    }
                    if s.attr != attr {
                        warnings.push(format!(
                            "vector sidecar `{}` declares attribute `{}` but is named for `{attr}`; \
                             skipping.",
                            path.display(),
                            s.attr
                        ));
                        continue;
                    }
                    sets.insert(attr, VectorSet::from_sidecar(s));
                }
                Err(e) => warnings.push(format!("vector sidecar `{}`: {e}", path.display())),
            }
        }
        (
            VectorStore {
                sets,
                enabled: Cell::new(true),
            },
            warnings,
        )
    }

    /// The attribute's vectors, or `None` when it has no sidecar or the
    /// store has been disabled.
    pub fn get(&self, attr: &str) -> Option<&VectorSet> {
        if !self.enabled.get() {
            return None;
        }
        self.sets.get(attr)
    }

    /// Loaded attribute names, sorted. Reported regardless of the enabled
    /// flag so diagnostics can say "there is a sidecar, but it is off".
    pub fn attrs(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.sets.keys().map(|s| s.as_str()).collect();
        out.sort_unstable();
        out
    }

    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.get()
    }

    /// Turn vector search off for the rest of the session. Called after
    /// any successful data-modifying statement.
    pub fn disable(&self) {
        self.enabled.set(false);
    }

    pub fn insert(&mut self, set: VectorSet) {
        self.sets.insert(set.attr().to_string(), set);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_of(metric: Metric, ids: Vec<u32>, data: Vec<f32>, dim: usize) -> VectorSet {
        VectorSet::from_sidecar(Sidecar {
            attr: "emb".to_string(),
            dim,
            metric,
            fingerprint: 7,
            ids,
            data,
            hnsw: None,
        })
    }

    /// Four points on a line at x = 0, 1, 2, 3, owned by nodes 10..40.
    fn line() -> VectorSet {
        set_of(
            Metric::L2Sq,
            vec![10, 20, 30, 40],
            vec![0.0, 0.0, 1.0, 0.0, 2.0, 0.0, 3.0, 0.0],
            2,
        )
    }

    fn drain(mut c: Box<dyn NnCursor + '_>) -> Vec<(Id, f32)> {
        let mut out = Vec::new();
        while let Some(e) = c.next() {
            out.push(e);
        }
        out
    }

    #[test]
    fn row_lookup_is_by_node_id_not_row_index() {
        let s = line();
        assert_eq!(s.row(20), Some(&[1.0, 0.0][..]));
        assert_eq!(s.row_index(30), Some(2));
        assert_eq!(s.row(25), None, "a node with no vector is absent");
    }

    #[test]
    fn cursor_walks_the_whole_attribute_in_distance_order() {
        let s = line();
        let q = [0.0, 0.0];
        let got = drain(s.cursor(&q, false));
        assert_eq!(
            got.iter().map(|e| e.0).collect::<Vec<_>>(),
            vec![10, 20, 30, 40]
        );
        assert_eq!(got[0].1, 0.0);
        assert_eq!(got[3].1, 9.0, "squared distance, not euclidean");
    }

    #[test]
    fn cursor_over_restricts_to_the_candidate_set() {
        let s = line();
        let q = [0.0, 0.0];
        let got = drain(s.cursor_over(&q, &[40, 20]));
        assert_eq!(got.iter().map(|e| e.0).collect::<Vec<_>>(), vec![20, 40]);
    }

    #[test]
    fn cursor_over_drops_candidates_without_vectors() {
        let s = line();
        let q = [0.0, 0.0];
        let got = drain(s.cursor_over(&q, &[999, 30]));
        assert_eq!(got.iter().map(|e| e.0).collect::<Vec<_>>(), vec![30]);
    }

    #[test]
    fn a_dimension_mismatch_yields_an_empty_cursor_and_an_error() {
        let s = line();
        let q = [0.0, 0.0, 0.0];
        assert!(s.validate_query(&q).is_err());
        assert!(drain(s.cursor(&q, false)).is_empty());
        assert!(drain(s.cursor_over(&q, &[10])).is_empty());
    }

    #[test]
    fn cosine_precomputes_norms_and_ranks_by_direction() {
        // Same direction, wildly different magnitude: cosine must rank
        // the long parallel vector ahead of the short orthogonal one.
        let s = set_of(Metric::Cosine, vec![1, 2], vec![100.0, 0.0, 0.0, 1.0], 2);
        let q = [1.0, 0.0];
        let got = drain(s.cursor(&q, false));
        assert_eq!(got[0].0, 1);
        assert!(got[0].1.abs() < 1e-6);
        assert!((got[1].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_metric_skips_norm_computation() {
        let s = line();
        assert_eq!(s.query_norm(&[3.0, 4.0]), 0.0);
        let c = set_of(Metric::Cosine, vec![1], vec![1.0, 0.0], 2);
        assert_eq!(c.query_norm(&[3.0, 4.0]), 5.0);
    }

    #[test]
    fn dist_to_is_none_for_a_node_without_a_vector() {
        let s = line();
        assert_eq!(s.dist_to(&[0.0, 0.0], 0.0, 20), Some(1.0));
        assert_eq!(s.dist_to(&[0.0, 0.0], 0.0, 21), None);
    }

    #[test]
    fn store_hides_sets_once_disabled() {
        let mut store = VectorStore::empty();
        store.insert(line());
        assert!(store.get("emb").is_some());
        assert_eq!(store.attrs(), vec!["emb"]);

        store.disable();
        assert!(store.get("emb").is_none(), "DML must switch vectors off");
        assert_eq!(store.attrs(), vec!["emb"], "diagnostics still see it");
        assert!(!store.is_enabled());
    }

    #[test]
    fn store_open_skips_a_fingerprint_mismatch() {
        let dir = std::env::temp_dir().join(format!("frogql_vstore_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let db = dir.join("t.gdb");

        let good = Sidecar {
            attr: "good".to_string(),
            dim: 1,
            metric: Metric::L2Sq,
            fingerprint: 42,
            ids: vec![1],
            data: vec![0.0],
            hnsw: None,
        };
        let stale = Sidecar {
            attr: "stale".to_string(),
            fingerprint: 43,
            ..good.clone()
        };
        good.write_to_path(&Sidecar::path_for(&db, "good"))
            .expect("write good");
        stale
            .write_to_path(&Sidecar::path_for(&db, "stale"))
            .expect("write stale");

        let (store, warnings) = VectorStore::open(&db, 42);
        assert_eq!(store.attrs(), vec!["good"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("different graph"), "{}", warnings[0]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn store_open_on_a_db_with_no_sidecars_is_empty_and_quiet() {
        let (store, warnings) = VectorStore::open(Path::new("/nonexistent-xyz/t.gdb"), 0);
        assert!(store.is_empty());
        assert!(warnings.is_empty());
    }
}
