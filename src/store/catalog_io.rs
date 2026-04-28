//! Persistence for the graph-type catalog.
//!
//! The catalog is serialized as JSON and stored in a chain of pages with
//! `PageType::Catalog`. The `.gdb` header points at the chain root via
//! `header.catalog_root`. Format on each page (after the standard 8-byte
//! page header at byte 0):
//!
//! ```text
//! first page:        [next_page u32 LE] [total_len u32 LE] [payload..]
//! continuation pages: [next_page u32 LE]                    [payload..]
//! ```
//!
//! `next_page = 0` marks the end of the chain. `total_len` lets the
//! reader stop before trailing zero bytes in the last page even when the
//! chain is reused for a longer payload later.
//!
//! Allocation strategy: each `write_catalog` call frees the old chain
//! (if any) and writes a fresh one. This keeps reads cheap (no read of
//! a free chain to track in-use pages) at the cost of churn in the page
//! free list. Catalogs are small (single-page in practice), so the
//! tradeoff is fine.

use std::io;

use crate::pager::{Page, PageType, Pager, PAGE_SIZE};
use crate::runtime::catalog::GraphTypeCatalog;

const PAGE_HEADER: usize = 8;
const NEXT_PTR: usize = 4;
const TOTAL_LEN: usize = 4;
const FIRST_HEADER_END: usize = PAGE_HEADER + NEXT_PTR + TOTAL_LEN; // 16
const CONT_HEADER_END: usize = PAGE_HEADER + NEXT_PTR; // 12

const PAYLOAD_FIRST: usize = PAGE_SIZE - FIRST_HEADER_END; // 4080
const PAYLOAD_CONT: usize = PAGE_SIZE - CONT_HEADER_END; // 4084

/// Read and deserialize a catalog from the chain rooted at `root`.
/// `root == 0` returns an empty catalog (used for legacy `.gdb` files
/// and freshly-created databases).
pub fn read_catalog(pager: &mut Pager, root: u32) -> io::Result<GraphTypeCatalog> {
    if root == 0 {
        return Ok(GraphTypeCatalog::new());
    }
    let bytes = read_chain(pager, root)?;
    if bytes.is_empty() {
        return Ok(GraphTypeCatalog::new());
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("catalog: {e}")))
}

/// Serialize and write `catalog` to a new chain. Frees the old chain
/// rooted at `old_root` (if non-zero) before allocating new pages, so
/// page numbers can be reused. Returns the new root page.
pub fn write_catalog(
    pager: &mut Pager,
    catalog: &GraphTypeCatalog,
    old_root: u32,
) -> io::Result<u32> {
    if old_root != 0 {
        free_chain(pager, old_root)?;
    }
    let json = serde_json::to_vec(catalog)
        .map_err(|e| io::Error::other(format!("catalog encode: {e}")))?;
    write_chain(pager, &json)
}

/// Walk a catalog chain and free every page. Used both before a write
/// (to reclaim pages) and on `DROP`-everything paths.
pub fn free_chain(pager: &mut Pager, root: u32) -> io::Result<()> {
    let mut p = root;
    while p != 0 {
        let page = pager.read_page(p)?;
        let next = read_next_pointer(&page);
        pager.free_page(p)?;
        p = next;
    }
    Ok(())
}

fn read_chain(pager: &mut Pager, root: u32) -> io::Result<Vec<u8>> {
    let first = pager.read_page(root)?;
    if first.page_type() != PageType::Catalog {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "catalog root page {root} has type {:?}, expected Catalog",
                first.page_type()
            ),
        ));
    }
    let total = u32::from_le_bytes([
        first.data[12],
        first.data[13],
        first.data[14],
        first.data[15],
    ]) as usize;
    let mut out = Vec::with_capacity(total);
    let chunk_first = total.min(PAYLOAD_FIRST);
    out.extend_from_slice(&first.data[FIRST_HEADER_END..FIRST_HEADER_END + chunk_first]);

    let mut next = read_next_pointer(&first);
    while next != 0 && out.len() < total {
        let page = pager.read_page(next)?;
        let chunk = (total - out.len()).min(PAYLOAD_CONT);
        out.extend_from_slice(&page.data[CONT_HEADER_END..CONT_HEADER_END + chunk]);
        next = read_next_pointer(&page);
    }

    Ok(out)
}

