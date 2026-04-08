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
    node_locs: Vec<RecordLoc>,     // internal_id → disk location
    edge_locs: Vec<RecordLoc>,

    // Edge topology (compact: just internal IDs, not full records)
    edge_src: Vec<u32>,            // edge internal_id → src node internal_id
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

        // Load string table (needed to resolve label names for indexes)
        let st_pages = collect_pages_by_type(&mut pager, PageType::StringTable)?;
        let strings = StringTable::load(&st_pages, &mut pager)?;

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

        // Scan all pages to build indexes (read-only, records not kept)
        let page_count = store.pager.borrow().header.page_count;
        for pg in 1..page_count {
            let page = store.pager.borrow_mut().read_page(pg)?;
            match page.page_type() {
                PageType::NodeData => {
                    store.index_nodes_from_page(pg, &page);
                }
                PageType::EdgeData => {
                    store.index_edges_from_page(pg, &page);
                }
                _ => {}
            }
        }

        Ok(store)
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
                self.label_to_nodes.entry(label).or_default().push(internal_id);
            }

            self.node_locs.push(RecordLoc { page_num, cell_index: i });
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
                self.label_to_edges.entry(label).or_default().push(internal_id);
            }

            let src = decoded.src_internal_id;
            let tgt = decoded.tgt_internal_id;

            if decoded.directed {
                self.outgoing.entry(src).or_default().push(internal_id);
                self.incoming.entry(tgt).or_default().push(internal_id);
            } else {
                self.undirected_adj.entry(src).or_default().push(internal_id);
                self.undirected_adj.entry(tgt).or_default().push(internal_id);
            }

            self.edge_locs.push(RecordLoc { page_num, cell_index: i });
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
        let labels: Vec<String> = label_str_ids.iter()
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
            let val = match pv {
                PropValue::Int(n) => Value::Int(*n),
                PropValue::Str(sid) => Value::Str(self.strings.resolve(*sid).unwrap().to_string()),
                PropValue::Bool(b) => Value::Bool(*b),
            };
            props.insert(name, val);
        }
        props
    }

    pub fn node_count(&self) -> u32 { self.node_count }
    pub fn edge_count(&self) -> u32 { self.edge_count }

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
        self.undirected_adj.get(&node_id).cloned().unwrap_or_default()
    }
}

fn cell_bounds(page: &Page, index: u16) -> (usize, usize) {
    let offset = page.cell_offset(index).unwrap() as usize;
    let end = if index == 0 { PAGE_SIZE } else { page.cell_offset(index - 1).unwrap() as usize };
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
        assert_eq!(lazy_fraud_run("(x WHERE x.isDummy is bool)"), 1);
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
