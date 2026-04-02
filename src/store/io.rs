//! Serialization: save a Graph to a .gql file and load it back.
//!
//! The .gql file format uses the pager (4KB pages) with:
//! - Page 0: file header
//! - StringTable pages: deduplicated strings
//! - NodeData pages: encoded node records
//! - EdgeData pages: encoded edge records

use std::collections::HashMap;
use std::io;
use std::path::Path;

use crate::model::graph::{Graph, Props};
use crate::model::value::Value;
use crate::pager::page::{Page, PageType, PAGE_SIZE};
use crate::pager::pager::Pager;
use crate::typing::label_type::LabelType;

use super::disk_index;
use super::record::{self, PropValue};
use super::string_table::StringTable;

/// Save a Graph to a .gql database file.
pub fn save_graph(graph: &Graph, db_path: &Path) -> io::Result<()> {
    let mut pager = Pager::create(db_path)?;
    let mut strings = StringTable::new();
    strings.init(&mut pager)?;

    let mut node_pages: Vec<u32> = Vec::new();
    let mut edge_pages: Vec<u32> = Vec::new();

    // Write nodes
    for node_id in &graph.nodes {
        let labels = extract_label_strings(&graph.labels[node_id]);
        let user_id_sid = strings.intern(node_id, &mut pager)?;
        let label_sids = intern_strings(&labels, &mut strings, &mut pager)?;
        let encoded_props = encode_props(&graph.props[node_id], &mut strings, &mut pager)?;
        let cell = record::encode_node(user_id_sid, &label_sids, &encoded_props);
        store_cell(&mut pager, PageType::NodeData, &cell, &mut node_pages)?;
    }

    // Write edges (directed then undirected)
    let all_edges: Vec<(&str, bool)> = graph.edges_d.iter().map(|id| (id.as_str(), true))
        .chain(graph.edges_u.iter().map(|id| (id.as_str(), false)))
        .collect();

    // Build node_id → internal index for endpoint references
    let node_idx: HashMap<&str, u32> = graph.nodes.iter().enumerate()
        .map(|(i, id)| (id.as_str(), i as u32))
        .collect();

    for (edge_id, directed) in &all_edges {
        let labels = extract_label_strings(&graph.labels[*edge_id]);
        let (ep0, ep1) = &graph.endpoints[*edge_id];
        let src_iid = node_idx[ep0.as_str()];
        let tgt_iid = node_idx[ep1.as_str()];

        let user_id_sid = strings.intern(edge_id, &mut pager)?;
        let label_sids = intern_strings(&labels, &mut strings, &mut pager)?;
        let encoded_props = encode_props(&graph.props[*edge_id], &mut strings, &mut pager)?;
        let cell = record::encode_edge(user_id_sid, &label_sids, &encoded_props, src_iid, tgt_iid, *directed);
        store_cell(&mut pager, PageType::EdgeData, &cell, &mut edge_pages)?;
    }

    // --- Write on-disk indexes ---

    // Label index: collect label_sid → vec of internal_ids (both nodes and edges)
    let mut label_node_map: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut label_edge_map: HashMap<u32, Vec<u32>> = HashMap::new();

    for (i, node_id) in graph.nodes.iter().enumerate() {
        for l in extract_label_strings(&graph.labels[node_id]) {
            let sid = strings.intern(&l, &mut pager)?;
            label_node_map.entry(sid).or_default().push(i as u32);
        }
    }

    let mut edge_idx = 0u32;
    for edge_id in graph.edges_d.iter().chain(graph.edges_u.iter()) {
        for l in extract_label_strings(&graph.labels[edge_id]) {
            let sid = strings.intern(&l, &mut pager)?;
            label_edge_map.entry(sid).or_default().push(edge_idx);
        }
        edge_idx += 1;
    }

    // Combine into one label index (tag node IDs with high bit to distinguish)
    // Actually, keep separate: write two label indexes, store both roots.
    let node_label_entries: Vec<(u32, Vec<u32>)> = label_node_map.into_iter().collect();
    let edge_label_entries: Vec<(u32, Vec<u32>)> = label_edge_map.into_iter().collect();

    let node_label_root = disk_index::write_label_index(&mut pager, &node_label_entries)?;
    let edge_label_root = disk_index::write_label_index(&mut pager, &edge_label_entries)?;

    // Adjacency index: node_iid → vec of (edge_iid, other_node_iid, kind)
    let mut adj: HashMap<u32, Vec<(u32, u32, u8)>> = HashMap::new();
    edge_idx = 0;
    for edge_id in &graph.edges_d {
        let src_iid = node_idx[graph.src[edge_id].as_str()];
        let tgt_iid = node_idx[graph.tgt[edge_id].as_str()];
        adj.entry(src_iid).or_default().push((edge_idx, tgt_iid, 0)); // outgoing
        adj.entry(tgt_iid).or_default().push((edge_idx, src_iid, 1)); // incoming
        edge_idx += 1;
    }
    for edge_id in &graph.edges_u {
        let (ep0, ep1) = &graph.endpoints[edge_id];
        let iid0 = node_idx[ep0.as_str()];
        let iid1 = node_idx[ep1.as_str()];
        adj.entry(iid0).or_default().push((edge_idx, iid1, 2)); // undirected
        adj.entry(iid1).or_default().push((edge_idx, iid0, 2)); // undirected
        edge_idx += 1;
    }

    let adj_entries: Vec<(u32, Vec<(u32, u32, u8)>)> = adj.into_iter().collect();
    let adj_root = disk_index::write_adjacency_index(&mut pager, &adj_entries)?;

    // ID indexes: sorted (user_id_string_id, internal_id) for binary search
    let mut node_id_entries: Vec<(u32, u32)> = Vec::new();
    for (i, nid) in graph.nodes.iter().enumerate() {
        let sid = strings.intern(nid, &mut pager)?;
        node_id_entries.push((sid, i as u32));
    }
    node_id_entries.sort_by_key(|(sid, _)| *sid);
    let node_id_root = disk_index::write_id_index(&mut pager, &node_id_entries)?;

    let mut edge_id_entries: Vec<(u32, u32)> = Vec::new();
    edge_idx = 0;
    for edge_id in graph.edges_d.iter().chain(graph.edges_u.iter()) {
        let sid = strings.intern(edge_id, &mut pager)?;
        edge_id_entries.push((sid, edge_idx));
        edge_idx += 1;
    }
    edge_id_entries.sort_by_key(|(sid, _)| *sid);
    let edge_id_root = disk_index::write_id_index(&mut pager, &edge_id_entries)?;

    // Update header
    pager.header.node_count = graph.nodes.len() as u32;
    pager.header.edge_count = (graph.edges_d.len() + graph.edges_u.len()) as u32;
    pager.header.label_index_root = node_label_root;
    pager.header.edge_label_index_root = edge_label_root;
    pager.header.adjacency_root = adj_root;
    pager.header.node_id_index_root = node_id_root;
    pager.header.edge_id_index_root = edge_id_root;
    pager.write_header()?;

    Ok(())
}