fn write_chain(pager: &mut Pager, payload: &[u8]) -> io::Result<u32> {
    let total = payload.len();
    let pages_needed = if total <= PAYLOAD_FIRST {
        1
    } else {
        1 + (total - PAYLOAD_FIRST).div_ceil(PAYLOAD_CONT)
    };

    let mut nums: Vec<u32> = Vec::with_capacity(pages_needed);
    for _ in 0..pages_needed {
        nums.push(pager.allocate_page()?);
    }

    let mut offset = 0;
    for (i, &pg_num) in nums.iter().enumerate() {
        let cap = if i == 0 { PAYLOAD_FIRST } else { PAYLOAD_CONT };
        let end = (offset + cap).min(total);
        let chunk = &payload[offset..end];
        let next = if i + 1 < nums.len() { nums[i + 1] } else { 0 };

        let mut page = Page::new(PageType::Catalog);
        page.data[PAGE_HEADER..PAGE_HEADER + 4].copy_from_slice(&next.to_le_bytes());
        let mut data_start = PAGE_HEADER + NEXT_PTR;
        if i == 0 {
            page.data[data_start..data_start + 4].copy_from_slice(&(total as u32).to_le_bytes());
            data_start += TOTAL_LEN;
        }
        page.data[data_start..data_start + chunk.len()].copy_from_slice(chunk);
        pager.write_page(pg_num, &page)?;

        offset = end;
    }

    Ok(nums[0])
}

fn read_next_pointer(page: &Page) -> u32 {
    u32::from_le_bytes([
        page.data[PAGE_HEADER],
        page.data[PAGE_HEADER + 1],
        page.data[PAGE_HEADER + 2],
        page.data[PAGE_HEADER + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::catalog::GraphTypeCatalog;
    use crate::typing::variable_type::Schema;
    use std::path::PathBuf;

    fn temp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("gqlrust_catalog_io_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn roundtrip_empty_catalog() {
        let path = temp_db("empty.gdb");
        let mut pager = Pager::create(&path).unwrap();
        let cat = GraphTypeCatalog::new();
        let root = write_catalog(&mut pager, &cat, 0).unwrap();
        assert!(root != 0);
        let back = read_catalog(&mut pager, root).unwrap();
        assert!(back.types.is_empty());
        assert!(back.active_name().is_none());
    }

    #[test]
    fn roundtrip_default_catalog() {
        let path = temp_db("default.gdb");
        let mut pager = Pager::create(&path).unwrap();
        let mut cat = GraphTypeCatalog::new();
        cat.install_default(Schema::star());
        let root = write_catalog(&mut pager, &cat, 0).unwrap();
        let back = read_catalog(&mut pager, root).unwrap();
        assert_eq!(back.active_name(), Some("DEFAULT"));
        assert!(back.contains("DEFAULT"));
    }

    #[test]
    fn read_zero_root_is_empty() {
        let path = temp_db("zeroroot.gdb");
        let mut pager = Pager::create(&path).unwrap();
        let back = read_catalog(&mut pager, 0).unwrap();
        assert!(back.types.is_empty());
        assert!(back.active_name().is_none());
    }

    #[test]
    fn rewrite_frees_old_chain() {
        let path = temp_db("rewrite.gdb");
        let mut pager = Pager::create(&path).unwrap();
        let mut cat = GraphTypeCatalog::new();
        cat.install_default(Schema::star());
        let root1 = write_catalog(&mut pager, &cat, 0).unwrap();
        let root2 = write_catalog(&mut pager, &cat, root1).unwrap();
        // The freed page should have been recycled into the new chain,
        // so root2 reuses root1.
        assert_eq!(root1, root2);
    }
}
