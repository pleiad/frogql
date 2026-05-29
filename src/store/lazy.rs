//! LazyGraphStore — page-cache-backed graph access for large graphs.
//!
//! Unlike `MemoryGraphStore` which loads everything into memory, LazyGraphStore keeps only
//! compact indexes in memory (label→IDs, adjacency) and reads node/edge records
//! on demand through the pager's LRU page cache.
//!
//! Memory usage: O(num_elements) for indexes + O(cache_size) for page data,
//! instead of O(graph_data_size) for the full in-memory approach.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::io;
use std::path::Path;

use crate::model::graph::Props;
use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, PathValue, Value};
use crate::pager::page::{Page, PageType, PAGE_SIZE};
use crate::pager::pager::Pager;
use crate::runtime::catalog::GraphTypeCatalog;
use crate::typing::label_type::LabelType;

use super::catalog_io;
use super::disk_index;
use super::overlay::{
    apply_label_mods, label_type_with_added, label_type_with_removed, labels_from_strings,
    MutationOverlay,
};
use super::record::{self, PropValue};
use super::secondary_index::SecondaryIndex;
use super::string_table::StringTable;

/// Location of a record on disk: page number + cell index within that page.
#[derive(Debug, Clone, Copy)]
struct RecordLoc {
    page_num: u32,
    cell_index: u16,
}

/// Compressed Sparse Row adjacency.
///
/// Replaces the previous `HashMap<NodeId, Vec<EdgeId>>` representation.
/// `offsets[n]..offsets[n+1]` gives the slice of `flat` belonging to node
/// `n`. Total memory is `(node_count + 1 + edge_count_in_dir) * 4` bytes,
/// roughly half the HashMap variant for the same data, with cache-friendly
/// contiguous access. Builds in O(N + E) via bucket-sort, ~10× faster than
/// per-edge HashMap inserts at SF0.1 scale.
#[derive(Debug, Clone, Default)]
struct AdjCsr {
    /// Length `node_count + 1`. Sentinel: `offsets[node_count] == flat.len()`.
    offsets: Vec<u32>,
    /// Edge ids, contiguous slices per node.
    flat: Vec<u32>,
}

impl AdjCsr {
    fn empty(node_count: usize) -> Self {
        Self {
            offsets: vec![0; node_count + 1],
            flat: Vec::new(),
        }
    }

    fn slice(&self, node_id: u32) -> &[u32] {
        let n = node_id as usize;
        if n + 1 >= self.offsets.len() {
            return &[];
        }
        let start = self.offsets[n] as usize;
        let end = self.offsets[n + 1] as usize;
        &self.flat[start..end]
    }
}

/// Build a CSR from an unordered list of `(node_id, edge_id)` pairs in O(N + E)
/// via bucket-sort. Replaces ~E HashMap entry/push operations with two linear
/// passes — the dominant per-direction cost in `LazyGraphStore::open`.
fn build_csr(node_count: usize, pairs: &[(u32, u32)]) -> AdjCsr {
    let mut offsets = vec![0u32; node_count + 1];
    for (n, _) in pairs {
        offsets[*n as usize + 1] += 1;
    }
    for i in 1..offsets.len() {
        offsets[i] += offsets[i - 1];
    }
    let mut flat = vec![0u32; pairs.len()];
    // `cursor` tracks the next free slot per node; starts equal to the
    // final offsets and advances as we place edges.
    let mut cursor = offsets[..node_count].to_vec();
    for (n, e) in pairs {
        let pos = cursor[*n as usize] as usize;
        flat[pos] = *e;
        cursor[*n as usize] += 1;
    }
    AdjCsr { offsets, flat }
}

pub struct LazyGraphStore {
    pager: RefCell<Pager>,
    strings: StringTable,

    // Counts
    node_count: u32,
    edge_count: u32,

    // Record locations on disk (for lazy loading)
    node_locs: Vec<RecordLoc>, // internal_id → disk location
    edge_locs: Vec<RecordLoc>,

    // Edge topology (compact: just internal IDs, not full records)
    edge_src: Vec<u32>, // edge internal_id → src node internal_id
    edge_tgt: Vec<u32>,
    edge_directed: Vec<bool>,

    // Indexes (use internal IDs for compactness)
    label_to_nodes: HashMap<String, Vec<u32>>,
    label_to_edges: HashMap<String, Vec<u32>>,
    /// CSR adjacency per direction. Indexed by NodeId (internal u32). Built
    /// in O(N + E) at open time; slice access is one prefix-sum lookup +
    /// contiguous read. Replaces a HashMap<NodeId, Vec<EdgeId>> that cost
    /// ~5s to populate at SF0.1 scale.
    outgoing_csr: AdjCsr,
    incoming_csr: AdjCsr,
    undirected_csr: AdjCsr,

    // Graph-type catalog (loaded from chain at open, persisted via save_catalog).
    catalog: RefCell<GraphTypeCatalog>,
    /// Last persisted catalog chain root. Tracked so subsequent writes
    /// can free the old chain before allocating new pages.
    catalog_root: Cell<u32>,

    // Secondary indexes — auto-inferred at open from unique-valued node
    // properties. Memory-only for now (rebuilt every open). Used by the LTJ
    // optimizer to constant-fold `(x:L {prop: literal})` start lookups.
    secondary: RefCell<SecondaryIndex>,
    /// Last persisted secondary-index DDL chain root. `0` means "no
    /// chain" — for legacy files written before this slot existed and
    /// for fresh databases that never declared a DDL index. Tracked
    /// here so subsequent writes can free the old chain. TODO: remove
    /// the legacy `0` interpretation once every stored .gdb has been
    /// re-saved with the slot present.
    secondary_index_root: Cell<u32>,

    /// In-RAM mutation overlay for ISO §13 DML (INSERT / DELETE / DETACH
    /// DELETE in MVP-0). Empty during read-only sessions; non-empty after
    /// the first DML statement. Persistence happens via `save()`, which
    /// materializes the merged base+overlay view into a fresh `.gdb`.
    overlay: RefCell<MutationOverlay>,
}

impl LazyGraphStore {
    /// Open a .gql database file with lazy loading.
    /// Scans all pages to build compact indexes, but does NOT load record data.
    pub fn open(db_path: &Path) -> io::Result<Self> {
        Self::open_with_cache(db_path, 2000)
    }

