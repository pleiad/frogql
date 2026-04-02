//! DiskGraphStore — fully disk-backed graph access with O(cache_size) memory.
//!
//! Nothing is held in memory except:
//! - The page cache (~8 MB default)
//! - The string table (needed to resolve string IDs — could also be lazy, but kept for now)
//! - Small root page pointers from the header
//!
//! All lookups (labels, properties, adjacency, ID resolution) go through
//! on-disk indexes read via the page cache.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::Path;

use crate::model::graph::Props;
use crate::model::graph_access::GraphAccess;
use crate::model::value::{PathValue, Value};
use crate::pager::page::PageType;
use crate::pager::pager::Pager;
use crate::typing::label_type::LabelType;

use super::disk_index;
use super::record::{self, PropValue};
use super::string_table::StringTable;

const ADJ_OUTGOING: u8 = 0;
const ADJ_INCOMING: u8 = 1;
const ADJ_UNDIRECTED: u8 = 2;

pub struct DiskGraphStore {
    pager: RefCell<Pager>,
    strings: StringTable,

    // Root page pointers (from header)
    node_label_root: u32,
    edge_label_root: u32,
    adjacency_root: u32,

    // Cached index roots: label_sid → first_page (loaded once from root pages)
    node_label_dir: RefCell<Option<Vec<(u32, u32)>>>,
    edge_label_dir: RefCell<Option<Vec<(u32, u32)>>>,
    adj_dir: RefCell<Option<Vec<(u32, u32)>>>,

    // Node/edge record page locations: internal_id → (page_num, cell_index)
    // These are compact (8 bytes per element) and needed for any record access.
    node_locs: Vec<(u32, u16)>,
    edge_locs: Vec<(u32, u16)>,

    // Edge topology: compact arrays (4 bytes each)
    edge_src: Vec<u32>,
    edge_tgt: Vec<u32>,
    edge_directed: Vec<bool>,

    // Reverse maps for user_id ↔ internal_id (loaded from ID index on demand)
    node_id_cache: RefCell<HashMap<String, u32>>,
    edge_id_cache: RefCell<HashMap<String, u32>>,
    node_user_ids: Vec<String>,
    edge_user_ids: Vec<String>,
}

impl DiskGraphStore {
    pub fn open(db_path: &Path) -> io::Result<Self> {
        Self::open_with_cache(db_path, 2000)
    }

    pub fn open_with_cache(db_path: &Path, cache_size: usize) -> io::Result<Self> {
        let mut pager = Pager::open_with_cache(db_path, cache_size)?;

        let node_count = pager.header.node_count;
        let edge_count = pager.header.edge_count;
        let node_label_root = pager.header.label_index_root;
        let edge_label_root = pager.header.edge_label_index_root;
        let adjacency_root = pager.header.adjacency_root;

        // Load string table (compact, needed for ID resolution)
        let st_pages = collect_st_pages(&mut pager)?;
        let strings = StringTable::load(&st_pages, &mut pager)?;

        // Scan node/edge data pages for record locations and topology
        let mut node_locs = Vec::with_capacity(node_count as usize);
        let mut edge_locs = Vec::with_capacity(edge_count as usize);
        let mut edge_src = Vec::with_capacity(edge_count as usize);
        let mut edge_tgt = Vec::with_capacity(edge_count as usize);
        let mut edge_directed = Vec::with_capacity(edge_count as usize);
        let mut node_user_ids = Vec::with_capacity(node_count as usize);
        let mut edge_user_ids = Vec::with_capacity(edge_count as usize);

        let page_count = pager.header.page_count;
        for pg in 1..page_count {
            let page = pager.read_page(pg)?;
            match page.page_type() {
                PageType::NodeData => {
                    for i in 0..page.cell_count() {
                        let (offset, end) = cell_bounds(&page, i);
                        let (decoded, _) = record::decode_node(&page.data[offset..end]);
                        let uid = strings.resolve(decoded.user_id_str_id).unwrap().to_string();
                        node_user_ids.push(uid);
                        node_locs.push((pg, i));
                    }
                }
                PageType::EdgeData => {
                    for i in 0..page.cell_count() {
                        let (offset, end) = cell_bounds(&page, i);
                        let decoded = record::decode_edge(&page.data[offset..end]);
                        let uid = strings.resolve(decoded.node.user_id_str_id).unwrap().to_string();
                        edge_user_ids.push(uid);
                        edge_locs.push((pg, i));
                        edge_src.push(decoded.src_internal_id);
                        edge_tgt.push(decoded.tgt_internal_id);
                        edge_directed.push(decoded.directed);
                    }
                }
                _ => {}
            }
        }

        // Build ID caches
        let mut node_id_cache = HashMap::new();
        for (i, uid) in node_user_ids.iter().enumerate() {
            node_id_cache.insert(uid.clone(), i as u32);
        }
        let mut edge_id_cache = HashMap::new();
        for (i, uid) in edge_user_ids.iter().enumerate() {
            edge_id_cache.insert(uid.clone(), i as u32);
        }

        Ok(DiskGraphStore {
            pager: RefCell::new(pager),
            strings,
            node_label_root,
            edge_label_root,
            adjacency_root,
            node_label_dir: RefCell::new(None),
            edge_label_dir: RefCell::new(None),
            adj_dir: RefCell::new(None),
            node_locs,
            edge_locs,
            edge_src,
            edge_tgt,
            edge_directed,
            node_id_cache: RefCell::new(node_id_cache),
            edge_id_cache: RefCell::new(edge_id_cache),
            node_user_ids,
            edge_user_ids,
        })
    }

