use std::collections::HashMap;
use std::io;
use std::path::Path;

use crate::model::graph::Props;
use crate::model::graph_access::GraphAccess;
use crate::model::value::{PathValue, Value};
use crate::pager::page::{Page, PageType};
use crate::pager::pager::Pager;
use crate::typing::label_type::LabelType;

use super::record::{self, DecodedEdge, DecodedNode, PropValue};
use super::string_table::StringTable;

/// A file-backed graph store.
///
/// On creation/import, graph data is written into the database file via the pager.
/// On open, in-memory indexes are rebuilt from the stored pages.
///
/// In-memory state (rebuilt on open):
/// - `strings`: deduplicated string table
/// - `node_ids`: user string ID → internal u32 index
/// - `edge_ids`: user string ID → internal u32 index
/// - `nodes`: sequential list of decoded node info
/// - `edges`: sequential list of decoded edge info
/// - `label_to_nodes`: label string → list of internal node indices
/// - `label_to_edges`: label string → list of internal edge indices
/// - `outgoing`: node internal id → list of edge internal ids
/// - `incoming`: node internal id → list of edge internal ids
/// - `undirected`: node internal id → list of edge internal ids
pub struct GraphStore {
    pager: Pager,
    pub strings: StringTable,

    // In-memory graph data (mirrors what's on disk)
    node_user_ids: Vec<String>,      // internal_id → user ID string
    edge_user_ids: Vec<String>,      // internal_id → user ID string
    node_id_map: HashMap<String, u32>, // user ID → internal_id
    edge_id_map: HashMap<String, u32>,

    node_labels: Vec<LabelType>,      // internal_id → label
    edge_labels: Vec<LabelType>,
    node_props: Vec<Props>,           // internal_id → properties
    edge_props: Vec<Props>,

    edge_src: Vec<u32>,               // edge internal_id → src node internal_id
    edge_tgt: Vec<u32>,               // edge internal_id → tgt node internal_id
    edge_directed: Vec<bool>,         // edge internal_id → is directed?

    // Indexes
    label_to_nodes: HashMap<String, Vec<u32>>,
    label_to_edges: HashMap<String, Vec<u32>>,
    outgoing: HashMap<u32, Vec<u32>>,   // node → outgoing directed edge ids
    incoming: HashMap<u32, Vec<u32>>,   // node → incoming directed edge ids
    undirected_adj: HashMap<u32, Vec<u32>>, // node → undirected edge ids

    // Page tracking for node/edge data
    node_pages: Vec<u32>,
    edge_pages: Vec<u32>,
}

impl GraphStore {
    /// Create a new database file and import a graph from JSON.
    pub fn from_json_file(db_path: &Path, json_path: &Path) -> io::Result<Self> {
        let json_str = std::fs::read_to_string(json_path)?;
        let json: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Self::from_json_value(db_path, &json)
    }

