//! LazyGraphStore — page-cache-backed graph access for large graphs.
//!
//! Unlike `Graph` which loads everything into memory, LazyGraphStore keeps only
//! compact indexes in memory (label→IDs, adjacency) and reads node/edge records
//! on demand through the pager's LRU page cache.
//!
//! Memory usage: O(num_elements) for indexes + O(cache_size) for page data,
//! instead of O(graph_data_size) for the full in-memory approach.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::Path;

use crate::model::graph::Props;
use crate::model::graph_access::GraphAccess;
use crate::model::value::{Id, PathValue, Value};
use crate::pager::page::{Page, PageType, PAGE_SIZE};
use crate::pager::pager::Pager;
use crate::typing::label_type::LabelType;

use super::disk_index;
use super::record::{self, PropValue};
use super::string_table::StringTable;

/// Location of a record on disk: page number + cell index within that page.
#[derive(Debug, Clone, Copy)]
struct RecordLoc {
    page_num: u32,
    cell_index: u16,
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
    outgoing: HashMap<u32, Vec<u32>>,
    incoming: HashMap<u32, Vec<u32>>,
    undirected_adj: HashMap<u32, Vec<u32>>,
}

impl LazyGraphStore {
    /// Open a .gql database file with lazy loading.
    /// Scans all pages to build compact indexes, but does NOT load record data.
    pub fn open(db_path: &Path) -> io::Result<Self> {
        Self::open_with_cache(db_path, 2000)
    }

    pub fn open_with_cache(db_path: &Path, cache_size: usize) -> io::Result<Self> {
        let mut pager = Pager::open_with_cache(db_path, cache_size)?;

        let string_table_root = pager.header.string_table_root;
        let node_locs_root = pager.header.node_locs_root;
        let edge_topo_root = pager.header.edge_topo_root;
        let label_index_root = pager.header.label_index_root;
        let edge_label_index_root = pager.header.edge_label_index_root;
        let adjacency_root = pager.header.adjacency_root;

        // Load string table (needed to resolve label names for indexes)
        let st_pages = if string_table_root != 0 {
            disk_index::read_u32_chain(&mut pager, string_table_root)?
        } else {
            collect_pages_by_type(&mut pager, PageType::StringTable)?
        };
        let strings = StringTable::load(&st_pages, &mut pager)?;

        // All three roots must be set for a valid fast index.
        // Files upgraded by an intermediate version may have node_locs/edge_topo
        // but not string_table_root — treat those as legacy.
        let has_fast_index = string_table_root != 0 && node_locs_root != 0 && edge_topo_root != 0;

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
            outgoing: HashMap::new(),
            incoming: HashMap::new(),
            undirected_adj: HashMap::new(),
        };

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

        Ok(store)
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