    /// SQLite-style "create on open": if `db_path` does not exist, write
    /// an empty `.gdb` to that path first, then open it. The empty file
    /// has zero nodes / zero edges / empty catalog (DEFAULT auto-active);
    /// the caller can then issue `INSERT` statements and persist via
    /// `save()`. If the path exists, behaves identically to `open()`.
    pub fn open_or_create(db_path: &Path) -> io::Result<Self> {
        if !db_path.exists() {
            let empty = crate::model::graph::MemoryGraphStore::from_raw(
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            super::io::save_graph(&empty, db_path)?;
        }
        Self::open(db_path)
    }

    pub fn open_with_cache(db_path: &Path, cache_size: usize) -> io::Result<Self> {
        let trace = std::env::var("GQLITE_TRACE_OPEN").is_ok();
        let t0 = std::time::Instant::now();
        let mut pager = Pager::open_with_cache(db_path, cache_size)?;
        if trace {
            eprintln!("  pager open:           {:.3}s", t0.elapsed().as_secs_f64());
        }

        let string_table_root = pager.header.string_table_root;
        let node_locs_root = pager.header.node_locs_root;
        let edge_topo_root = pager.header.edge_topo_root;
        let label_index_root = pager.header.label_index_root;
        let edge_label_index_root = pager.header.edge_label_index_root;
        let adjacency_root = pager.header.adjacency_root;

        // Load string table (needed to resolve label names for indexes)
        let t1 = std::time::Instant::now();
        let st_pages = if string_table_root != 0 {
            disk_index::read_u32_chain(&mut pager, string_table_root)?
        } else {
            collect_pages_by_type(&mut pager, PageType::StringTable)?
        };
        let strings = StringTable::load(&st_pages, &mut pager)?;
        if trace {
            eprintln!("  string table load:    {:.3}s", t1.elapsed().as_secs_f64());
        }

        // All three roots must be set for a valid fast index.
        // Files upgraded by an intermediate version may have node_locs/edge_topo
        // but not string_table_root — treat those as legacy.
        let has_fast_index = string_table_root != 0 && node_locs_root != 0 && edge_topo_root != 0;

        let catalog_root = pager.header.catalog_root;
        let secondary_index_root = pager.header.secondary_index_root;
        let mut store = LazyGraphStore {
            pager: RefCell::new(pager),
            strings,
            node_count: 0,
            edge_count: 0,
            node_locs: Vec::new(),
            edge_locs: Vec::new(),
            edge_src: Vec::new(),
            edge_tgt: Vec::new(),
            edge_directed: Vec::new(),
            label_to_nodes: HashMap::new(),
            label_to_edges: HashMap::new(),
            outgoing_csr: AdjCsr::default(),
            incoming_csr: AdjCsr::default(),
            undirected_csr: AdjCsr::default(),
            catalog: RefCell::new(GraphTypeCatalog::new()),
            catalog_root: Cell::new(catalog_root),
            secondary: RefCell::new(SecondaryIndex::new()),
            secondary_index_root: Cell::new(secondary_index_root),
            overlay: RefCell::new(MutationOverlay::default()),
        };

        let t2 = std::time::Instant::now();
        if has_fast_index {
            // Fast path: read pre-built indexes directly
            store.load_from_indexes(
                node_locs_root,
                edge_topo_root,
                label_index_root,
                edge_label_index_root,
                adjacency_root,
            )?;
        } else {
            // Legacy file: full page scan, then upgrade the file with new indexes
            store.load_from_page_scan()?;
            store.upgrade_file(db_path)?;
        }
        if trace {
            eprintln!(
                "  topology + indexes:   {:.3}s  ({} nodes, {} edges)",
                t2.elapsed().as_secs_f64(),
                store.node_count,
                store.edge_count
            );
        }

        // Seed the mutation overlay with the base counts so newly inserted
        // ids land contiguously above the disk-backed range.
        *store.overlay.borrow_mut() = MutationOverlay::new(store.node_count, store.edge_count);

        // Load the catalog chain (if any). A legacy file with
        // catalog_root=0 yields an empty catalog and stays permissive.
        let t3 = std::time::Instant::now();
        if store.catalog_root.get() != 0 {
            let mut pager = store.pager.borrow_mut();
            let cat = catalog_io::read_catalog(&mut pager, store.catalog_root.get())?;
            *store.catalog.borrow_mut() = cat;
        }
        if trace {
            eprintln!("  catalog load:         {:.3}s", t3.elapsed().as_secs_f64());
        }

        // Auto-infer secondary indexes. Bulk path: walk node records once,
        // decode each exactly once, and build hash+btree per (label, prop)
        // in a single pass. Skip with GQLITE_DISABLE_AUTO_INDEXES=1.
        let t4 = std::time::Instant::now();
        if std::env::var("GQLITE_DISABLE_AUTO_INDEXES").is_err() {
            let idx = store.build_auto_indexes_bulk();
            if std::env::var("GQLITE_DEBUG_INDEXES").is_ok() {
                eprintln!("auto-built {} secondary indexes:", idx.list().len());
                for spec in idx.list() {
                    eprintln!(
                        "  {} on (:{} {{{}}}) — {} entries",
                        spec.name, spec.label, spec.prop, spec.entries
                    );
                }
            }
            *store.secondary.borrow_mut() = idx;
        }
        if trace {
            eprintln!(
                "  secondary index auto-build: {:.3}s  ({} indexes)",
                t4.elapsed().as_secs_f64(),
                store.secondary.borrow().list().len()
            );
        }

        // Replay persisted DDL-declared indexes. The auto-build above
        // produced the indexes the heuristic infers from data; this
        // adds back the ones the user declared via `CREATE INDEX`,
        // which the heuristic skips (typically because the property is
        // non-unique within the label, like `Post.creationDate`). A
        // legacy `.gdb` written before the slot existed has
        // `secondary_index_root == 0` and `read_specs` returns an
        // empty list, so this path is a no-op for old files. TODO:
        // drop the `0` legacy path once all stored databases have been
        // re-saved with the slot present.
        let t5 = std::time::Instant::now();
        let specs = {
            let mut pager = store.pager.borrow_mut();
            super::secondary_index_io::read_specs(&mut pager, store.secondary_index_root.get())?
        };
        if !specs.is_empty() {
            let mut sec = store.secondary.borrow_mut();
            for spec in specs {
                // `build_declared` errors only on a same-kind conflict
                // with an already-present index, which here would mean
                // the auto pass already built one for this `(label,
                // prop, kind)`. Skip silently — the auto-built copy is
                // semantically identical, no need to rebuild.
                let _ = sec.build_declared(&store, spec.name, &spec.label, &spec.prop, spec.kind);
            }
        }
        if trace {
            eprintln!(
                "  secondary index DDL replay:  {:.3}s  ({} indexes total)",
                t5.elapsed().as_secs_f64(),
                store.secondary.borrow().list().len()
            );
        }

        Ok(store)
    }

    /// Read-only borrow of the secondary indexes.
    pub fn secondary_indexes(&self) -> Ref<'_, SecondaryIndex> {
        self.secondary.borrow()
    }

