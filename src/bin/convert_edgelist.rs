//! Convert a SNAP-format edge list to a .gql database file.
//!
//! Usage: convert_edgelist <input.txt> <output.gql> [--limit N]
//!
//! Input format: tab-separated (src_id, tgt_id) per line, lines starting with '#' are skipped.
//! The graph is treated as directed with no labels and no properties.

use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Instant;

use gqlrust::pager::page::{Page, PageType};
use gqlrust::pager::pager::Pager;
use gqlrust::store::disk_index;
use gqlrust::store::record;
use gqlrust::store::string_table::StringTable;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <input.txt> <output.gql> [--limit N]", args[0]);
        std::process::exit(1);
    }

    let input_path = &args[1];
    let output_path = &args[2];

    let mut limit: Option<usize> = None;
    if args.len() >= 5 && args[3] == "--limit" {
        limit = Some(args[4].parse().expect("invalid limit"));
    }

    let t0 = Instant::now();

    // --- Pass 1: Read edges and collect unique nodes ---
    eprintln!("Pass 1: Reading edges...");
    let mut edges: Vec<(u32, u32)> = Vec::new();
    let mut node_set: HashMap<u32, u32> = HashMap::new(); // original_id -> internal_id
    let mut next_iid: u32 = 0;

    let file = File::open(input_path).expect("cannot open input file");
    let reader = BufReader::with_capacity(8 * 1024 * 1024, file);

    for line in reader.lines() {
        let line = line.expect("read error");
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let src: u32 = parts[0].trim().parse().expect("bad src id");
        let tgt: u32 = parts[1].trim().parse().expect("bad tgt id");

        node_set.entry(src).or_insert_with(|| {
            let id = next_iid;
            next_iid += 1;
            id
        });
        node_set.entry(tgt).or_insert_with(|| {
            let id = next_iid;
            next_iid += 1;
            id
        });
        edges.push((src, tgt));

        if let Some(lim) = limit {
            if edges.len() >= lim {
                break;
            }
        }
    }

    let node_count = node_set.len();
    let edge_count = edges.len();
    eprintln!(
        "  {} nodes, {} edges in {:.1}s",
        node_count,
        edge_count,
        t0.elapsed().as_secs_f64()
    );

    // Build sorted node list (by original ID for determinism)
    let mut nodes_sorted: Vec<(u32, u32)> =
        node_set.iter().map(|(&orig, &iid)| (orig, iid)).collect();
    nodes_sorted.sort_by_key(|(orig, _)| *orig);

    // --- Pass 2: Write .gql file ---
    eprintln!("Pass 2: Writing .gql file...");
    let t1 = Instant::now();

    if Path::new(output_path).exists() {
        std::fs::remove_file(output_path).expect("cannot remove existing output");
    }

    let mut pager = Pager::create(Path::new(output_path)).expect("cannot create output");
    let mut strings = StringTable::new();
    strings.init(&mut pager).expect("cannot init string table");

    // Write node records
    let mut node_pages: Vec<u32> = Vec::new();
    let progress_interval = (node_count / 10).max(1);

    for (i, &(orig_id, _iid)) in nodes_sorted.iter().enumerate() {
        let id_str = orig_id.to_string();
        let user_id_sid = strings.intern(&id_str, &mut pager).expect("intern failed");
        let cell = record::encode_node(user_id_sid, &[], &[]);
        store_cell(&mut pager, PageType::NodeData, &cell, &mut node_pages);

        if (i + 1) % progress_interval == 0 {
            eprintln!("  nodes: {}/{}", i + 1, node_count);
        }
    }

    // Write edge records
    let mut edge_pages: Vec<u32> = Vec::new();
    let progress_interval = (edge_count / 10).max(1);

    // Build adjacency map while iterating edges
    let mut adj: HashMap<u32, Vec<(u32, u32, u8)>> = HashMap::new();

    for (i, &(src_orig, tgt_orig)) in edges.iter().enumerate() {
        let src_iid = node_set[&src_orig];
        let tgt_iid = node_set[&tgt_orig];
        let edge_iid = i as u32;

        let id_str = i.to_string();
        let user_id_sid = strings.intern(&id_str, &mut pager).expect("intern failed");
        let cell = record::encode_edge(user_id_sid, &[], &[], src_iid, tgt_iid, true);
        store_cell(&mut pager, PageType::EdgeData, &cell, &mut edge_pages);

        // Adjacency: outgoing from src, incoming to tgt
        adj.entry(src_iid).or_default().push((edge_iid, tgt_iid, 0)); // outgoing
        adj.entry(tgt_iid).or_default().push((edge_iid, src_iid, 1)); // incoming

        if (i + 1) % progress_interval == 0 {
            eprintln!("  edges: {}/{}", i + 1, edge_count);
        }
    }
    eprintln!("  records written in {:.1}s", t1.elapsed().as_secs_f64());

    // --- Write indexes ---
    eprintln!("Writing indexes...");
    let t2 = Instant::now();

    // Label index: empty (no labels)
    let node_label_root = disk_index::write_label_index(&mut pager, &[]).expect("label index");
    let edge_label_root = disk_index::write_label_index(&mut pager, &[]).expect("label index");

    // Adjacency index — shape matches disk_index::write_adjacency_index (file format).
    #[allow(clippy::type_complexity)]
    let mut adj_entries: Vec<(u32, Vec<(u32, u32, u8)>)> = adj.into_iter().collect();
    adj_entries.sort_by_key(|(iid, _)| *iid);
    let adj_root = disk_index::write_adjacency_index(&mut pager, &adj_entries).expect("adj index");
    drop(adj_entries); // free memory

    eprintln!("  indexes written in {:.1}s", t2.elapsed().as_secs_f64());

    // --- Update header ---
    pager.header.node_count = node_count as u32;
    pager.header.edge_count = edge_count as u32;
    pager.header.label_index_root = node_label_root;
    pager.header.edge_label_index_root = edge_label_root;
    pager.header.adjacency_root = adj_root;
    pager.write_header().expect("write header");

    let total = t0.elapsed().as_secs_f64();
    let file_size = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);
    eprintln!("Done in {:.1}s", total);
    eprintln!(
        "Output: {} ({:.1} MB, {:.1} bytes/edge)",
        output_path,
        file_size as f64 / 1_048_576.0,
        file_size as f64 / edge_count as f64
    );
}

fn store_cell(pager: &mut Pager, page_type: PageType, cell: &[u8], pages: &mut Vec<u32>) {
    if let Some(&last_pg) = pages.last() {
        let mut page = pager.read_page(last_pg).expect("read page");
        if page.insert_cell(cell).is_some() {
            pager.write_page(last_pg, &page).expect("write page");
            return;
        }
    }
    let pg = pager.allocate_page().expect("allocate page");
    let mut page = Page::new(page_type);
    page.insert_cell(cell).expect("cell too large for page");
    pager.write_page(pg, &page).expect("write page");
    pages.push(pg);
}
