//! On-disk index structures for DiskGraphStore.
//!
//! All indexes are stored as sorted arrays on pages, searched via binary search.
//! Overflow is handled by page chains (each page has a "next page" pointer).
//!
//! ## Label Index
//! Root page: array of (label_string_id: u32, first_page: u32) entries.
//! Each label's page chain contains sorted internal_ids (u32) of elements with that label.
//!
//! ## Adjacency Index
//! Array of pages where node internal_id maps to a page offset.
//! Each node's adjacency data: (edge_internal_id: u32, other_node_id: u32, kind: u8)
//!   kind: 0=outgoing, 1=incoming, 2=undirected
//!

use std::io;

use crate::pager::page::{Page, PageType, PAGE_SIZE};
use crate::pager::pager::Pager;

// Page layout for index pages:
// bytes 0:     page_type (u8)
// bytes 1:     unused
// bytes 2-3:   entry_count (u16 LE)
// bytes 4-7:   next_page (u32 LE, 0 = no next)
// bytes 8+:    entries (fixed-size records)
const IDX_HEADER: usize = 8;

/// Write a label index: root page maps label_sid → page chain of element IDs.
pub fn write_label_index(
    pager: &mut Pager,
    label_entries: &[(u32, Vec<u32>)], // (label_string_id, sorted element internal_ids)
) -> io::Result<u32> {
    // Write each label's ID list as a page chain
    let mut root_entries: Vec<(u32, u32)> = Vec::new(); // (label_sid, first_page)
    for (label_sid, ids) in label_entries {
        let first_page = write_u32_list(pager, ids)?;
        root_entries.push((*label_sid, first_page));
    }

    // Write root page: array of (label_sid, first_page) pairs
    write_pair_list(pager, &root_entries)
}

/// Read label index root: returns vec of (label_string_id, first_page_of_ids).
pub fn read_label_index_root(pager: &mut Pager, root_page: u32) -> io::Result<Vec<(u32, u32)>> {
    read_pair_list(pager, root_page)
}

/// Read all u32 IDs from a page chain.
pub fn read_u32_chain(pager: &mut Pager, first_page: u32) -> io::Result<Vec<u32>> {
    let mut result = Vec::new();
    let mut current = first_page;
    while current != 0 {
        let page = pager.read_page(current)?;
        let count = entry_count(&page) as usize;
        let next = next_page(&page);
        for i in 0..count {
            let offset = IDX_HEADER + i * 4;
            let val = u32::from_le_bytes([
                page.data[offset], page.data[offset+1],
                page.data[offset+2], page.data[offset+3],
            ]);
            result.push(val);
        }
        current = next;
    }
    Ok(result)
}

/// Write adjacency index: one page chain per node with (edge_id, other_node, kind) triples.
/// Returns the root page containing (node_internal_id, first_adj_page) pairs.
pub fn write_adjacency_index(
    pager: &mut Pager,
    adj_entries: &[(u32, Vec<(u32, u32, u8)>)], // (node_iid, vec of (edge_iid, other_node_iid, kind))
) -> io::Result<u32> {
    let mut root_entries: Vec<(u32, u32)> = Vec::new();
    for (node_iid, triples) in adj_entries {
        let first_page = write_triple_list(pager, triples)?;
        root_entries.push((*node_iid, first_page));
    }
    write_pair_list(pager, &root_entries)
}

/// Read adjacency root: returns vec of (node_internal_id, first_adj_page).
pub fn read_adjacency_root(pager: &mut Pager, root_page: u32) -> io::Result<Vec<(u32, u32)>> {
    read_pair_list(pager, root_page)
}

/// Read adjacency triples from a page chain: (edge_iid, other_node_iid, kind).
pub fn read_triple_chain(pager: &mut Pager, first_page: u32) -> io::Result<Vec<(u32, u32, u8)>> {
    let mut result = Vec::new();
    let mut current = first_page;
    while current != 0 {
        let page = pager.read_page(current)?;
        let count = entry_count(&page) as usize;
        let next = next_page(&page);
        for i in 0..count {
            let offset = IDX_HEADER + i * 9; // 4 + 4 + 1 = 9 bytes per triple
            let edge_id = u32::from_le_bytes([
                page.data[offset], page.data[offset+1],
                page.data[offset+2], page.data[offset+3],
            ]);
            let other_id = u32::from_le_bytes([
                page.data[offset+4], page.data[offset+5],
                page.data[offset+6], page.data[offset+7],
            ]);
            let kind = page.data[offset+8];
            result.push((edge_id, other_id, kind));
        }
        current = next;
    }
    Ok(result)
}

// ============================================================================
// Low-level page chain writers/readers
// ============================================================================