    /// Build the auto-inferred SecondaryIndex via a single pass over node
    /// records. Three optimizations vs the generic `SecondaryIndex::auto_build`:
    ///
    /// 1. Each node record is decoded exactly once (the trait path called
    ///    `node_labels` then `node_props` — both decoded the same record).
    /// 2. Bucket maps are keyed by `(label_sid, prop_sid)` — u32 string IDs
    ///    that the StringTable already interned. Avoids allocating two
    ///    `String`s per (label, prop) pair per node.
    /// 3. Label / prop names are resolved exactly once per pair at the end,
    ///    when emitting the final IndexSpec.
    pub fn build_auto_indexes_bulk(&self) -> SecondaryIndex {
        use crate::store::secondary_index::{IndexKey, IndexKind};
        use std::collections::{BTreeMap, HashMap};

        let mut per_label_prop: HashMap<(u32, u32), HashMap<IndexKey, Vec<Id>>> = HashMap::new();
        let mut per_label_count: HashMap<u32, usize> = HashMap::new();

        for nid in 0..self.node_count {
            let decoded = self.read_node_record(nid);
            // Convert each PropValue to Value once (shared across every
            // label this node carries) and key by its interned `name_sid`.
            let props_resolved: Vec<(u32, crate::model::value::Value)> = decoded
                .props
                .iter()
                .map(|(name_sid, pv)| (*name_sid, self.prop_to_value(pv)))
                .collect();
            for &label_sid in &decoded.label_str_ids {
                *per_label_count.entry(label_sid).or_insert(0) += 1;
                for (prop_sid, v) in &props_resolved {
                    if let Some(idx_k) = IndexKey::from_value(v) {
                        per_label_prop
                            .entry((label_sid, *prop_sid))
                            .or_default()
                            .entry(idx_k)
                            .or_default()
                            .push(nid);
                    }
                }
            }
        }

        let mut idx = SecondaryIndex::new();
        // Same uniqueness rule as `auto_build`: only index when every value
        // bucket is a singleton AND every node of the label has the prop.
        for ((label_sid, prop_sid), bucket) in per_label_prop {
            let label_count = *per_label_count.get(&label_sid).unwrap_or(&0);
            let total_present: usize = bucket.values().map(|v| v.len()).sum();
            let unique = bucket.values().all(|v| v.len() == 1);
            if unique && total_present == label_count && label_count > 0 {
                let entries = bucket.len();
                let label = self.strings.resolve(label_sid).unwrap().to_string();
                let prop = self.strings.resolve(prop_sid).unwrap().to_string();
                let btree: BTreeMap<IndexKey, Vec<Id>> =
                    bucket.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                idx.insert_prebuilt(
                    &label,
                    &prop,
                    IndexKind::Hash,
                    true,
                    entries,
                    Some(bucket),
                    None,
                );
                idx.insert_prebuilt(
                    &label,
                    &prop,
                    IndexKind::BTree,
                    true,
                    entries,
                    None,
                    Some(btree),
                );
            }
        }
        idx
    }