    pub fn from_json_value(db_path: &Path, json: &serde_json::Value) -> io::Result<Self> {
        let mut pager = Pager::create(db_path)?;
        let mut strings = StringTable::new();
        let st_root = strings.init(&mut pager)?;
        pager.header.string_table_root = st_root;

        let mut store = GraphStore {
            pager,
            strings,
            node_user_ids: Vec::new(),
            edge_user_ids: Vec::new(),
            node_id_map: HashMap::new(),
            edge_id_map: HashMap::new(),
            node_labels: Vec::new(),
            edge_labels: Vec::new(),
            node_props: Vec::new(),
            edge_props: Vec::new(),
            edge_src: Vec::new(),
            edge_tgt: Vec::new(),
            edge_directed: Vec::new(),
            label_to_nodes: HashMap::new(),
            label_to_edges: HashMap::new(),
            outgoing: HashMap::new(),
            incoming: HashMap::new(),
            undirected_adj: HashMap::new(),
            node_pages: Vec::new(),
            edge_pages: Vec::new(),
        };

        // Import nodes
        let json_nodes = json["nodes"].as_array()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing nodes"))?;
        for n in json_nodes {
            let id = n["id"].as_str().unwrap();
            let labels: Vec<String> = n["labels"].as_array().unwrap()
                .iter().map(|l| l.as_str().unwrap().to_string()).collect();
            let props = Self::parse_json_props(&n["props"]);
            store.insert_node(id, &labels, &props)?;
        }

        // Import edges
        let json_edges = json["edges"].as_array()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing edges"))?;
        for e in json_edges {
            let id = e["id"].as_str().unwrap();
            let labels: Vec<String> = e["labels"].as_array().unwrap()
                .iter().map(|l| l.as_str().unwrap().to_string()).collect();
            let props = Self::parse_json_props(&e["props"]);
            let eps = e["endpoints"].as_array().unwrap();
            let src = eps[0].as_str().unwrap();
            let tgt = eps[1].as_str().unwrap();
            let directed = e["directionality"].as_str().unwrap() == "->";
            store.insert_edge(id, &labels, &props, src, tgt, directed)?;
        }

        // Update header
        store.pager.header.node_count = store.node_user_ids.len() as u32;
        store.pager.header.edge_count = store.edge_user_ids.len() as u32;
        store.pager.write_header()?;

        Ok(store)
    }

    /// Open an existing database and rebuild in-memory state.
    pub fn open(db_path: &Path) -> io::Result<Self> {
        let mut pager = Pager::open(db_path)?;

        // Load string table
        let st_root = pager.header.string_table_root;
        let st_pages = Self::collect_string_table_pages(st_root, &mut pager)?;
        let strings = StringTable::load(&st_pages, &mut pager)?;

        let mut store = GraphStore {
            pager,
            strings,
            node_user_ids: Vec::new(),
            edge_user_ids: Vec::new(),
            node_id_map: HashMap::new(),
            edge_id_map: HashMap::new(),
            node_labels: Vec::new(),
            edge_labels: Vec::new(),
            node_props: Vec::new(),
            edge_props: Vec::new(),
            edge_src: Vec::new(),
            edge_tgt: Vec::new(),
            edge_directed: Vec::new(),
            label_to_nodes: HashMap::new(),
            label_to_edges: HashMap::new(),
            outgoing: HashMap::new(),
            incoming: HashMap::new(),
            undirected_adj: HashMap::new(),
            node_pages: Vec::new(),
            edge_pages: Vec::new(),
        };

        // Scan all pages to rebuild state
        let page_count = store.pager.header.page_count;
        for pg in 1..page_count {
            let page = store.pager.read_page(pg)?;
            match page.page_type() {
                PageType::NodeData => {
                    store.node_pages.push(pg);
                    store.load_nodes_from_page(&page);
                }
                PageType::EdgeData => {
                    store.edge_pages.push(pg);
                    store.load_edges_from_page(&page);
                }
                _ => {}
            }
        }

        Ok(store)
    }

    // --- Insert operations ---

    fn insert_node(&mut self, user_id: &str, labels: &[String], props: &Props) -> io::Result<u32> {
        let internal_id = self.node_user_ids.len() as u32;

        // Intern strings
        let user_id_sid = self.strings.intern(user_id, &mut self.pager)?;
        let mut label_sids = Vec::new();
        for l in labels {
            label_sids.push(self.strings.intern(l, &mut self.pager)?);
        }
        let encoded_props = self.encode_props(props)?;

        // Encode and store on a page
        let cell = record::encode_node(user_id_sid, &label_sids, &encoded_props);
        self.store_cell_on_page(PageType::NodeData, &cell, &mut self.node_pages.clone())?;

        // Update in-memory state
        self.node_user_ids.push(user_id.to_string());
        self.node_id_map.insert(user_id.to_string(), internal_id);
        self.node_labels.push(LabelType::from_list(labels));
        self.node_props.push(props.clone());

        // Label index
        for l in labels {
            self.label_to_nodes.entry(l.clone()).or_default().push(internal_id);
        }

        Ok(internal_id)
    }