    // --- Lazy loading of index directories ---

    fn ensure_node_label_dir(&self) {
        if self.node_label_dir.borrow().is_none() {
            let dir = disk_index::read_label_index_root(
                &mut self.pager.borrow_mut(), self.node_label_root
            ).unwrap_or_default();
            *self.node_label_dir.borrow_mut() = Some(dir);
        }
    }

    fn ensure_edge_label_dir(&self) {
        if self.edge_label_dir.borrow().is_none() {
            let dir = disk_index::read_label_index_root(
                &mut self.pager.borrow_mut(), self.edge_label_root
            ).unwrap_or_default();
            *self.edge_label_dir.borrow_mut() = Some(dir);
        }
    }

    fn ensure_adj_dir(&self) {
        if self.adj_dir.borrow().is_none() {
            let dir = disk_index::read_adjacency_root(
                &mut self.pager.borrow_mut(), self.adjacency_root
            ).unwrap_or_default();
            *self.adj_dir.borrow_mut() = Some(dir);
        }
    }

    fn read_node_record(&self, iid: u32) -> record::DecodedNode {
        let (pg, ci) = self.node_locs[iid as usize];
        let page = self.pager.borrow_mut().read_page(pg).unwrap();
        let (offset, end) = cell_bounds(&page, ci);
        let (decoded, _) = record::decode_node(&page.data[offset..end]);
        decoded
    }

    fn read_edge_record(&self, iid: u32) -> record::DecodedEdge {
        let (pg, ci) = self.edge_locs[iid as usize];
        let page = self.pager.borrow_mut().read_page(pg).unwrap();
        let (offset, end) = cell_bounds(&page, ci);
        record::decode_edge(&page.data[offset..end])
    }

    fn decode_labels(&self, label_str_ids: &[u32]) -> LabelType {
        let labels: Vec<String> = label_str_ids.iter()
            .map(|&sid| self.strings.resolve(sid).unwrap().to_string())
            .collect();
        LabelType::from_list(&labels)
    }