    /// Mutable borrow of the secondary indexes — used by `CREATE / DROP
    /// INDEX` handlers in the REPL and Python bindings.
    pub fn secondary_indexes_mut(&self) -> RefMut<'_, SecondaryIndex> {
        self.secondary.borrow_mut()
    }

    // ---- Graph-type catalog ----

    /// Read-only borrow of the active catalog.
    pub fn catalog(&self) -> Ref<'_, GraphTypeCatalog> {
        self.catalog.borrow()
    }

    /// Mutable borrow of the catalog. Callers must invoke
    /// [`save_catalog`](Self::save_catalog) before dropping the store
    /// for changes to persist.
    pub fn catalog_mut(&self) -> RefMut<'_, GraphTypeCatalog> {
        self.catalog.borrow_mut()
    }

    /// Persist the in-memory catalog to disk: rewrites the chain and
    /// updates the file header. Idempotent.
    pub fn save_catalog(&self) -> io::Result<()> {
        let cat = self.catalog.borrow();
        let mut pager = self.pager.borrow_mut();
        let new_root = catalog_io::write_catalog(&mut pager, &cat, self.catalog_root.get())?;
        self.catalog_root.set(new_root);
        pager.header.catalog_root = new_root;
        pager.header.active_type_name = cat.active_name().map(|s| s.to_string());
        pager.write_header()?;
        Ok(())
    }

    /// Fast open: read node locs, edge topology, label indexes, and adjacency
    /// directly from pre-built index pages.
    fn load_from_indexes(
        &mut self,
        node_locs_root: u32,
        edge_topo_root: u32,
        label_index_root: u32,
        edge_label_index_root: u32,
        adjacency_root: u32,
    ) -> io::Result<()> {
        let mut pager = self.pager.borrow_mut();

        // Node locations
        let raw_node_locs = disk_index::read_node_locs(&mut pager, node_locs_root)?;
        self.node_count = raw_node_locs.len() as u32;
        self.node_locs = raw_node_locs
            .into_iter()
            .map(|(pg, ci)| RecordLoc {
                page_num: pg,
                cell_index: ci,
            })
            .collect();

        // Edge topology (locations + src/tgt/directed)
        let (raw_edge_locs, edge_src, edge_tgt, edge_directed) =
            disk_index::read_edge_topo(&mut pager, edge_topo_root)?;
        self.edge_count = raw_edge_locs.len() as u32;
        self.edge_locs = raw_edge_locs
            .into_iter()
            .map(|(pg, ci)| RecordLoc {
                page_num: pg,
                cell_index: ci,
            })
            .collect();
        self.edge_src = edge_src;
        self.edge_tgt = edge_tgt;
        self.edge_directed = edge_directed;

        // Label indexes (already persisted by save_graph)
        if label_index_root != 0 {
            let label_entries = disk_index::read_label_index_root(&mut pager, label_index_root)?;
            for (label_sid, first_page) in label_entries {
                let label = self.strings.resolve(label_sid).unwrap().to_string();
                let ids = disk_index::read_u32_chain(&mut pager, first_page)?;
                self.label_to_nodes.insert(label, ids);
            }
        }
        if edge_label_index_root != 0 {
            let label_entries =
                disk_index::read_label_index_root(&mut pager, edge_label_index_root)?;
            for (label_sid, first_page) in label_entries {
                let label = self.strings.resolve(label_sid).unwrap().to_string();
                let ids = disk_index::read_u32_chain(&mut pager, first_page)?;
                self.label_to_edges.insert(label, ids);
            }
        }

        // Adjacency: prefer the new CSR layout if present (one big sequential
        // chain per direction), fall back to the legacy per-node format
        // (slow: ~N small reads). Both produce the same in-memory CSR.
        let csr_root = pager.header.csr_adjacency_root;
        let nc = self.node_count as usize;
        if csr_root != 0 {
            let ((out_off, out_fl), (in_off, in_fl), (und_off, und_fl)) =
                disk_index::read_adjacency_csr(&mut pager, csr_root)?;
            self.outgoing_csr = AdjCsr {
                offsets: out_off,
                flat: out_fl,
            };
            self.incoming_csr = AdjCsr {
                offsets: in_off,
                flat: in_fl,
            };
            self.undirected_csr = AdjCsr {
                offsets: und_off,
                flat: und_fl,
            };
        } else if adjacency_root != 0 {
            let adj_entries = disk_index::read_adjacency_root(&mut pager, adjacency_root)?;
            let mut out_pairs: Vec<(u32, u32)> = Vec::new();
            let mut in_pairs: Vec<(u32, u32)> = Vec::new();
            let mut und_pairs: Vec<(u32, u32)> = Vec::new();
            for (node_iid, first_page) in adj_entries {
                let triples = disk_index::read_triple_chain(&mut pager, first_page)?;
                for (edge_iid, _other_node_iid, kind) in triples {
                    match kind {
                        0 => out_pairs.push((node_iid, edge_iid)),
                        1 => in_pairs.push((node_iid, edge_iid)),
                        2 => und_pairs.push((node_iid, edge_iid)),
                        _ => {}
                    }
                }
            }
            self.outgoing_csr = build_csr(nc, &out_pairs);
            self.incoming_csr = build_csr(nc, &in_pairs);
            self.undirected_csr = build_csr(nc, &und_pairs);
        } else {
            self.outgoing_csr = AdjCsr::empty(nc);
            self.incoming_csr = AdjCsr::empty(nc);
            self.undirected_csr = AdjCsr::empty(nc);
        }

        Ok(())
    }

    /// Legacy fallback: scan all pages to build indexes.
    fn load_from_page_scan(&mut self) -> io::Result<()> {
        let page_count = self.pager.borrow().header.page_count;
        // Accumulate adjacency pairs across all pages, then build CSR once
        // at the end. Avoids per-edge HashMap entry/push.
        let mut out_pairs: Vec<(u32, u32)> = Vec::new();
        let mut in_pairs: Vec<(u32, u32)> = Vec::new();
        let mut und_pairs: Vec<(u32, u32)> = Vec::new();
        for pg in 1..page_count {
            let page = self.pager.borrow_mut().read_page(pg)?;
            match page.page_type() {
                PageType::NodeData => {
                    self.index_nodes_from_page(pg, &page);
                }
                PageType::EdgeData => {
                    self.index_edges_from_page(
                        pg,
                        &page,
                        &mut out_pairs,
                        &mut in_pairs,
                        &mut und_pairs,
                    );
                }
                _ => {}
            }
        }
        let nc = self.node_count as usize;
        self.outgoing_csr = build_csr(nc, &out_pairs);
        self.incoming_csr = build_csr(nc, &in_pairs);
        self.undirected_csr = build_csr(nc, &und_pairs);
        Ok(())
    }

    /// Upgrade a legacy .gdb file by writing fast-open index pages.
    fn upgrade_file(&self, db_path: &Path) -> io::Result<()> {
        // Re-open the file in read-write mode to append index pages
        let mut pager = Pager::open(db_path)?;

        // Write string table page directory
        let st_page_list = self.strings.page_numbers().to_vec();
        let st_root = disk_index::write_u32_list(&mut pager, &st_page_list)?;

        // Write node locations
        let node_locs: Vec<(u32, u16)> = self
            .node_locs
            .iter()
            .map(|loc| (loc.page_num, loc.cell_index))
            .collect();
        let node_locs_root = disk_index::write_node_locs(&mut pager, &node_locs)?;

        // Write edge topology
        let edge_locs: Vec<(u32, u16)> = self
            .edge_locs
            .iter()
            .map(|loc| (loc.page_num, loc.cell_index))
            .collect();
        let edge_topo_root = disk_index::write_edge_topo(
            &mut pager,
            &edge_locs,
            &self.edge_src,
            &self.edge_tgt,
            &self.edge_directed,
        )?;

        // Update header
        pager.header.string_table_root = st_root;
        pager.header.node_locs_root = node_locs_root;
        pager.header.edge_topo_root = edge_topo_root;
        pager.write_header()?;

        Ok(())
    }

    // --- Index building (scan-time, lightweight) ---

    fn index_nodes_from_page(&mut self, page_num: u32, page: &Page) {
        for i in 0..page.cell_count() {
            let (offset, end) = cell_bounds(page, i);
            let (decoded, _) = record::decode_node(&page.data[offset..end]);

            let internal_id = self.node_count;
            self.node_count += 1;

            for &sid in &decoded.label_str_ids {
                let label = self.strings.resolve(sid).unwrap().to_string();
                self.label_to_nodes
                    .entry(label)
                    .or_default()
                    .push(internal_id);
            }

            self.node_locs.push(RecordLoc {
                page_num,
                cell_index: i,
            });
        }
    }

    fn index_edges_from_page(
        &mut self,
        page_num: u32,
        page: &Page,
        out_pairs: &mut Vec<(u32, u32)>,
        in_pairs: &mut Vec<(u32, u32)>,
        und_pairs: &mut Vec<(u32, u32)>,
    ) {
        for i in 0..page.cell_count() {
            let (offset, end) = cell_bounds(page, i);
            let decoded = record::decode_edge(&page.data[offset..end]);

            let internal_id = self.edge_count;
            self.edge_count += 1;

            for &sid in &decoded.node.label_str_ids {
                let label = self.strings.resolve(sid).unwrap().to_string();
                self.label_to_edges
                    .entry(label)
                    .or_default()
                    .push(internal_id);
            }

            let src = decoded.src_internal_id;
            let tgt = decoded.tgt_internal_id;

            if decoded.directed {
                out_pairs.push((src, internal_id));
                in_pairs.push((tgt, internal_id));
            } else {
                und_pairs.push((src, internal_id));
                und_pairs.push((tgt, internal_id));
            }

            self.edge_locs.push(RecordLoc {
                page_num,
                cell_index: i,
            });
            self.edge_src.push(src);
            self.edge_tgt.push(tgt);
            self.edge_directed.push(decoded.directed);
        }
    }

    // --- Lazy record reading ---

    fn read_node_record(&self, internal_id: u32) -> record::DecodedNode {
        let loc = self.node_locs[internal_id as usize];
        let page = self.pager.borrow_mut().read_page(loc.page_num).unwrap();
        let (offset, end) = cell_bounds(&page, loc.cell_index);
        let (decoded, _) = record::decode_node(&page.data[offset..end]);
        decoded
    }

    fn read_edge_record(&self, internal_id: u32) -> record::DecodedEdge {
        let loc = self.edge_locs[internal_id as usize];
        let page = self.pager.borrow_mut().read_page(loc.page_num).unwrap();
        let (offset, end) = cell_bounds(&page, loc.cell_index);
        record::decode_edge(&page.data[offset..end])
    }

    fn decode_props_from_record(&self, encoded: &[(u32, PropValue)]) -> Props {
        let mut props = HashMap::new();
        for (name_sid, pv) in encoded {
            let name = self.strings.resolve(*name_sid).unwrap().to_string();
            props.insert(name, self.prop_to_value(pv));
        }
        props
    }

    fn prop_to_value(&self, pv: &PropValue) -> Value {
        match pv {
            PropValue::Null => Value::Null,
            PropValue::Int(n) => Value::Int(*n),
            PropValue::Float(x) => Value::Float(*x),
            PropValue::Str(sid) => Value::Str(self.strings.resolve(*sid).unwrap().to_string()),
            PropValue::Bool(b) => Value::Bool(*b),
            PropValue::List(items) => {
                Value::List(items.iter().map(|it| self.prop_to_value(it)).collect())
            }
            PropValue::Record(fields) => {
                let mut m = std::collections::BTreeMap::new();
                for (sid, v) in fields {
                    let name = self.strings.resolve(*sid).unwrap().to_string();
                    m.insert(name, self.prop_to_value(v));
                }
                Value::Record(m)
            }
        }
    }

    pub fn node_count(&self) -> u32 {
        self.node_count
    }
    pub fn edge_count(&self) -> u32 {
        self.edge_count
    }

    /// Cache statistics from the underlying pager.
    pub fn cache_stats(&self) -> (u64, u64) {
        self.pager.borrow().cache_stats()
    }

    /// Materialize the merged base+overlay view into an in-RAM `MemoryGraphStore`.
    /// IDs get compacted (tombstoned slots disappear); edges remap their
    /// endpoints accordingly. Used by `save()` and `dump_*()`.
    pub fn materialize_to_graph(&self) -> crate::model::graph::MemoryGraphStore {
        // Walk live nodes in current ID order; remember their original →
        // new (dense) id mapping so edges can be remapped.
        let mut id_map: HashMap<u32, u32> = HashMap::new();
        let mut node_names: Vec<String> = Vec::new();
        let mut node_labels: Vec<LabelType> = Vec::new();
        let mut node_props: Vec<Props> = Vec::new();
        for old_id in self.nodes() {
            let new_id = node_names.len() as u32;
            id_map.insert(old_id, new_id);
            node_names.push(self.node_name(old_id).to_string());
            node_labels.push(self.node_labels(old_id));
            node_props.push(self.node_props(old_id));
        }

        // Edges: walk both directed and undirected, preserving direction.
        let mut edge_names: Vec<String> = Vec::new();
        let mut edge_labels: Vec<LabelType> = Vec::new();
        let mut edge_props: Vec<Props> = Vec::new();
        let mut edge_src: Vec<u32> = Vec::new();
        let mut edge_tgt: Vec<u32> = Vec::new();
        let mut edge_directed: Vec<bool> = Vec::new();
        for old_id in self.edges_directed() {
            let s = self.src(old_id);
            let t = self.tgt(old_id);
            // Tombstoned endpoints are filtered out by `nodes()`, so a
            // stranded edge would produce a missing-key here. Skip it.
            let (Some(&new_s), Some(&new_t)) = (id_map.get(&s), id_map.get(&t)) else {
                continue;
            };
            edge_names.push(self.edge_name(old_id).to_string());
            edge_labels.push(self.edge_labels(old_id));
            edge_props.push(self.edge_props(old_id));
            edge_src.push(new_s);
            edge_tgt.push(new_t);
            edge_directed.push(true);
        }
        for old_id in self.edges_undirected() {
            let s = self.src(old_id);
            let t = self.tgt(old_id);
            let (Some(&new_s), Some(&new_t)) = (id_map.get(&s), id_map.get(&t)) else {
                continue;
            };
            edge_names.push(self.edge_name(old_id).to_string());
            edge_labels.push(self.edge_labels(old_id));
            edge_props.push(self.edge_props(old_id));
            edge_src.push(new_s);
            edge_tgt.push(new_t);
            edge_directed.push(false);
        }

        crate::model::graph::MemoryGraphStore::from_raw(
            node_names,
            node_labels,
            node_props,
            edge_names,
            edge_labels,
            edge_props,
            edge_src,
            edge_tgt,
            edge_directed,
        )
    }

    /// Refresh the catalog's `DEFAULT` schema from the live store iff the
    /// dirty flag is set. Idempotent: every read path that fetches the
    /// active schema or pretty-prints DEFAULT calls through here, so DML
    /// statements get O(1) "mark dirty" while the schema-consulting paths
    /// pay the O(N+E) inference at most once per dirty cycle.
    pub fn refresh_default_if_dirty(&self) {
        let needs_refresh = {
            let cat = self.catalog.borrow();
            cat.is_default_dirty()
                || !cat
                    .types
                    .contains_key(super::super::runtime::catalog::DEFAULT_NAME)
        };
        if !needs_refresh {
            return;
        }
        let schema = crate::typing::inference::infer_simple_schema(self);
        let mut cat = self.catalog.borrow_mut();
        // Don't activate or alter `active` here; just replace the entry.
        // `install_default` would also flip `active`, which is wrong when
        // the user is just inspecting DEFAULT without using it.
        cat.types.insert(
            super::super::runtime::catalog::DEFAULT_NAME.to_string(),
            schema,
        );
        cat.validations
            .remove(super::super::runtime::catalog::DEFAULT_NAME);
        cat.default_dirty = false;
    }

    /// Persist the current state to `db_path`. ISO doesn't mandate this
    /// surface — gqlite mirrors SQLite's "explicit save" model: until the
    /// caller calls `save`, mutations live only in the in-RAM overlay.
    ///
    /// Atomicity: writes to `<db_path>.tmp` first and renames over the
    /// destination, so a crash mid-write cannot corrupt the existing file.
    /// The current `LazyGraphStore` keeps its old in-memory state pointing
    /// at the pre-save pager (POSIX rename keeps the old inode alive while
    /// our fd holds it open), so subsequent reads / mutations stay
    /// coherent. Re-opening from the file in a new process yields the
    /// post-save image.
    pub fn save(&self, db_path: &Path) -> io::Result<()> {
        // Refresh DEFAULT first so the persisted file's catalog matches
        // the post-mutation data. (No-op when nothing's dirty.)
        self.refresh_default_if_dirty();
        // If the catalog has no DEFAULT entry yet (fresh DB never queried
        // DEFAULT), build one now so the saved file ships with a valid
        // schema users can SHOW after reopen. The catalog stays "active:
        // None" — we just populate the entry.
        {
            let needs_default = !self
                .catalog
                .borrow()
                .types
                .contains_key(super::super::runtime::catalog::DEFAULT_NAME);
            if needs_default {
                let schema = crate::typing::inference::infer_simple_schema(self);
                self.catalog.borrow_mut().types.insert(
                    super::super::runtime::catalog::DEFAULT_NAME.to_string(),
                    schema,
                );
            }
        }
        let g = self.materialize_to_graph();
        let cat = self.catalog.borrow().clone();
        // Persist DDL-declared indexes too. The current `secondary`
        // RefCell holds specs whose NodeIds reference the live graph,
        // not the materialised + ID-compacted output, but the DDL
        // entries we serialise are pure schema (label, prop, kind,
        // name) so they round-trip cleanly: the next open's auto
        // pass populates buckets from the on-disk node records, then
        // replays the DDL list on top.
        let specs: Vec<_> = self.secondary.borrow().list().to_vec();
        super::io::save_graph_with_catalog_and_indexes_atomic(&g, &cat, &specs, db_path)
    }
}