    fn insert_edge(
        &mut self, user_id: &str, labels: &[String], props: &Props,
        src_user_id: &str, tgt_user_id: &str, directed: bool,
    ) -> io::Result<u32> {
        let internal_id = self.edge_user_ids.len() as u32;
        let src_iid = *self.node_id_map.get(src_user_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData,
                format!("unknown source node '{src_user_id}'")))?;
        let tgt_iid = *self.node_id_map.get(tgt_user_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData,
                format!("unknown target node '{tgt_user_id}'")))?;

        let user_id_sid = self.strings.intern(user_id, &mut self.pager)?;
        let mut label_sids = Vec::new();
        for l in labels {
            label_sids.push(self.strings.intern(l, &mut self.pager)?);
        }
        let encoded_props = self.encode_props(props)?;

        let cell = record::encode_edge(user_id_sid, &label_sids, &encoded_props, src_iid, tgt_iid, directed);
        self.store_cell_on_page(PageType::EdgeData, &cell, &mut self.edge_pages.clone())?;

        // In-memory state
        self.edge_user_ids.push(user_id.to_string());
        self.edge_id_map.insert(user_id.to_string(), internal_id);
        self.edge_labels.push(LabelType::from_list(labels));
        self.edge_props.push(props.clone());
        self.edge_src.push(src_iid);
        self.edge_tgt.push(tgt_iid);
        self.edge_directed.push(directed);

        // Label index
        for l in labels {
            self.label_to_edges.entry(l.clone()).or_default().push(internal_id);
        }

        // Adjacency
        if directed {
            self.outgoing.entry(src_iid).or_default().push(internal_id);
            self.incoming.entry(tgt_iid).or_default().push(internal_id);
        } else {
            self.undirected_adj.entry(src_iid).or_default().push(internal_id);
            self.undirected_adj.entry(tgt_iid).or_default().push(internal_id);
        }

        Ok(internal_id)
    }

    // --- Query API (used by runtime via GraphAccess trait) ---

    pub fn all_node_user_ids(&self) -> &[String] {
        &self.node_user_ids
    }

    pub fn all_directed_edge_user_ids(&self) -> Vec<&String> {
        self.edge_user_ids.iter().enumerate()
            .filter(|(i, _)| self.edge_directed[*i])
            .map(|(_, id)| id)
            .collect()
    }

    pub fn all_undirected_edge_user_ids(&self) -> Vec<&String> {
        self.edge_user_ids.iter().enumerate()
            .filter(|(i, _)| !self.edge_directed[*i])
            .map(|(_, id)| id)
            .collect()
    }

    pub fn node_labels_by_user_id(&self, user_id: &str) -> Option<&LabelType> {
        self.node_id_map.get(user_id).map(|&iid| &self.node_labels[iid as usize])
    }

    pub fn edge_labels_by_user_id(&self, user_id: &str) -> Option<&LabelType> {
        self.edge_id_map.get(user_id).map(|&iid| &self.edge_labels[iid as usize])
    }

    pub fn labels_by_user_id(&self, user_id: &str) -> Option<&LabelType> {
        self.node_labels_by_user_id(user_id)
            .or_else(|| self.edge_labels_by_user_id(user_id))
    }

    pub fn props_by_user_id(&self, user_id: &str) -> Option<&Props> {
        if let Some(&iid) = self.node_id_map.get(user_id) {
            Some(&self.node_props[iid as usize])
        } else if let Some(&iid) = self.edge_id_map.get(user_id) {
            Some(&self.edge_props[iid as usize])
        } else {
            None
        }
    }

    pub fn edge_src_user_id(&self, edge_user_id: &str) -> Option<&String> {
        self.edge_id_map.get(edge_user_id).map(|&iid| {
            let src_iid = self.edge_src[iid as usize] as usize;
            &self.node_user_ids[src_iid]
        })
    }

    pub fn edge_tgt_user_id(&self, edge_user_id: &str) -> Option<&String> {
        self.edge_id_map.get(edge_user_id).map(|&iid| {
            let tgt_iid = self.edge_tgt[iid as usize] as usize;
            &self.node_user_ids[tgt_iid]
        })
    }

    pub fn edge_endpoints_user_ids(&self, edge_user_id: &str) -> Option<(&String, &String)> {
        self.edge_id_map.get(edge_user_id).map(|&iid| {
            let src = self.edge_src[iid as usize] as usize;
            let tgt = self.edge_tgt[iid as usize] as usize;
            (&self.node_user_ids[src], &self.node_user_ids[tgt])
        })
    }

    pub fn is_edge_directed(&self, edge_user_id: &str) -> bool {
        self.edge_id_map.get(edge_user_id)
            .map(|&iid| self.edge_directed[iid as usize])
            .unwrap_or(false)
    }

    // --- Internal helpers ---

    fn encode_props(&mut self, props: &Props) -> io::Result<Vec<(u32, PropValue)>> {
        let mut result = Vec::new();
        for (k, v) in props {
            let name_sid = self.strings.intern(k, &mut self.pager)?;
            let pv = match v {
                Value::Int(n) => PropValue::Int(*n),
                Value::Str(s) => PropValue::Str(self.strings.intern(s, &mut self.pager)?),
                Value::Bool(b) => PropValue::Bool(*b),
            };
            result.push((name_sid, pv));
        }
        Ok(result)
    }

    fn store_cell_on_page(
        &mut self, page_type: PageType, cell: &[u8], pages_list: &mut Vec<u32>,
    ) -> io::Result<()> {
        // Try to insert into the last page
        if let Some(&last_pg) = pages_list.last() {
            let mut page = self.pager.read_page(last_pg)?;
            if page.insert_cell(cell).is_some() {
                self.pager.write_page(last_pg, &page)?;
                // Update the actual store's page list
                self.sync_page_list(page_type, pages_list);
                return Ok(());
            }
        }

        // Allocate a new page
        let pg = self.pager.allocate_page()?;
        let mut page = Page::new(page_type);
        page.insert_cell(cell)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "cell too large for page"))?;
        self.pager.write_page(pg, &page)?;
        pages_list.push(pg);
        self.sync_page_list(page_type, pages_list);
        Ok(())
    }

    fn sync_page_list(&mut self, page_type: PageType, pages_list: &[u32]) {
        match page_type {
            PageType::NodeData => self.node_pages = pages_list.to_vec(),
            PageType::EdgeData => self.edge_pages = pages_list.to_vec(),
            _ => {}
        }
    }

    fn load_nodes_from_page(&mut self, page: &Page) {
        for i in 0..page.cell_count() {
            let offset = page.cell_offset(i).unwrap() as usize;
            // We need to know the cell size. Since cells are variable length and
            // packed downward, we compute size from offset to the next cell (or page end).
            let end = if i == 0 {
                crate::pager::page::PAGE_SIZE
            } else {
                page.cell_offset(i - 1).unwrap() as usize
            };
            let cell_data = &page.data[offset..end];
            let (decoded, _) = record::decode_node(cell_data);
            self.rebuild_node(&decoded);
        }
    }

    fn load_edges_from_page(&mut self, page: &Page) {
        for i in 0..page.cell_count() {
            let offset = page.cell_offset(i).unwrap() as usize;
            let end = if i == 0 {
                crate::pager::page::PAGE_SIZE
            } else {
                page.cell_offset(i - 1).unwrap() as usize
            };
            let cell_data = &page.data[offset..end];
            let decoded = record::decode_edge(cell_data);
            self.rebuild_edge(&decoded);
        }
    }

    fn rebuild_node(&mut self, decoded: &DecodedNode) {
        let internal_id = self.node_user_ids.len() as u32;
        let user_id = self.strings.resolve(decoded.user_id_str_id).unwrap().to_string();

        let labels: Vec<String> = decoded.label_str_ids.iter()
            .map(|&sid| self.strings.resolve(sid).unwrap().to_string())
            .collect();

        let props = self.decode_props(&decoded.props);

        self.node_user_ids.push(user_id.clone());
        self.node_id_map.insert(user_id, internal_id);
        self.node_labels.push(LabelType::from_list(&labels));
        self.node_props.push(props);

        for l in &labels {
            self.label_to_nodes.entry(l.clone()).or_default().push(internal_id);
        }
    }

    fn rebuild_edge(&mut self, decoded: &DecodedEdge) {
        let internal_id = self.edge_user_ids.len() as u32;
        let user_id = self.strings.resolve(decoded.node.user_id_str_id).unwrap().to_string();

        let labels: Vec<String> = decoded.node.label_str_ids.iter()
            .map(|&sid| self.strings.resolve(sid).unwrap().to_string())
            .collect();

        let props = self.decode_props(&decoded.node.props);

        self.edge_user_ids.push(user_id.clone());
        self.edge_id_map.insert(user_id, internal_id);
        self.edge_labels.push(LabelType::from_list(&labels));
        self.edge_props.push(props);
        self.edge_src.push(decoded.src_internal_id);
        self.edge_tgt.push(decoded.tgt_internal_id);
        self.edge_directed.push(decoded.directed);

        for l in &labels {
            self.label_to_edges.entry(l.clone()).or_default().push(internal_id);
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

    fn parse_json_props(obj: &serde_json::Value) -> Props {
        let mut props = HashMap::new();
        if let Some(map) = obj.as_object() {
            for (k, v) in map {
                let val = match v {
                    serde_json::Value::String(s) => Value::Str(s.clone()),
                    serde_json::Value::Bool(b) => Value::Bool(*b),
                    serde_json::Value::Number(n) => Value::Int(n.as_i64().unwrap()),
                    _ => continue,
                };
                props.insert(k.clone(), val);
            }
        }
        props
    }

    fn collect_string_table_pages(_root: u32, pager: &mut Pager) -> io::Result<Vec<u32>> {
        // Scan all pages to find string table pages.
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
}

impl GraphAccess for GraphStore {
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
        self.labels_by_user_id(id).expect("unknown element id")
    }
    fn props(&self, id: &str) -> &Props {
        self.props_by_user_id(id).expect("unknown element id")
    }
    fn src(&self, edge_id: &str) -> &str {
        self.edge_src_user_id(edge_id).unwrap()
    }
    fn tgt(&self, edge_id: &str) -> &str {
        self.edge_tgt_user_id(edge_id).unwrap()
    }
    fn endpoints(&self, edge_id: &str) -> (&str, &str) {
        let (a, b) = self.edge_endpoints_user_ids(edge_id).unwrap();
        (a, b)
    }
    fn is_directed(&self, edge_id: &str) -> bool {
        self.is_edge_directed(edge_id)
    }
    fn edge_path_value(&self, edge_id: &str) -> PathValue {
        if self.is_edge_directed(edge_id) {
            PathValue::EdgeDirectional(edge_id.to_string())
        } else {
            PathValue::EdgeUndirectional(edge_id.to_string())
        }
    }
    fn nodes_with_label(&self, label: &str) -> Option<Vec<String>> {
        self.label_to_nodes.get(label).map(|ids| {
            ids.iter().map(|&iid| self.node_user_ids[iid as usize].clone()).collect()
        })
    }
    fn directed_edges_with_label(&self, label: &str) -> Option<Vec<String>> {
        self.label_to_edges.get(label).map(|ids| {
            ids.iter()
                .filter(|&&iid| self.edge_directed[iid as usize])
                .map(|&iid| self.edge_user_ids[iid as usize].clone())
                .collect()
        })
    }
    fn undirected_edges_with_label(&self, label: &str) -> Option<Vec<String>> {
        self.label_to_edges.get(label).map(|ids| {
            ids.iter()
                .filter(|&&iid| !self.edge_directed[iid as usize])
                .map(|&iid| self.edge_user_ids[iid as usize].clone())
                .collect()
        })
    }
    fn outgoing_edges(&self, node_id: &str) -> Vec<String> {
        if let Some(&niid) = self.node_id_map.get(node_id) {
            self.outgoing.get(&niid).map(|eids| {
                eids.iter().map(|&eiid| self.edge_user_ids[eiid as usize].clone()).collect()
            }).unwrap_or_default()
        } else {
            vec![]
        }
    }
    fn incoming_edges(&self, node_id: &str) -> Vec<String> {
        if let Some(&niid) = self.node_id_map.get(node_id) {
            self.incoming.get(&niid).map(|eids| {
                eids.iter().map(|&eiid| self.edge_user_ids[eiid as usize].clone()).collect()
            }).unwrap_or_default()
        } else {
            vec![]
        }
    }
    fn undirected_edges_of(&self, node_id: &str) -> Vec<String> {
        if let Some(&niid) = self.node_id_map.get(node_id) {
            self.undirected_adj.get(&niid).map(|eids| {
                eids.iter().map(|&eiid| self.edge_user_ids[eiid as usize].clone()).collect()
            }).unwrap_or_default()
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("gqlrust_test");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_import_fraud_json() {
        let db_path = temp_path("fraud_import.gql");
        cleanup(&db_path);

        let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");
        let store = GraphStore::from_json_file(&db_path, &json_path).unwrap();

        assert_eq!(store.all_node_user_ids().len(), 5);
        assert_eq!(store.all_directed_edge_user_ids().len(), 5);
        assert_eq!(store.all_undirected_edge_user_ids().len(), 0);

        // Check labels
        assert_eq!(
            store.labels_by_user_id("a1"),
            Some(&LabelType::Label("Account".into()))
        );
        assert_eq!(
            store.labels_by_user_id("d1"),
            Some(&LabelType::And(
                Box::new(LabelType::Label("Dummy".into())),
                Box::new(LabelType::Label("Person".into())),
            ))
        );

        // Check props
        let p = store.props_by_user_id("a1").unwrap();
        assert_eq!(p.get("owner"), Some(&Value::Str("Aretha".into())));

        // Check endpoints
        assert_eq!(store.edge_src_user_id("t1").unwrap(), "p1");
        assert_eq!(store.edge_tgt_user_id("t1").unwrap(), "p2");

        cleanup(&db_path);
    }

    #[test]
    fn test_import_social_json() {
        let db_path = temp_path("social_import.gql");
        cleanup(&db_path);

        let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/social-network.json");
        let store = GraphStore::from_json_file(&db_path, &json_path).unwrap();

        assert_eq!(store.all_node_user_ids().len(), 3);
        assert_eq!(store.all_directed_edge_user_ids().len(), 2);
        assert_eq!(store.all_undirected_edge_user_ids().len(), 1);

        assert!(!store.is_edge_directed("e1")); // Knows is undirected
        assert!(store.is_edge_directed("e2"));  // Likes is directed

        cleanup(&db_path);
    }

    #[test]
    fn test_persistence_roundtrip() {
        let db_path = temp_path("persist_rt.gql");
        cleanup(&db_path);

        let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_data/fraud.json");

        // Import
        {
            let _store = GraphStore::from_json_file(&db_path, &json_path).unwrap();
        }

        // Reopen
        {
            let store = GraphStore::open(&db_path).unwrap();
            assert_eq!(store.all_node_user_ids().len(), 5);
            assert_eq!(store.all_directed_edge_user_ids().len(), 5);

            assert_eq!(
                store.labels_by_user_id("a1"),
                Some(&LabelType::Label("Account".into()))
            );
            let p = store.props_by_user_id("t1").unwrap();
            assert_eq!(p.get("amount"), Some(&Value::Int(2500000)));

            assert_eq!(store.edge_src_user_id("t3").unwrap(), "a2");
            assert_eq!(store.edge_tgt_user_id("t3").unwrap(), "a1");
        }

        cleanup(&db_path);
    }
}
