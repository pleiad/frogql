//! Serialization: save a MemoryGraphStore to a .gql file and load it back.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use crate::model::graph::{MemoryGraphStore, Props};
use crate::model::value::Value;
use crate::pager::page::{Page, PageType, PAGE_SIZE};
use crate::pager::pager::Pager;
use crate::typing::label_type::LabelType;

use super::disk_index;
use super::record::{self, PropValue};
use super::string_table::StringTable;

/// Atomic save: write the graph to `<db_path>.tmp` first, then `rename` it
/// over the destination. ISO doesn't mandate atomicity here, but a crash
/// in the middle of a multi-second `.gdb` rewrite would otherwise corrupt
/// the file. The rename is atomic on POSIX and on Windows ≥ 8 when the
/// target sits on the same filesystem (`Pager::create` only ever opens
/// local paths). Falls back to a non-atomic write if the directory is
/// not writable for the temp file (rare but worth a clear error).
pub fn save_graph_atomic(graph: &MemoryGraphStore, db_path: &Path) -> io::Result<()> {
    let mut tmp = db_path.to_path_buf().into_os_string();
    tmp.push(".tmp");
    let tmp_path = std::path::PathBuf::from(tmp);
    save_graph(graph, &tmp_path)?;
    std::fs::rename(&tmp_path, db_path)?;
    Ok(())
}

/// Same as `save_graph_atomic` but also persists the GRAPH TYPE catalog
/// into the new file. Used by `LazyGraphStore::save` so the post-save
/// `.gdb` keeps every entry the in-memory catalog had (DEFAULT included).
/// Without this, `.save` followed by reopen would surface "graph type
/// 'DEFAULT' not found" because `save_graph` alone never writes the
/// catalog page chain.
pub fn save_graph_with_catalog_atomic(
    graph: &MemoryGraphStore,
    catalog: &crate::runtime::catalog::GraphTypeCatalog,
    db_path: &Path,
) -> io::Result<()> {
    save_graph_with_catalog_and_indexes_atomic(graph, catalog, &[], db_path)
}

/// Like `save_graph_with_catalog_atomic` but also persists the
/// DDL-declared subset of `index_specs` (entries with `auto = false`)
/// into a fresh `header.secondary_index_root` chain. Auto entries are
/// not written: the open-time `build_auto_indexes_bulk` reproduces
/// them from the data without help. Pass `&[]` (or use the wrapper
/// above) when there's nothing to persist.
pub fn save_graph_with_catalog_and_indexes_atomic(
    graph: &MemoryGraphStore,
    catalog: &crate::runtime::catalog::GraphTypeCatalog,
    index_specs: &[super::secondary_index::IndexSpec],
    db_path: &Path,
) -> io::Result<()> {
    let mut tmp = db_path.to_path_buf().into_os_string();
    tmp.push(".tmp");
    let tmp_path = std::path::PathBuf::from(tmp);
    save_graph(graph, &tmp_path)?;
    // Re-open the temp file, write the catalog and DDL-index list into
    // fresh chains, and update both header pointers. Two-pass write
    // keeps `save_graph` itself unchanged (no catalog plumbing) at the
    // cost of one extra open. The DDL list write is a no-op when
    // `index_specs` has no `auto = false` entries — the chain root
    // stays 0, which the open path treats as "no persisted list".
    {
        let mut pager = Pager::open(&tmp_path)?;
        let root = super::catalog_io::write_catalog(&mut pager, catalog, 0)?;
        pager.header.catalog_root = root;
        let idx_root = super::secondary_index_io::write_specs(&mut pager, index_specs, 0)?;
        pager.header.secondary_index_root = idx_root;
        pager.write_header()?;
    }
    std::fs::rename(&tmp_path, db_path)?;
    Ok(())
}