impl GraphAccess for LazyGraphStore {
    fn nodes(&self) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = (0..self.node_count)
            .filter(|id| !overlay.is_node_deleted(*id))
            .collect();
        for offset in 0..overlay.new_nodes.len() as u32 {
            let id = overlay.base_node_count + offset;
            if !overlay.is_node_deleted(id) {
                out.push(id);
            }
        }
        out
    }

    fn edges_directed(&self) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = (0..self.edge_count)
            .filter(|i| !overlay.is_edge_deleted(*i) && self.edge_directed[*i as usize])
            .collect();
        for (offset, e) in overlay.new_edges.iter().enumerate() {
            let id = overlay.base_edge_count + offset as u32;
            if !overlay.is_edge_deleted(id) && e.directed {
                out.push(id);
            }
        }
        out
    }

    fn edges_undirected(&self) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = (0..self.edge_count)
            .filter(|i| !overlay.is_edge_deleted(*i) && !self.edge_directed[*i as usize])
            .collect();
        for (offset, e) in overlay.new_edges.iter().enumerate() {
            let id = overlay.base_edge_count + offset as u32;
            if !overlay.is_edge_deleted(id) && !e.directed {
                out.push(id);
            }
        }
        out
    }

    fn node_labels(&self, id: Id) -> LabelType {
        let overlay = self.overlay.borrow();
        if let Some(n) = overlay.get_new_node(id) {
            return n.labels.clone();
        }
        let decoded = self.read_node_record(id);
        let mut labels: Vec<String> = decoded
            .label_str_ids
            .iter()
            .map(|&sid| self.strings.resolve(sid).unwrap().to_string())
            .collect();
        apply_label_mods(&mut labels, overlay.mod_node_labels.get(&id));
        labels_from_strings(labels)
    }

    fn edge_labels(&self, id: Id) -> LabelType {
        let overlay = self.overlay.borrow();
        if let Some(e) = overlay.get_new_edge(id) {
            return e.labels.clone();
        }
        let decoded = self.read_edge_record(id);
        let mut labels: Vec<String> = decoded
            .node
            .label_str_ids
            .iter()
            .map(|&sid| self.strings.resolve(sid).unwrap().to_string())
            .collect();
        apply_label_mods(&mut labels, overlay.mod_edge_labels.get(&id));
        labels_from_strings(labels)
    }

    fn node_props(&self, id: Id) -> Props {
        let overlay = self.overlay.borrow();
        if let Some(n) = overlay.get_new_node(id) {
            return n.props.clone();
        }
        // Base node: start from disk, apply per-prop mutations on top.
        let decoded = self.read_node_record(id);
        let mut base = self.decode_props_from_record(&decoded.props);
        if let Some(mods) = overlay.mod_node_props.get(&id) {
            if mods.cleared {
                base.clear();
            }
            for (name, op) in &mods.set {
                match op {
                    Some(v) => {
                        base.insert(name.clone(), v.clone());
                    }
                    None => {
                        base.remove(name);
                    }
                }
            }
        }
        base
    }

    fn edge_props(&self, id: Id) -> Props {
        let overlay = self.overlay.borrow();
        if let Some(e) = overlay.get_new_edge(id) {
            return e.props.clone();
        }
        let decoded = self.read_edge_record(id);
        let mut base = self.decode_props_from_record(&decoded.node.props);
        if let Some(mods) = overlay.mod_edge_props.get(&id) {
            if mods.cleared {
                base.clear();
            }
            for (name, op) in &mods.set {
                match op {
                    Some(v) => {
                        base.insert(name.clone(), v.clone());
                    }
                    None => {
                        base.remove(name);
                    }
                }
            }
        }
        base
    }

    fn src(&self, edge_id: Id) -> Id {
        if let Some(e) = self.overlay.borrow().get_new_edge(edge_id) {
            return e.src;
        }
        self.edge_src[edge_id as usize]
    }

    fn tgt(&self, edge_id: Id) -> Id {
        if let Some(e) = self.overlay.borrow().get_new_edge(edge_id) {
            return e.tgt;
        }
        self.edge_tgt[edge_id as usize]
    }

    fn is_directed(&self, edge_id: Id) -> bool {
        if let Some(e) = self.overlay.borrow().get_new_edge(edge_id) {
            return e.directed;
        }
        self.edge_directed[edge_id as usize]
    }

    fn edge_path_value(&self, edge_id: Id) -> PathValue {
        if self.is_directed(edge_id) {
            PathValue::EdgeDirectional(edge_id)
        } else {
            PathValue::EdgeUndirectional(edge_id)
        }
    }

    fn node_name(&self, id: Id) -> &str {
        if id >= self.overlay.borrow().base_node_count {
            // Synthetic display name for overlay-tracked nodes — keeps
            // node_count display + REPL "path" column working without
            // round-tripping through the (still empty) string table.
            return Box::leak(Box::new(format!("auto-n-{id}")));
        }
        let decoded = self.read_node_record(id);
        let s = self.strings.resolve(decoded.user_id_str_id).unwrap();
        // Leak the string to return &str — only called for display, not hot path
        Box::leak(Box::new(s.to_string()))
    }

    fn edge_name(&self, id: Id) -> &str {
        if id >= self.overlay.borrow().base_edge_count {
            return Box::leak(Box::new(format!("auto-e-{id}")));
        }
        let decoded = self.read_edge_record(id);
        let s = self.strings.resolve(decoded.node.user_id_str_id).unwrap();
        Box::leak(Box::new(s.to_string()))
    }

    fn nodes_with_label(&self, label: &str) -> Option<Vec<Id>> {
        let overlay = self.overlay.borrow();
        let base = self.label_to_nodes.get(label);
        let overlay_dirty = !overlay.new_nodes.is_empty()
            || !overlay.deleted_nodes.is_empty()
            || !overlay.mod_node_labels.is_empty();
        if !overlay_dirty {
            return base.cloned();
        }
        // Filter base list by tombstones AND by `REMOVE x:label` mods.
        let mut out: Vec<Id> = base
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|id| !overlay.is_node_deleted(*id))
                    .filter(|id| {
                        overlay
                            .mod_node_labels
                            .get(id)
                            .map(|m| !m.removed.contains(label))
                            .unwrap_or(true)
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Base nodes that gained this label via SET x:label.
        for (id, mods) in &overlay.mod_node_labels {
            if mods.added.contains(label) && !overlay.is_node_deleted(*id) && !out.contains(id) {
                out.push(*id);
            }
        }
        for (offset, n) in overlay.new_nodes.iter().enumerate() {
            let id = overlay.base_node_count + offset as u32;
            if overlay.is_node_deleted(id) {
                continue;
            }
            if crate::model::graph::MemoryGraphStore::label_strings(&n.labels)
                .iter()
                .any(|l| l == label)
            {
                out.push(id);
            }
        }
        // The label index "exists" iff the base map carried this label OR
        // any overlay node uses it. Otherwise return None so callers fall
        // back to a full scan.
        if base.is_some() || !out.is_empty() {
            Some(out)
        } else {
            None
        }
    }

    fn directed_edges_with_label(&self, label: &str) -> Option<Vec<Id>> {
        let overlay = self.overlay.borrow();
        let base = self.label_to_edges.get(label);
        let overlay_dirty = !overlay.new_edges.is_empty()
            || !overlay.deleted_edges.is_empty()
            || !overlay.mod_edge_labels.is_empty();
        if !overlay_dirty {
            return base.map(|ids| {
                ids.iter()
                    .filter(|&&iid| self.edge_directed[iid as usize])
                    .copied()
                    .collect()
            });
        }
        let mut out: Vec<Id> = base
            .map(|ids| {
                ids.iter()
                    .filter(|&&iid| {
                        !overlay.is_edge_deleted(iid) && self.edge_directed[iid as usize]
                    })
                    .filter(|&&iid| {
                        overlay
                            .mod_edge_labels
                            .get(&iid)
                            .map(|m| !m.removed.contains(label))
                            .unwrap_or(true)
                    })
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        for (id, mods) in &overlay.mod_edge_labels {
            if mods.added.contains(label)
                && (*id as usize) < self.edge_directed.len()
                && self.edge_directed[*id as usize]
                && !overlay.is_edge_deleted(*id)
                && !out.contains(id)
            {
                out.push(*id);
            }
        }
        for (offset, e) in overlay.new_edges.iter().enumerate() {
            let id = overlay.base_edge_count + offset as u32;
            if overlay.is_edge_deleted(id) || !e.directed {
                continue;
            }
            if crate::model::graph::MemoryGraphStore::label_strings(&e.labels)
                .iter()
                .any(|l| l == label)
            {
                out.push(id);
            }
        }
        if base.is_some() || !out.is_empty() {
            Some(out)
        } else {
            None
        }
    }

    fn undirected_edges_with_label(&self, label: &str) -> Option<Vec<Id>> {
        let overlay = self.overlay.borrow();
        let base = self.label_to_edges.get(label);
        let overlay_dirty = !overlay.new_edges.is_empty()
            || !overlay.deleted_edges.is_empty()
            || !overlay.mod_edge_labels.is_empty();
        if !overlay_dirty {
            return base.map(|ids| {
                ids.iter()
                    .filter(|&&iid| !self.edge_directed[iid as usize])
                    .copied()
                    .collect()
            });
        }
        let mut out: Vec<Id> = base
            .map(|ids| {
                ids.iter()
                    .filter(|&&iid| {
                        !overlay.is_edge_deleted(iid) && !self.edge_directed[iid as usize]
                    })
                    .filter(|&&iid| {
                        overlay
                            .mod_edge_labels
                            .get(&iid)
                            .map(|m| !m.removed.contains(label))
                            .unwrap_or(true)
                    })
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        for (id, mods) in &overlay.mod_edge_labels {
            if mods.added.contains(label)
                && (*id as usize) < self.edge_directed.len()
                && !self.edge_directed[*id as usize]
                && !overlay.is_edge_deleted(*id)
                && !out.contains(id)
            {
                out.push(*id);
            }
        }
        for (offset, e) in overlay.new_edges.iter().enumerate() {
            let id = overlay.base_edge_count + offset as u32;
            if overlay.is_edge_deleted(id) || e.directed {
                continue;
            }
            if crate::model::graph::MemoryGraphStore::label_strings(&e.labels)
                .iter()
                .any(|l| l == label)
            {
                out.push(id);
            }
        }
        if base.is_some() || !out.is_empty() {
            Some(out)
        } else {
            None
        }
    }

    fn lookup_node_eq(&self, label: &str, prop: &str, value: &Value) -> Option<Vec<Id>> {
        // Secondary indexes capture only the base nodes. With overlay
        // mutations in flight, returning a base-only set would silently
        // hide newly inserted matches and pre-tombstone existing ones, so
        // we conservatively force the caller to fall back to a scan
        // (`None`). A future MVP-2 will maintain the indexes incrementally.
        let overlay = self.overlay.borrow();
        if !overlay.new_nodes.is_empty() || !overlay.deleted_nodes.is_empty() {
            return None;
        }
        self.secondary.borrow().lookup_eq(label, prop, value)
    }

    fn lookup_node_range(
        &self,
        label: &str,
        prop: &str,
        lo: std::ops::Bound<Value>,
        hi: std::ops::Bound<Value>,
    ) -> Option<Vec<Id>> {
        let overlay = self.overlay.borrow();
        if !overlay.new_nodes.is_empty() || !overlay.deleted_nodes.is_empty() {
            return None;
        }
        self.secondary.borrow().lookup_range(label, prop, lo, hi)
    }

    fn lookup_node_ordered(&self, label: &str, prop: &str, ascending: bool) -> Option<Vec<Id>> {
        self.secondary.borrow().ordered_ids(label, prop, ascending)
    }

    fn outgoing_edges(&self, node_id: Id) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = self
            .outgoing_csr
            .slice(node_id)
            .iter()
            .copied()
            .filter(|id| !overlay.is_edge_deleted(*id))
            .collect();
        if let Some(extra) = overlay.new_outgoing.get(&node_id) {
            for &eid in extra {
                if !overlay.is_edge_deleted(eid) {
                    out.push(eid);
                }
            }
        }
        out
    }

    fn incoming_edges(&self, node_id: Id) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = self
            .incoming_csr
            .slice(node_id)
            .iter()
            .copied()
            .filter(|id| !overlay.is_edge_deleted(*id))
            .collect();
        if let Some(extra) = overlay.new_incoming.get(&node_id) {
            for &eid in extra {
                if !overlay.is_edge_deleted(eid) {
                    out.push(eid);
                }
            }
        }
        out
    }

    fn undirected_edges_of(&self, node_id: Id) -> Vec<Id> {
        let overlay = self.overlay.borrow();
        let mut out: Vec<Id> = self
            .undirected_csr
            .slice(node_id)
            .iter()
            .copied()
            .filter(|id| !overlay.is_edge_deleted(*id))
            .collect();
        if let Some(extra) = overlay.new_undirected.get(&node_id) {
            for &eid in extra {
                if !overlay.is_edge_deleted(eid) {
                    out.push(eid);
                }
            }
        }
        out
    }
}

impl crate::model::graph_access::GraphAccessMut for LazyGraphStore {
    fn insert_node(&self, labels: LabelType, props: Props) -> Id {
        let mut overlay = self.overlay.borrow_mut();
        overlay.insert_node(labels, props)
    }

    fn insert_edge(&self, src: Id, tgt: Id, directed: bool, labels: LabelType, props: Props) -> Id {
        // Endpoint validation lives in the runtime so the caller can
        // raise the right ISO error code (G1002 / G1003). Here we just
        // record the mutation.
        let mut overlay = self.overlay.borrow_mut();
        overlay.insert_edge(src, tgt, directed, labels, props)
    }

    fn delete_edge(&self, id: Id) {
        let mut overlay = self.overlay.borrow_mut();
        overlay.delete_edge(id);
    }

    fn detach_delete_node(&self, id: Id) {
        // Collect incident edges from the merged view first; mutating
        // the overlay invalidates `outgoing_edges` etc., so snapshot.
        let incident: Vec<Id> = self
            .outgoing_edges(id)
            .into_iter()
            .chain(self.incoming_edges(id))
            .chain(self.undirected_edges_of(id))
            .collect();
        let mut overlay = self.overlay.borrow_mut();
        for eid in incident {
            overlay.delete_edge(eid);
        }
        overlay.delete_node(id);
    }

    fn delete_node_no_detach(&self, id: Id) -> Result<(), crate::model::graph_access::G1001> {
        let remaining: Vec<Id> = self
            .outgoing_edges(id)
            .into_iter()
            .chain(self.incoming_edges(id))
            .chain(self.undirected_edges_of(id))
            .collect();
        if !remaining.is_empty() {
            return Err(crate::model::graph_access::G1001 {
                node: id,
                remaining_edges: remaining,
            });
        }
        self.overlay.borrow_mut().delete_node(id);
        Ok(())
    }

    fn is_node_alive(&self, id: Id) -> bool {
        let overlay = self.overlay.borrow();
        if overlay.is_node_deleted(id) {
            return false;
        }
        if id < overlay.base_node_count {
            return true;
        }
        overlay.get_new_node(id).is_some()
    }

    fn is_edge_alive(&self, id: Id) -> bool {
        let overlay = self.overlay.borrow();
        if overlay.is_edge_deleted(id) {
            return false;
        }
        if id < overlay.base_edge_count {
            return true;
        }
        overlay.get_new_edge(id).is_some()
    }

    fn rollback_session(&self) {
        let mut overlay = self.overlay.borrow_mut();
        let bn = overlay.base_node_count;
        let be = overlay.base_edge_count;
        overlay.clear(bn, be);
    }

    fn set_node_prop(&self, id: Id, prop: &str, value: crate::model::value::Value) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_node_count {
            // New (overlay-tracked) node: write directly into its
            // OverlayNode entry, no PropMods bookkeeping needed.
            let off = (id - overlay.base_node_count) as usize;
            if let Some(n) = overlay.new_nodes.get_mut(off) {
                n.props.insert(prop.to_string(), value);
            }
            return;
        }
        // Base node: stage the change in the per-record PropMods map.
        let entry = overlay.mod_node_props.entry(id).or_default();
        entry.set.insert(prop.to_string(), Some(value));
    }

    fn set_edge_prop(&self, id: Id, prop: &str, value: crate::model::value::Value) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_edge_count {
            let off = (id - overlay.base_edge_count) as usize;
            if let Some(e) = overlay.new_edges.get_mut(off) {
                e.props.insert(prop.to_string(), value);
            }
            return;
        }
        let entry = overlay.mod_edge_props.entry(id).or_default();
        entry.set.insert(prop.to_string(), Some(value));
    }

    fn replace_node_props(&self, id: Id, props: Props) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_node_count {
            let off = (id - overlay.base_node_count) as usize;
            if let Some(n) = overlay.new_nodes.get_mut(off) {
                n.props = props;
            }
            return;
        }
        // ISO §13.3 GR8 b.i: clear the existing props, then apply the
        // new map. We encode that by flipping `cleared` and rebuilding
        // `set` to mirror only the new entries.
        let mut entry = crate::store::overlay::PropMods {
            cleared: true,
            set: HashMap::new(),
        };
        for (k, v) in props {
            entry.set.insert(k, Some(v));
        }
        overlay.mod_node_props.insert(id, entry);
    }

    fn replace_edge_props(&self, id: Id, props: Props) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_edge_count {
            let off = (id - overlay.base_edge_count) as usize;
            if let Some(e) = overlay.new_edges.get_mut(off) {
                e.props = props;
            }
            return;
        }
        let mut entry = crate::store::overlay::PropMods {
            cleared: true,
            set: HashMap::new(),
        };
        for (k, v) in props {
            entry.set.insert(k, Some(v));
        }
        overlay.mod_edge_props.insert(id, entry);
    }

    fn remove_node_prop(&self, id: Id, prop: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_node_count {
            let off = (id - overlay.base_node_count) as usize;
            if let Some(n) = overlay.new_nodes.get_mut(off) {
                n.props.remove(prop);
            }
            return;
        }
        // Stage a remove (None) in the per-record PropMods map so
        // future reads filter the property out.
        let entry = overlay.mod_node_props.entry(id).or_default();
        entry.set.insert(prop.to_string(), None);
    }

    fn remove_edge_prop(&self, id: Id, prop: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_edge_count {
            let off = (id - overlay.base_edge_count) as usize;
            if let Some(e) = overlay.new_edges.get_mut(off) {
                e.props.remove(prop);
            }
            return;
        }
        let entry = overlay.mod_edge_props.entry(id).or_default();
        entry.set.insert(prop.to_string(), None);
    }

    fn add_node_label(&self, id: Id, label: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_node_count {
            let off = (id - overlay.base_node_count) as usize;
            if let Some(n) = overlay.new_nodes.get_mut(off) {
                n.labels = label_type_with_added(&n.labels, label);
            }
            return;
        }
        let entry = overlay.mod_node_labels.entry(id).or_default();
        entry.removed.remove(label);
        entry.added.insert(label.to_string());
    }

    fn add_edge_label(&self, id: Id, label: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_edge_count {
            let off = (id - overlay.base_edge_count) as usize;
            if let Some(e) = overlay.new_edges.get_mut(off) {
                e.labels = label_type_with_added(&e.labels, label);
            }
            return;
        }
        let entry = overlay.mod_edge_labels.entry(id).or_default();
        entry.removed.remove(label);
        entry.added.insert(label.to_string());
    }

    fn remove_node_label(&self, id: Id, label: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_node_count {
            let off = (id - overlay.base_node_count) as usize;
            if let Some(n) = overlay.new_nodes.get_mut(off) {
                n.labels = label_type_with_removed(&n.labels, label);
            }
            return;
        }
        let entry = overlay.mod_node_labels.entry(id).or_default();
        entry.added.remove(label);
        entry.removed.insert(label.to_string());
    }

    fn remove_edge_label(&self, id: Id, label: &str) {
        let mut overlay = self.overlay.borrow_mut();
        if id >= overlay.base_edge_count {
            let off = (id - overlay.base_edge_count) as usize;
            if let Some(e) = overlay.new_edges.get_mut(off) {
                e.labels = label_type_with_removed(&e.labels, label);
            }
            return;
        }
        let entry = overlay.mod_edge_labels.entry(id).or_default();
        entry.added.remove(label);
        entry.removed.insert(label.to_string());
    }
}