/// Load a Graph from a .gql database file.
pub fn load_graph(db_path: &Path) -> io::Result<Graph> {
    let mut pager = Pager::open(db_path)?;

    // Load string table
    let st_pages = collect_pages_by_type(&mut pager, PageType::StringTable)?;
    let strings = StringTable::load(&st_pages, &mut pager)?;

    // Collect node and edge data
    let mut node_ids: Vec<String> = Vec::new();
    let mut edge_data: Vec<(String, Vec<String>, Props, u32, u32, bool)> = Vec::new();
    let mut all_labels: HashMap<String, LabelType> = HashMap::new();
    let mut all_props: HashMap<String, Props> = HashMap::new();

    let page_count = pager.header.page_count;
    for pg in 1..page_count {
        let page = pager.read_page(pg)?;
        match page.page_type() {
            PageType::NodeData => {
                for i in 0..page.cell_count() {
                    let (offset, end) = cell_bounds(&page, i);
                    let (decoded, _) = record::decode_node(&page.data[offset..end]);
                    let user_id = strings.resolve(decoded.user_id_str_id).unwrap().to_string();
                    let labels: Vec<String> = decoded.label_str_ids.iter()
                        .map(|&sid| strings.resolve(sid).unwrap().to_string())
                        .collect();
                    let props = decode_props(&decoded.props, &strings);

                    all_labels.insert(user_id.clone(), LabelType::from_list(&labels));
                    all_props.insert(user_id.clone(), props);
                    node_ids.push(user_id);
                }
            }
            PageType::EdgeData => {
                for i in 0..page.cell_count() {
                    let (offset, end) = cell_bounds(&page, i);
                    let decoded = record::decode_edge(&page.data[offset..end]);
                    let user_id = strings.resolve(decoded.node.user_id_str_id).unwrap().to_string();
                    let labels: Vec<String> = decoded.node.label_str_ids.iter()
                        .map(|&sid| strings.resolve(sid).unwrap().to_string())
                        .collect();
                    let props = decode_props(&decoded.node.props, &strings);

                    all_labels.insert(user_id.clone(), LabelType::from_list(&labels));
                    all_props.insert(user_id.clone(), props);
                    edge_data.push((
                        user_id,
                        labels,
                        HashMap::new(), // props already in all_props
                        decoded.src_internal_id,
                        decoded.tgt_internal_id,
                        decoded.directed,
                    ));
                }
            }
            _ => {}
        }
    }

    // Rebuild Graph struct
    let mut edges_d = Vec::new();
    let mut edges_u = Vec::new();
    let mut endpoints = HashMap::new();
    let mut src_map = HashMap::new();
    let mut tgt_map = HashMap::new();

    for (edge_id, _labels, _props, src_iid, tgt_iid, directed) in &edge_data {
        let src_user = &node_ids[*src_iid as usize];
        let tgt_user = &node_ids[*tgt_iid as usize];
        endpoints.insert(edge_id.clone(), (src_user.clone(), tgt_user.clone()));

        if *directed {
            src_map.insert(edge_id.clone(), src_user.clone());
            tgt_map.insert(edge_id.clone(), tgt_user.clone());
            edges_d.push(edge_id.clone());
        } else {
            edges_u.push(edge_id.clone());
        }
    }

    // Build the Graph through its JSON constructor logic won't work (we don't have JSON).
    // Instead, build it directly and compute indexes.
    let graph = Graph::from_raw(
        node_ids, edges_d, edges_u,
        all_labels, all_props,
        endpoints, src_map, tgt_map,
    );

    Ok(graph)
}