        // Adjacency index (already persisted by save_graph)
        if adjacency_root != 0 {
            let adj_entries = disk_index::read_adjacency_root(&mut pager, adjacency_root)?;
            for (node_iid, first_page) in adj_entries {
                let triples = disk_index::read_triple_chain(&mut pager, first_page)?;
                for (edge_iid, _other_node_iid, kind) in triples {
                    match kind {
                        0 => self.outgoing.entry(node_iid).or_default().push(edge_iid),
                        1 => self.incoming.entry(node_iid).or_default().push(edge_iid),
                        2 => self
                            .undirected_adj
                            .entry(node_iid)
                            .or_default()
                            .push(edge_iid),
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }

    /// Legacy fallback: scan all pages to build indexes.
    fn load_from_page_scan(&mut self) -> io::Result<()> {
        let page_count = self.pager.borrow().header.page_count;
        for pg in 1..page_count {
            let page = self.pager.borrow_mut().read_page(pg)?;
            match page.page_type() {
                PageType::NodeData => {
                    self.index_nodes_from_page(pg, &page);
                }
                PageType::EdgeData => {
                    self.index_edges_from_page(pg, &page);
                }
                _ => {}
            }
        }
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

    fn index_edges_from_page(&mut self, page_num: u32, page: &Page) {
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
                self.outgoing.entry(src).or_default().push(internal_id);
                self.incoming.entry(tgt).or_default().push(internal_id);
            } else {
                self.undirected_adj
                    .entry(src)
                    .or_default()
                    .push(internal_id);
                self.undirected_adj
                    .entry(tgt)
                    .or_default()
                    .push(internal_id);
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

    fn decode_labels_from_record(&self, label_str_ids: &[u32]) -> LabelType {
        let labels: Vec<String> = label_str_ids
            .iter()
            .map(|&sid| self.strings.resolve(sid).unwrap().to_string())
            .collect();
        if labels.is_empty() {
            LabelType::Star
        } else {
            LabelType::from_list(&labels)
        }
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
}

impl GraphAccess for LazyGraphStore {
    fn nodes(&self) -> Vec<Id> {
        (0..self.node_count).collect()
    }

    fn edges_directed(&self) -> Vec<Id> {
        (0..self.edge_count)
            .filter(|&i| self.edge_directed[i as usize])
            .collect()
    }

    fn edges_undirected(&self) -> Vec<Id> {
        (0..self.edge_count)
            .filter(|&i| !self.edge_directed[i as usize])
            .collect()
    }

    fn node_labels(&self, id: Id) -> LabelType {
        let decoded = self.read_node_record(id);
        self.decode_labels_from_record(&decoded.label_str_ids)
    }

    fn edge_labels(&self, id: Id) -> LabelType {
        let decoded = self.read_edge_record(id);
        self.decode_labels_from_record(&decoded.node.label_str_ids)
    }

    fn node_props(&self, id: Id) -> Props {
        let decoded = self.read_node_record(id);
        self.decode_props_from_record(&decoded.props)
    }

    fn edge_props(&self, id: Id) -> Props {
        let decoded = self.read_edge_record(id);
        self.decode_props_from_record(&decoded.node.props)
    }

    fn src(&self, edge_id: Id) -> Id {
        self.edge_src[edge_id as usize]
    }

    fn tgt(&self, edge_id: Id) -> Id {
        self.edge_tgt[edge_id as usize]
    }

    fn is_directed(&self, edge_id: Id) -> bool {
        self.edge_directed[edge_id as usize]
    }

    fn edge_path_value(&self, edge_id: Id) -> PathValue {
        if self.edge_directed[edge_id as usize] {
            PathValue::EdgeDirectional(edge_id)
        } else {
            PathValue::EdgeUndirectional(edge_id)
        }
    }

    fn node_name(&self, id: Id) -> &str {
        let decoded = self.read_node_record(id);
        let s = self.strings.resolve(decoded.user_id_str_id).unwrap();
        // Leak the string to return &str — only called for display, not hot path
        Box::leak(Box::new(s.to_string()))
    }

    fn edge_name(&self, id: Id) -> &str {
        let decoded = self.read_edge_record(id);
        let s = self.strings.resolve(decoded.node.user_id_str_id).unwrap();
        Box::leak(Box::new(s.to_string()))
    }

    fn nodes_with_label(&self, label: &str) -> Option<Vec<Id>> {
        self.label_to_nodes.get(label).cloned()
    }

    fn directed_edges_with_label(&self, label: &str) -> Option<Vec<Id>> {
        self.label_to_edges.get(label).map(|ids| {
            ids.iter()
                .filter(|&&iid| self.edge_directed[iid as usize])
                .copied()
                .collect()
        })
    }

    fn undirected_edges_with_label(&self, label: &str) -> Option<Vec<Id>> {
        self.label_to_edges.get(label).map(|ids| {
            ids.iter()
                .filter(|&&iid| !self.edge_directed[iid as usize])
                .copied()
                .collect()
        })
    }

    fn outgoing_edges(&self, node_id: Id) -> Vec<Id> {
        self.outgoing.get(&node_id).cloned().unwrap_or_default()
    }

    fn incoming_edges(&self, node_id: Id) -> Vec<Id> {
        self.incoming.get(&node_id).cloned().unwrap_or_default()
    }

    fn undirected_edges_of(&self, node_id: Id) -> Vec<Id> {
        self.undirected_adj
            .get(&node_id)
            .cloned()
            .unwrap_or_default()
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
    use crate::model::graph::Graph;
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
        let graph = Graph::from_file(&json_path).unwrap();
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
        let graph = Graph::from_file(&json_path).unwrap();
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