fn cell_bounds(page: &Page, index: u16) -> (usize, usize) {
    let offset = page.cell_offset(index).unwrap() as usize;
    let end = if index == 0 {
        PAGE_SIZE
    } else {
        page.cell_offset(index - 1).unwrap() as usize
    };
    (offset, end)
}

fn collect_pages_by_type(pager: &mut Pager, pt: PageType) -> io::Result<Vec<u32>> {
    let mut pages = Vec::new();
    let count = pager.header.page_count;
    for pg in 1..count {
        let page = pager.read_page(pg)?;
        if page.page_type() == pt {
            pages.push(pg);
        }
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile;
    use crate::model::graph::MemoryGraphStore;
    use crate::runtime::engine::Runtime;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("gqlrust_test");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
    }

    fn lazy_fraud_run(query: &str) -> usize {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        query.hash(&mut h);
        let db_path = temp_path(&format!("lazy_fraud_{}.gql", h.finish()));
        cleanup(&db_path);

        let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
        let graph = MemoryGraphStore::from_file(&json_path).unwrap();
        graph.save(&db_path).unwrap();

        let store = LazyGraphStore::open(&db_path).unwrap();
        let rt = Runtime::new(&store);
        let pattern = compile(query).unwrap();
        let result = rt.run(&pattern).rows.len();

        cleanup(&db_path);
        result
    }

    #[test]
    fn test_lazy_basic_queries() {
        assert_eq!(lazy_fraud_run("()"), 5);
        assert_eq!(lazy_fraud_run("(x: Account)"), 4);
        assert_eq!(lazy_fraud_run("-[]->"), 5);
        assert_eq!(lazy_fraud_run("-[:Transfer]->"), 4);
    }

    #[test]
    fn test_lazy_traversal() {
        assert_eq!(lazy_fraud_run("()-[]->"), 5);
        assert_eq!(lazy_fraud_run("(x)-[:Foo]->"), 1);
        assert_eq!(lazy_fraud_run("()-[]->()-[]->()"), 5);
    }

    #[test]
    fn test_lazy_filters() {
        assert_eq!(lazy_fraud_run("(y WHERE y.isBlocked=true)"), 1);
        assert_eq!(lazy_fraud_run("(y WHERE y.isBlocked=false)"), 4);
        assert_eq!(lazy_fraud_run("(x WHERE x.isDummy bool)"), 1);
    }

    #[test]
    fn test_lazy_complex() {
        assert_eq!(
            lazy_fraud_run("(x) -[z:Transfer WHERE z.amount>1000000]-> (y WHERE y.isBlocked=true)"),
            1
        );
        assert_eq!(lazy_fraud_run("(x: Dummy) | (y: Account)"), 5);
        assert_eq!(lazy_fraud_run("-->{1,2}"), 23);
    }

    #[test]
    fn test_lazy_multi_label() {
        assert_eq!(lazy_fraud_run("(x: Dummy & Person)"), 1);
    }

    #[test]
    fn test_lazy_tiny_cache() {
        let db_path = temp_path("lazy_tiny_cache.gql");
        cleanup(&db_path);

        let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
        let graph = MemoryGraphStore::from_file(&json_path).unwrap();
        graph.save(&db_path).unwrap();

        // Open with a very small cache (3 pages)
        let store = LazyGraphStore::open_with_cache(&db_path, 3).unwrap();
        let rt = Runtime::new(&store);

        let pattern = compile("(x: Account)-[:Transfer]->(y)").unwrap();
        let results = rt.run(&pattern);
        assert_eq!(results.rows.len(), 4);

        let (hits, misses) = store.cache_stats();
        assert!(misses > 0, "tiny cache should have misses");
        println!("Tiny cache: hits={hits}, misses={misses}");

        cleanup(&db_path);
    }
}
