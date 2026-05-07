//! Persistence for the DDL-declared subset of the `SecondaryIndex`.
//!
//! Auto-built indexes are not persisted: the open path's
//! `build_auto_indexes_bulk` reproduces them deterministically from the
//! data on every load, so storing them would just duplicate work and
//! grow `.gdb` files. DDL-declared indexes (i.e.
//! `CREATE [HASH | BTREE] INDEX ...`) target `(label, prop)` pairs that
//! the auto path skips (typically because the property is non-unique
//! within the label, like `Post.creationDate`); those are the ones we
//! save so the user's `CREATE INDEX` survives close/reopen.
//!
//! On-disk layout follows the same single-chain JSON pattern as
//! `catalog_io`. The .gdb header points at the chain root via
//! `header.secondary_index_root`. `0` means "no persisted list" — the
//! standard signal for legacy files written before this slot existed
//! and for fresh databases that never declared a DDL index.
//!
//! Format on each page (after the standard 8-byte page header at byte 0):
//!
//! ```text
//! first page:        [next_page u32 LE] [total_len u32 LE] [payload..]
//! continuation pages: [next_page u32 LE]                    [payload..]
//! ```
//!
//! The payload is JSON-encoded `Vec<PersistedSpec>`. Each rewrite frees
//! the old chain and allocates fresh pages, mirroring `catalog_io` —
//! the DDL list is small (typically a few entries), so the churn is
//! negligible and reads avoid having to skip free pages.

use std::io;

use crate::pager::{Page, PageType, Pager, PAGE_SIZE};

use super::secondary_index::{IndexKind, IndexSpec};

/// On-disk shape of one persisted DDL spec. Mirrors the public fields
/// of `IndexSpec` that the loader needs to rebuild the bucket via
/// `build_declared`. We keep this as a parallel type rather than
/// deriving `Serialize` on `IndexSpec` itself so the in-memory struct
/// stays free of serde dependencies and the on-disk format can evolve
/// independently from the runtime API.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
struct PersistedSpec {
    name: String,
    label: String,
    prop: String,
    /// 0 = HASH, 1 = BTREE. Stored as a small int so on-disk
    /// representation is stable across renames of the in-memory enum.
    kind: u8,
}

impl From<&IndexSpec> for PersistedSpec {
    fn from(spec: &IndexSpec) -> Self {
        let kind = match spec.kind {
            IndexKind::Hash => 0,
            IndexKind::BTree => 1,
        };
        PersistedSpec {
            name: spec.name.clone(),
            label: spec.label.clone(),
            prop: spec.prop.clone(),
            kind,
        }
    }
}

/// Decoded DDL spec ready to feed into `build_declared`.
pub struct DeclaredSpec {
    pub name: String,
    pub label: String,
    pub prop: String,
    pub kind: IndexKind,
}

const PAGE_HEADER: usize = 8;
const NEXT_PTR: usize = 4;
const TOTAL_LEN: usize = 4;
const FIRST_HEADER_END: usize = PAGE_HEADER + NEXT_PTR + TOTAL_LEN;
const CONT_HEADER_END: usize = PAGE_HEADER + NEXT_PTR;
const PAYLOAD_FIRST: usize = PAGE_SIZE - FIRST_HEADER_END;
const PAYLOAD_CONT: usize = PAGE_SIZE - CONT_HEADER_END;

/// Read the persisted DDL list. `root == 0` returns an empty list,
/// which is what every legacy file (written before the slot existed)
/// reports — and what newly-created databases without any DDL also
/// report. Callers therefore don't need to special-case legacy.
pub fn read_specs(pager: &mut Pager, root: u32) -> io::Result<Vec<DeclaredSpec>> {
    if root == 0 {
        return Ok(Vec::new());
    }
    let bytes = read_chain(pager, root)?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let raw: Vec<PersistedSpec> = serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("ddl-index: {e}")))?;
    Ok(raw
        .into_iter()
        .map(|p| DeclaredSpec {
            name: p.name,
            label: p.label,
            prop: p.prop,
            kind: if p.kind == 0 {
                IndexKind::Hash
            } else {
                IndexKind::BTree
            },
        })
        .collect())
}

/// Write the DDL subset of `specs` (entries with `auto = false`) to a
/// new chain. Frees `old_root` (if non-zero) before allocating so page
/// numbers can be recycled. Returns the new root, or `0` when there
/// are no DDL entries to persist (caller updates the header
/// accordingly).
pub fn write_specs(pager: &mut Pager, specs: &[IndexSpec], old_root: u32) -> io::Result<u32> {
    if old_root != 0 {
        free_chain(pager, old_root)?;
    }
    let persisted: Vec<PersistedSpec> = specs
        .iter()
        .filter(|s| !s.auto)
        .map(PersistedSpec::from)
        .collect();
    if persisted.is_empty() {
        return Ok(0);
    }
    let json = serde_json::to_vec(&persisted)
        .map_err(|e| io::Error::other(format!("ddl-index encode: {e}")))?;
    write_chain(pager, &json)
}

fn free_chain(pager: &mut Pager, root: u32) -> io::Result<()> {
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
    if first.page_type() != PageType::SecondaryIndex {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "secondary-index root page {root} has type {:?}, expected SecondaryIndex",
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

        let mut page = Page::new(PageType::SecondaryIndex);
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