/// Save a MemoryGraphStore to a .gql database file.
pub fn save_graph(graph: &MemoryGraphStore, db_path: &Path) -> io::Result<()> {
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
        let labels = MemoryGraphStore::label_strings(&graph.node_labels[nid]);
        let user_id_sid = strings.intern(name, &mut pager)?;
        let label_sids = intern_strings(&labels, &mut strings, &mut pager)?;
        let encoded_props = encode_props(&graph.node_props[nid], &mut strings, &mut pager)?;
        let cell = record::encode_node(user_id_sid, &label_sids, &encoded_props);
        let loc = store_cell(&mut pager, PageType::NodeData, &cell, &mut node_pages)?;
        node_locs.push(loc);
    }

    // Write edges
    for (eid, name) in graph.edge_names.iter().enumerate() {
        let labels = MemoryGraphStore::label_strings(&graph.edge_labels[eid]);
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
        for l in MemoryGraphStore::label_strings(lt) {
            let sid = strings.intern(&l, &mut pager)?;
            label_node_map.entry(sid).or_default().push(nid as u32);
        }
    }
    for (eid, lt) in graph.edge_labels.iter().enumerate() {
        for l in MemoryGraphStore::label_strings(lt) {
            let sid = strings.intern(&l, &mut pager)?;
            label_edge_map.entry(sid).or_default().push(eid as u32);
        }
    }

    let node_label_entries: Vec<(u32, Vec<u32>)> = label_node_map.into_iter().collect();
    let edge_label_entries: Vec<(u32, Vec<u32>)> = label_edge_map.into_iter().collect();
    let node_label_root = disk_index::write_label_index(&mut pager, &node_label_entries)?;
    let edge_label_root = disk_index::write_label_index(&mut pager, &edge_label_entries)?;

    // Adjacency: build CSR (offsets + flat per direction) AND the legacy
    // per-node format. Old readers see `adjacency_root`; new readers prefer
    // `csr_adjacency_root`. Both consume the same source data, so they stay
    // in sync.
    let node_count = graph.node_names.len();
    let mut out_pairs: Vec<(u32, u32)> = Vec::new();
    let mut in_pairs: Vec<(u32, u32)> = Vec::new();
    let mut und_pairs: Vec<(u32, u32)> = Vec::new();
    let mut adj: HashMap<u32, Vec<(u32, u32, u8)>> = HashMap::new();
    for (eid, _) in graph.edge_names.iter().enumerate() {
        let eid32 = eid as u32;
        let src = graph.edge_src[eid];
        let tgt = graph.edge_tgt[eid];
        if graph.edge_directed[eid] {
            adj.entry(src).or_default().push((eid32, tgt, 0));
            adj.entry(tgt).or_default().push((eid32, src, 1));
            out_pairs.push((src, eid32));
            in_pairs.push((tgt, eid32));
        } else {
            adj.entry(src).or_default().push((eid32, tgt, 2));
            adj.entry(tgt).or_default().push((eid32, src, 2));
            und_pairs.push((src, eid32));
            und_pairs.push((tgt, eid32));
        }
    }
    // Matches disk_index::write_adjacency_index's parameter shape (file format).
    #[allow(clippy::type_complexity)]
    let adj_entries: Vec<(u32, Vec<(u32, u32, u8)>)> = adj.into_iter().collect();
    let adj_root = disk_index::write_adjacency_index(&mut pager, &adj_entries)?;

    // Build CSR offsets+flat arrays for each direction. Bucket-sort: count,
    // prefix-sum, then place using a cursor.
    let csr = |pairs: &[(u32, u32)]| -> (Vec<u32>, Vec<u32>) {
        let mut offsets = vec![0u32; node_count + 1];
        for (n, _) in pairs {
            offsets[*n as usize + 1] += 1;
        }
        for i in 1..offsets.len() {
            offsets[i] += offsets[i - 1];
        }
        let mut flat = vec![0u32; pairs.len()];
        let mut cursor = offsets[..node_count].to_vec();
        for (n, e) in pairs {
            let pos = cursor[*n as usize] as usize;
            flat[pos] = *e;
            cursor[*n as usize] += 1;
        }
        (offsets, flat)
    };
    let (out_off, out_fl) = csr(&out_pairs);
    let (in_off, in_fl) = csr(&in_pairs);
    let (und_off, und_fl) = csr(&und_pairs);
    let csr_adj_root = disk_index::write_adjacency_csr(
        &mut pager, &out_off, &out_fl, &in_off, &in_fl, &und_off, &und_fl,
    )?;

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
    pager.header.csr_adjacency_root = csr_adj_root;
    pager.header.string_table_root = st_root;
    pager.header.node_locs_root = node_locs_root;
    pager.header.edge_topo_root = edge_topo_root;
    pager.write_header()?;

    Ok(())
}

/// Load a MemoryGraphStore from a .gql database file.
pub fn load_graph(db_path: &Path) -> io::Result<MemoryGraphStore> {
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

    let graph = MemoryGraphStore::from_raw(
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
        // Null is encoded as absence: skip the key entirely so the on-disk
        // record does not carry a sentinel value for it. Loading the same
        // record back yields a `Props` without that key, which the engine
        // already treats as null.
        if v.is_null() {
            continue;
        }
        let name_sid = st.intern(k, pager)?;
        let pv = value_to_prop(v, st, pager)?;
        result.push((name_sid, pv));
    }
    Ok(result)
}

fn value_to_prop(v: &Value, st: &mut StringTable, pager: &mut Pager) -> io::Result<PropValue> {
    Ok(match v {
        // Top-level Null is filtered out by `encode_props` (the key is
        // omitted entirely); reaching here means a Null nested inside a
        // list or record. The wire format reserves `VALUE_TYPE_NULL` for
        // this case so positional alignment is preserved.
        Value::Null => PropValue::Null,
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
        // Node/Edge/Path are runtime-only values (§4.4.4 reference
        // values and the §4.4 path value). They can appear in
        // projections (`RETURN n`, `RETURN p`) and predicates
        // (`WHERE n <> m`) but are never written back to disk as
        // property data — one inside a list literal would be a
        // programming error in the runtime, not user input.
        Value::Node(_) | Value::Edge(_) | Value::Path(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "node / edge reference values cannot be stored as properties",
            ));
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
        PropValue::Null => Value::Null,
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