    fn decode_props(&self, encoded: &[(u32, PropValue)]) -> Props {
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

    fn get_adj_entries(&self, node_iid: u32, kind: u8) -> Vec<u32> {
        self.ensure_adj_dir();
        let dir = self.adj_dir.borrow();
        let dir = dir.as_ref().unwrap();

        // Find this node's adjacency page
        if let Some((_, adj_page)) = dir.iter().find(|(niid, _)| *niid == node_iid) {
            let triples = disk_index::read_triple_chain(
                &mut self.pager.borrow_mut(), *adj_page
            ).unwrap_or_default();
            triples.iter()
                .filter(|(_, _, k)| *k == kind)
                .map(|(edge_iid, _, _)| *edge_iid)
                .collect()
        } else {
            vec![]
        }
    }

    fn label_index_lookup(&self, label: &str, dir: &[(u32, u32)]) -> Option<Vec<u32>> {
        let label_sid = self.strings.str_to_id.get(label)?;
        let (_, first_page) = dir.iter().find(|(sid, _)| sid == label_sid)?;
        let ids = disk_index::read_u32_chain(&mut self.pager.borrow_mut(), *first_page)
            .unwrap_or_default();
        Some(ids)
    }

    pub fn cache_stats(&self) -> (u64, u64) {
        self.pager.borrow().cache_stats()
    }
}

impl GraphAccess for DiskGraphStore {
    fn nodes(&self) -> Vec<String> {
        self.node_user_ids.clone()
    }

    fn edges_directed(&self) -> Vec<String> {
        self.edge_user_ids.iter().enumerate()
            .filter(|(i, _)| self.edge_directed[*i])
            .map(|(_, id)| id.clone())
            .collect()
    }

    fn edges_undirected(&self) -> Vec<String> {
        self.edge_user_ids.iter().enumerate()
            .filter(|(i, _)| !self.edge_directed[*i])
            .map(|(_, id)| id.clone())
            .collect()
    }

    fn labels(&self, id: &str) -> &LabelType {
        if let Some(&iid) = self.node_id_cache.borrow().get(id) {
            let decoded = self.read_node_record(iid);
            Box::leak(Box::new(self.decode_labels(&decoded.label_str_ids)))
        } else if let Some(&iid) = self.edge_id_cache.borrow().get(id) {
            let decoded = self.read_edge_record(iid);
            Box::leak(Box::new(self.decode_labels(&decoded.node.label_str_ids)))
        } else {
            panic!("unknown element: {id}")
        }
    }

    fn props(&self, id: &str) -> &Props {
        if let Some(&iid) = self.node_id_cache.borrow().get(id) {
            let decoded = self.read_node_record(iid);
            Box::leak(Box::new(self.decode_props(&decoded.props)))
        } else if let Some(&iid) = self.edge_id_cache.borrow().get(id) {
            let decoded = self.read_edge_record(iid);
            Box::leak(Box::new(self.decode_props(&decoded.node.props)))
        } else {
            panic!("unknown element: {id}")
        }
    }

    fn src(&self, edge_id: &str) -> &str {
        let iid = self.edge_id_cache.borrow()[edge_id] as usize;
        &self.node_user_ids[self.edge_src[iid] as usize]
    }

    fn tgt(&self, edge_id: &str) -> &str {
        let iid = self.edge_id_cache.borrow()[edge_id] as usize;
        &self.node_user_ids[self.edge_tgt[iid] as usize]
    }

    fn endpoints(&self, edge_id: &str) -> (&str, &str) {
        (self.src(edge_id), self.tgt(edge_id))
    }

    fn is_directed(&self, edge_id: &str) -> bool {
        let iid = self.edge_id_cache.borrow()[edge_id] as usize;
        self.edge_directed[iid]
    }

    fn edge_path_value(&self, edge_id: &str) -> PathValue {
        if self.is_directed(edge_id) {
            PathValue::EdgeDirectional(edge_id.to_string())
        } else {
            PathValue::EdgeUndirectional(edge_id.to_string())
        }
    }

    fn nodes_with_label(&self, label: &str) -> Option<Vec<String>> {
        self.ensure_node_label_dir();
        let dir = self.node_label_dir.borrow();
        let ids = self.label_index_lookup(label, dir.as_ref().unwrap())?;
        Some(ids.iter().map(|&iid| self.node_user_ids[iid as usize].clone()).collect())
    }

    fn directed_edges_with_label(&self, label: &str) -> Option<Vec<String>> {
        self.ensure_edge_label_dir();
        let dir = self.edge_label_dir.borrow();
        let ids = self.label_index_lookup(label, dir.as_ref().unwrap())?;
        Some(ids.iter()
            .filter(|&&iid| self.edge_directed[iid as usize])
            .map(|&iid| self.edge_user_ids[iid as usize].clone())
            .collect())
    }