// --- Helpers ---

fn extract_label_strings(lt: &LabelType) -> Vec<String> {
    match lt {
        LabelType::Label(s) => vec![s.clone()],
        LabelType::And(a, b) => {
            let mut v = extract_label_strings(a);
            v.extend(extract_label_strings(b));
            v
        }
        _ => vec![],
    }
}

fn intern_strings(strings_list: &[String], st: &mut StringTable, pager: &mut Pager) -> io::Result<Vec<u32>> {
    strings_list.iter().map(|s| st.intern(s, pager)).collect()
}

fn encode_props(props: &Props, st: &mut StringTable, pager: &mut Pager) -> io::Result<Vec<(u32, PropValue)>> {
    let mut result = Vec::new();
    for (k, v) in props {
        let name_sid = st.intern(k, pager)?;
        let pv = match v {
            Value::Int(n) => PropValue::Int(*n),
            Value::Str(s) => PropValue::Str(st.intern(s, pager)?),
            Value::Bool(b) => PropValue::Bool(*b),
        };
        result.push((name_sid, pv));
    }
    Ok(result)
}

fn decode_props(encoded: &[(u32, PropValue)], strings: &StringTable) -> Props {
    let mut props = HashMap::new();
    for (name_sid, pv) in encoded {
        let name = strings.resolve(*name_sid).unwrap().to_string();
        let val = match pv {
            PropValue::Int(n) => Value::Int(*n),
            PropValue::Str(sid) => Value::Str(strings.resolve(*sid).unwrap().to_string()),
            PropValue::Bool(b) => Value::Bool(*b),
        };
        props.insert(name, val);
    }
    props
}

fn store_cell(pager: &mut Pager, page_type: PageType, cell: &[u8], pages: &mut Vec<u32>) -> io::Result<()> {
    if let Some(&last_pg) = pages.last() {
        let mut page = pager.read_page(last_pg)?;
        if page.insert_cell(cell).is_some() {
            pager.write_page(last_pg, &page)?;
            return Ok(());
        }
    }
    let pg = pager.allocate_page()?;
    let mut page = Page::new(page_type);
    page.insert_cell(cell)
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "cell too large for page"))?;
    pager.write_page(pg, &page)?;
    pages.push(pg);
    Ok(())
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