const MAX_U32_PER_PAGE: usize = (PAGE_SIZE - IDX_HEADER) / 4;
const MAX_PAIRS_PER_PAGE: usize = (PAGE_SIZE - IDX_HEADER) / 8;
const MAX_TRIPLES_PER_PAGE: usize = (PAGE_SIZE - IDX_HEADER) / 9;

fn write_u32_list(pager: &mut Pager, values: &[u32]) -> io::Result<u32> {
    if values.is_empty() {
        // Write an empty page
        let pg = pager.allocate_page()?;
        let page = make_index_page(0, 0);
        pager.write_page(pg, &page)?;
        return Ok(pg);
    }

    // Split into chunks, write back-to-front so we know the next_page pointers
    let chunks: Vec<&[u32]> = values.chunks(MAX_U32_PER_PAGE).collect();
    let mut next = 0u32;
    let mut first = 0u32;
    for chunk in chunks.iter().rev() {
        let pg = pager.allocate_page()?;
        let mut page = make_index_page(chunk.len() as u16, next);
        for (i, &val) in chunk.iter().enumerate() {
            let offset = IDX_HEADER + i * 4;
            page.data[offset..offset+4].copy_from_slice(&val.to_le_bytes());
        }
        pager.write_page(pg, &page)?;
        next = pg;
        first = pg;
    }
    Ok(first)
}

fn write_pair_list(pager: &mut Pager, pairs: &[(u32, u32)]) -> io::Result<u32> {
    if pairs.is_empty() {
        let pg = pager.allocate_page()?;
        let page = make_index_page(0, 0);
        pager.write_page(pg, &page)?;
        return Ok(pg);
    }

    let chunks: Vec<&[(u32, u32)]> = pairs.chunks(MAX_PAIRS_PER_PAGE).collect();
    let mut next = 0u32;
    let mut first = 0u32;
    for chunk in chunks.iter().rev() {
        let pg = pager.allocate_page()?;
        let mut page = make_index_page(chunk.len() as u16, next);
        for (i, (a, b)) in chunk.iter().enumerate() {
            let offset = IDX_HEADER + i * 8;
            page.data[offset..offset+4].copy_from_slice(&a.to_le_bytes());
            page.data[offset+4..offset+8].copy_from_slice(&b.to_le_bytes());
        }
        pager.write_page(pg, &page)?;
        next = pg;
        first = pg;
    }
    Ok(first)
}

fn write_triple_list(pager: &mut Pager, triples: &[(u32, u32, u8)]) -> io::Result<u32> {
    if triples.is_empty() {
        let pg = pager.allocate_page()?;
        let page = make_index_page(0, 0);
        pager.write_page(pg, &page)?;
        return Ok(pg);
    }

    let chunks: Vec<&[(u32, u32, u8)]> = triples.chunks(MAX_TRIPLES_PER_PAGE).collect();
    let mut next = 0u32;
    let mut first = 0u32;
    for chunk in chunks.iter().rev() {
        let pg = pager.allocate_page()?;
        let mut page = make_index_page(chunk.len() as u16, next);
        for (i, (a, b, c)) in chunk.iter().enumerate() {
            let offset = IDX_HEADER + i * 9;
            page.data[offset..offset+4].copy_from_slice(&a.to_le_bytes());
            page.data[offset+4..offset+8].copy_from_slice(&b.to_le_bytes());
            page.data[offset+8] = *c;
        }
        pager.write_page(pg, &page)?;
        next = pg;
        first = pg;
    }
    Ok(first)
}

fn read_pair_list(pager: &mut Pager, first_page: u32) -> io::Result<Vec<(u32, u32)>> {
    let mut result = Vec::new();
    let mut current = first_page;
    while current != 0 {
        let page = pager.read_page(current)?;
        let count = entry_count(&page) as usize;
        let next = next_page(&page);
        for i in 0..count {
            let offset = IDX_HEADER + i * 8;
            let a = u32::from_le_bytes([
                page.data[offset], page.data[offset+1],
                page.data[offset+2], page.data[offset+3],
            ]);
            let b = u32::from_le_bytes([
                page.data[offset+4], page.data[offset+5],
                page.data[offset+6], page.data[offset+7],
            ]);
            result.push((a, b));
        }
        current = next;
    }
    Ok(result)
}

fn make_index_page(count: u16, next: u32) -> Page {
    let mut page = Page::new(PageType::LabelIndex);
    page.data[2] = (count & 0xFF) as u8;
    page.data[3] = (count >> 8) as u8;
    page.data[4..8].copy_from_slice(&next.to_le_bytes());
    page
}

fn entry_count(page: &Page) -> u16 {
    u16::from_le_bytes([page.data[2], page.data[3]])
}

fn next_page(page: &Page) -> u32 {
    u32::from_le_bytes([page.data[4], page.data[5], page.data[6], page.data[7]])
}