    fn undirected_edges_with_label(&self, label: &str) -> Option<Vec<String>> {
        self.ensure_edge_label_dir();
        let dir = self.edge_label_dir.borrow();
        let ids = self.label_index_lookup(label, dir.as_ref().unwrap())?;
        Some(ids.iter()
            .filter(|&&iid| !self.edge_directed[iid as usize])
            .map(|&iid| self.edge_user_ids[iid as usize].clone())
            .collect())
    }

    fn outgoing_edges(&self, node_id: &str) -> Vec<String> {
        if let Some(&niid) = self.node_id_cache.borrow().get(node_id) {
            self.get_adj_entries(niid, ADJ_OUTGOING).iter()
                .map(|&eiid| self.edge_user_ids[eiid as usize].clone())
                .collect()
        } else {
            vec![]
        }
    }

    fn incoming_edges(&self, node_id: &str) -> Vec<String> {
        if let Some(&niid) = self.node_id_cache.borrow().get(node_id) {
            self.get_adj_entries(niid, ADJ_INCOMING).iter()
                .map(|&eiid| self.edge_user_ids[eiid as usize].clone())
                .collect()
        } else {
            vec![]
        }
    }

    fn undirected_edges_of(&self, node_id: &str) -> Vec<String> {
        if let Some(&niid) = self.node_id_cache.borrow().get(node_id) {
            self.get_adj_entries(niid, ADJ_UNDIRECTED).iter()
                .map(|&eiid| self.edge_user_ids[eiid as usize].clone())
                .collect()
        } else {
            vec![]
        }
    }
}

fn cell_bounds(page: &crate::pager::page::Page, index: u16) -> (usize, usize) {
    let offset = page.cell_offset(index).unwrap() as usize;
    let end = if index == 0 {
        crate::pager::page::PAGE_SIZE
    } else {
        page.cell_offset(index - 1).unwrap() as usize
    };
    (offset, end)
}

fn collect_st_pages(pager: &mut Pager) -> io::Result<Vec<u32>> {
    let mut pages = Vec::new();
    let count = pager.header.page_count;
    for pg in 1..count {
        let page = pager.read_page(pg)?;
        if page.page_type() == PageType::StringTable {
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

    fn disk_fraud_run(query: &str) -> usize {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        query.hash(&mut h);
        let db_path = temp_path(&format!("disk_fraud_{}.gql", h.finish()));
        cleanup(&db_path);

        let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
        let graph = Graph::from_file(&json_path).unwrap();
        graph.save(&db_path).unwrap();

        let store = DiskGraphStore::open(&db_path).unwrap();
        let rt = Runtime::new(&store);
        let pattern = compile(query).unwrap();
        let result = rt.run(&pattern).rows.len();

        cleanup(&db_path);
        result
    }

    #[test]
    fn test_disk_basic() {
        assert_eq!(disk_fraud_run("()"), 5);
        assert_eq!(disk_fraud_run("(x: Account)"), 4);
        assert_eq!(disk_fraud_run("-[]->"), 5);
        assert_eq!(disk_fraud_run("-[:Transfer]->"), 4);
    }

    #[test]
    fn test_disk_traversal() {
        assert_eq!(disk_fraud_run("()-[]->"), 5);
        assert_eq!(disk_fraud_run("(x)-[:Foo]->"), 1);
        assert_eq!(disk_fraud_run("()-[]->()-[]->()"), 5);
    }

    #[test]
    fn test_disk_filters() {
        assert_eq!(disk_fraud_run("(y WHERE y.isBlocked=true)"), 1);
        assert_eq!(disk_fraud_run("(y WHERE y.isBlocked=false)"), 4);
        assert_eq!(disk_fraud_run("(x WHERE x.isDummy is bool)"), 1);
    }

    #[test]
    fn test_disk_complex() {
        assert_eq!(
            disk_fraud_run("(x) -[z:Transfer WHERE z.amount>1000000]-> (y WHERE y.isBlocked=true)"),
            1
        );
        assert_eq!(disk_fraud_run("(x: Dummy) | (y: Account)"), 5);
        assert_eq!(disk_fraud_run("-->{1,2}"), 23);
        assert_eq!(disk_fraud_run("(x: Dummy & Person)"), 1);
    }
}
