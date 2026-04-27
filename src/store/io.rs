//! Serialization: save a Graph to a .gql file and load it back.

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

    // Track record locations for the fast-open index
    let mut node_locs: Vec<(u32, u16)> = Vec::new();
    let mut edge_locs: Vec<(u32, u16)> = Vec::new();

    // Write nodes
    for (nid, name) in graph.node_names.iter().enumerate() {
        let labels = Graph::label_strings(&graph.node_labels[nid]);
        let user_id_sid = strings.intern(name, &mut pager)?;
        let label_sids = intern_strings(&labels, &mut strings, &mut pager)?;
        let encoded_props = encode_props(&graph.node_props[nid], &mut strings, &mut pager)?;
        let cell = record::encode_node(user_id_sid, &label_sids, &encoded_props);
        let loc = store_cell(&mut pager, PageType::NodeData, &cell, &mut node_pages)?;
        node_locs.push(loc);
    }

    // Write edges
    for (eid, name) in graph.edge_names.iter().enumerate() {
        let labels = Graph::label_strings(&graph.edge_labels[eid]);
        let user_id_sid = strings.intern(name, &mut pager)?;
        let label_sids = intern_strings(&labels, &mut strings, &mut pager)?;
        let encoded_props = encode_props(&graph.edge_props[eid], &mut strings, &mut pager)?;
        let cell = record::encode_edge(
            user_id_sid,
            &label_sids,
            &encoded_props,
            graph.edge_src[eid],
            graph.edge_tgt[eid],
            graph.edge_directed[eid],
        );
        let loc = store_cell(&mut pager, PageType::EdgeData, &cell, &mut edge_pages)?;
        edge_locs.push(loc);
    }

    // --- Write on-disk indexes ---

    // Label index
    let mut label_node_map: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut label_edge_map: HashMap<u32, Vec<u32>> = HashMap::new();

    for (nid, lt) in graph.node_labels.iter().enumerate() {
        for l in Graph::label_strings(lt) {
            let sid = strings.intern(&l, &mut pager)?;
            label_node_map.entry(sid).or_default().push(nid as u32);
        }
    }
    for (eid, lt) in graph.edge_labels.iter().enumerate() {
        for l in Graph::label_strings(lt) {
            let sid = strings.intern(&l, &mut pager)?;
            label_edge_map.entry(sid).or_default().push(eid as u32);
        }
    }

    let node_label_entries: Vec<(u32, Vec<u32>)> = label_node_map.into_iter().collect();
    let edge_label_entries: Vec<(u32, Vec<u32>)> = label_edge_map.into_iter().collect();
    let node_label_root = disk_index::write_label_index(&mut pager, &node_label_entries)?;
    let edge_label_root = disk_index::write_label_index(&mut pager, &edge_label_entries)?;

    // Adjacency index
    let mut adj: HashMap<u32, Vec<(u32, u32, u8)>> = HashMap::new();
    for (eid, _) in graph.edge_names.iter().enumerate() {
        let eid32 = eid as u32;
        let src = graph.edge_src[eid];
        let tgt = graph.edge_tgt[eid];
        if graph.edge_directed[eid] {
            adj.entry(src).or_default().push((eid32, tgt, 0));
            adj.entry(tgt).or_default().push((eid32, src, 1));
        } else {
            adj.entry(src).or_default().push((eid32, tgt, 2));
            adj.entry(tgt).or_default().push((eid32, src, 2));
        }
    }
    let adj_entries: Vec<(u32, Vec<(u32, u32, u8)>)> = adj.into_iter().collect();
    let adj_root = disk_index::write_adjacency_index(&mut pager, &adj_entries)?;

    // --- Write string table page directory ---
    let st_page_list = strings.page_numbers().to_vec();
    let st_root = disk_index::write_u32_list(&mut pager, &st_page_list)?;

    // --- Write fast-open indexes (node locs + edge topology) ---
    let node_locs_root = disk_index::write_node_locs(&mut pager, &node_locs)?;
    let edge_topo_root = disk_index::write_edge_topo(
        &mut pager,
        &edge_locs,
        &graph.edge_src,
        &graph.edge_tgt,
        &graph.edge_directed,
    )?;

    // Update header
    pager.header.node_count = graph.node_names.len() as u32;
    pager.header.edge_count = graph.edge_names.len() as u32;
    pager.header.label_index_root = node_label_root;
    pager.header.edge_label_index_root = edge_label_root;
    pager.header.adjacency_root = adj_root;
    pager.header.string_table_root = st_root;
    pager.header.node_locs_root = node_locs_root;
    pager.header.edge_topo_root = edge_topo_root;
    pager.write_header()?;

    Ok(())
}

/// Load a Graph from a .gql database file.
pub fn load_graph(db_path: &Path) -> io::Result<Graph> {
    let mut pager = Pager::open(db_path)?;

    let st_root = pager.header.string_table_root;
    let st_pages = if st_root != 0 {
        disk_index::read_u32_chain(&mut pager, st_root)?
    } else {
        collect_pages_by_type(&mut pager, PageType::StringTable)?
    };
    let strings = StringTable::load(&st_pages, &mut pager)?;

    let mut node_names: Vec<String> = Vec::new();
    let mut node_labels: Vec<LabelType> = Vec::new();
    let mut node_props: Vec<Props> = Vec::new();

    let mut edge_names: Vec<String> = Vec::new();
    let mut edge_labels_vec: Vec<LabelType> = Vec::new();
    let mut edge_props_vec: Vec<Props> = Vec::new();
    let mut edge_src: Vec<u32> = Vec::new();
    let mut edge_tgt: Vec<u32> = Vec::new();
    let mut edge_directed: Vec<bool> = Vec::new();

    let page_count = pager.header.page_count;
    for pg in 1..page_count {
        let page = pager.read_page(pg)?;
        match page.page_type() {
            PageType::NodeData => {
                for i in 0..page.cell_count() {
                    let (offset, end) = cell_bounds(&page, i);
                    let (decoded, _) = record::decode_node(&page.data[offset..end]);
                    let name = strings.resolve(decoded.user_id_str_id).unwrap().to_string();
                    let labs: Vec<String> = decoded
                        .label_str_ids
                        .iter()
                        .map(|&sid| strings.resolve(sid).unwrap().to_string())
                        .collect();
                    let lt = if labs.is_empty() {
                        LabelType::Star
                    } else {
                        LabelType::from_list(&labs)
                    };
                    node_names.push(name);
                    node_labels.push(lt);
                    node_props.push(decode_props(&decoded.props, &strings));
                }
            }
            PageType::EdgeData => {
                for i in 0..page.cell_count() {
                    let (offset, end) = cell_bounds(&page, i);
                    let decoded = record::decode_edge(&page.data[offset..end]);
                    let name = strings
                        .resolve(decoded.node.user_id_str_id)
                        .unwrap()
                        .to_string();
                    let labs: Vec<String> = decoded
                        .node
                        .label_str_ids
                        .iter()
                        .map(|&sid| strings.resolve(sid).unwrap().to_string())
                        .collect();
                    let lt = if labs.is_empty() {
                        LabelType::Star
                    } else {
                        LabelType::from_list(&labs)
                    };
                    edge_names.push(name);
                    edge_labels_vec.push(lt);
                    edge_props_vec.push(decode_props(&decoded.node.props, &strings));
                    edge_src.push(decoded.src_internal_id);
                    edge_tgt.push(decoded.tgt_internal_id);
                    edge_directed.push(decoded.directed);
                }
            }
            _ => {}
        }
    }

    let graph = Graph::from_raw(
        node_names,
        node_labels,
        node_props,
        edge_names,
        edge_labels_vec,
        edge_props_vec,
        edge_src,
        edge_tgt,
        edge_directed,
    );

    Ok(graph)
}

// --- Helpers ---

fn intern_strings(
    strings_list: &[String],
    st: &mut StringTable,
    pager: &mut Pager,
) -> io::Result<Vec<u32>> {
    strings_list.iter().map(|s| st.intern(s, pager)).collect()
}

fn encode_props(
    props: &Props,
    st: &mut StringTable,
    pager: &mut Pager,
) -> io::Result<Vec<(u32, PropValue)>> {
    let mut result = Vec::new();
    for (k, v) in props {
        let name_sid = st.intern(k, pager)?;
        let pv = value_to_prop(v, st, pager)?;
        result.push((name_sid, pv));
    }
    Ok(result)
}

fn value_to_prop(v: &Value, st: &mut StringTable, pager: &mut Pager) -> io::Result<PropValue> {
    Ok(match v {
        Value::Int(n) => PropValue::Int(*n),
        Value::Float(x) => PropValue::Float(*x),
        Value::Str(s) => PropValue::Str(st.intern(s, pager)?),
        Value::Bool(b) => PropValue::Bool(*b),
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(value_to_prop(it, st, pager)?);
            }
            PropValue::List(out)
        }
        Value::Record(fields) => {
            let mut out = Vec::with_capacity(fields.len());
            for (k, v) in fields {
                let sid = st.intern(k, pager)?;
                out.push((sid, value_to_prop(v, st, pager)?));
            }
            PropValue::Record(out)
        }
    })
}

fn decode_props(encoded: &[(u32, PropValue)], strings: &StringTable) -> Props {
    let mut props = HashMap::new();
    for (name_sid, pv) in encoded {
        let name = strings.resolve(*name_sid).unwrap().to_string();
        props.insert(name, prop_to_value(pv, strings));
    }
    props
}

fn prop_to_value(pv: &PropValue, strings: &StringTable) -> Value {
    match pv {
        PropValue::Int(n) => Value::Int(*n),
        PropValue::Float(x) => Value::Float(*x),
        PropValue::Str(sid) => Value::Str(strings.resolve(*sid).unwrap().to_string()),
        PropValue::Bool(b) => Value::Bool(*b),
        PropValue::List(items) => {
            Value::List(items.iter().map(|it| prop_to_value(it, strings)).collect())
        }
        PropValue::Record(fields) => {
            let mut m = std::collections::BTreeMap::new();
            for (sid, v) in fields {
                let name = strings.resolve(*sid).unwrap().to_string();
                m.insert(name, prop_to_value(v, strings));
            }
            Value::Record(m)
        }
    }
}

fn store_cell(
    pager: &mut Pager,
    page_type: PageType,
    cell: &[u8],
    pages: &mut Vec<u32>,
) -> io::Result<(u32, u16)> {
    if let Some(&last_pg) = pages.last() {
        let mut page = pager.read_page(last_pg)?;
        if let Some(cell_idx) = page.insert_cell(cell) {
            pager.write_page(last_pg, &page)?;
            return Ok((last_pg, cell_idx));
        }
    }
    let pg = pager.allocate_page()?;
    let mut page = Page::new(page_type);
    let cell_idx = page
        .insert_cell(cell)
        .ok_or_else(|| io::Error::other("cell too large for page"))?;
    pager.write_page(pg, &page)?;
    pages.push(pg);
    Ok((pg, cell_idx))
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
