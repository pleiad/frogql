use std::collections::HashMap;

use crate::pager::pager::Pager;
use crate::pager::page::{Page, PageType, PAGE_SIZE};

/// Page header size for string table pages (reusing the standard 8-byte page header).
const PAGE_HEADER: usize = 8;

/// Deduplicated string storage.
///
/// Each unique string gets a u32 ID. Strings are stored on StringTable pages
/// as length-prefixed entries. An in-memory hash map provides fast dedup.
///
/// On-disk format per string table page (after the 8-byte page header):
/// ```text
/// [entry_count: u16 LE]
/// [entries...]
///   each entry: [len: u16 LE][utf8_bytes: len bytes]
/// ```
///
/// String IDs are assigned sequentially (0, 1, 2, ...).
/// To resolve an ID: id / entries_per_first_page gives approximate page,
/// but since strings are variable-length we store a flat vector in memory.
pub struct StringTable {
    /// String → ID mapping for deduplication.
    str_to_id: HashMap<String, u32>,
    /// ID → String for resolution.
    id_to_str: Vec<String>,
    /// Pages used by the string table (page numbers in the database file).
    pages: Vec<u32>,
    /// Current write page index (into `pages` vec).
    current_page_idx: usize,
    /// Byte offset within the current page for the next write.
    current_offset: usize,
}

impl StringTable {
    /// Create a new empty string table. Call `init` to allocate its first page.
    pub fn new() -> Self {
        StringTable {
            str_to_id: HashMap::new(),
            id_to_str: Vec::new(),
            pages: Vec::new(),
            current_page_idx: 0,
            current_offset: PAGE_HEADER + 2, // skip page header + entry_count
        }
    }

    /// Allocate the first page for a fresh string table.
    pub fn init(&mut self, pager: &mut Pager) -> std::io::Result<u32> {
        let page_num = pager.allocate_page()?;
        let page = Page::new(PageType::StringTable);
        pager.write_page(page_num, &page)?;
        self.pages.push(page_num);
        self.current_page_idx = 0;
        self.current_offset = PAGE_HEADER + 2;
        Ok(page_num)
    }

    /// Intern a string: returns its ID, inserting if new.
    pub fn intern(&mut self, s: &str, pager: &mut Pager) -> std::io::Result<u32> {
        if let Some(&id) = self.str_to_id.get(s) {
            return Ok(id);
        }

        let id = self.id_to_str.len() as u32;
        let encoded_len = 2 + s.len(); // u16 len prefix + bytes

        // Check if current page has space
        if self.current_offset + encoded_len > PAGE_SIZE {
            // Flush entry count to current page, then allocate a new one
            self.flush_entry_count(pager)?;
            let new_page_num = pager.allocate_page()?;
            let page = Page::new(PageType::StringTable);
            pager.write_page(new_page_num, &page)?;
            self.pages.push(new_page_num);
            self.current_page_idx = self.pages.len() - 1;
            self.current_offset = PAGE_HEADER + 2;
        }

        // Write the string to the current page
        let page_num = self.pages[self.current_page_idx];
        let mut page = pager.read_page(page_num)?;

        let len_bytes = (s.len() as u16).to_le_bytes();
        page.data[self.current_offset] = len_bytes[0];
        page.data[self.current_offset + 1] = len_bytes[1];
        page.data[self.current_offset + 2..self.current_offset + 2 + s.len()]
            .copy_from_slice(s.as_bytes());

        self.current_offset += encoded_len;

        // Update entry count
        let count = self.page_entry_count(&page) + 1;
        self.set_page_entry_count(&mut page, count);

        pager.write_page(page_num, &page)?;

        self.str_to_id.insert(s.to_string(), id);
        self.id_to_str.push(s.to_string());

        Ok(id)
    }

    /// Resolve a string ID to its string.
    pub fn resolve(&self, id: u32) -> Option<&str> {
        self.id_to_str.get(id as usize).map(|s| s.as_str())
    }

    /// Load the string table from disk into memory.
    pub fn load(pages: &[u32], pager: &mut Pager) -> std::io::Result<Self> {
        let mut st = StringTable {
            str_to_id: HashMap::new(),
            id_to_str: Vec::new(),
            pages: pages.to_vec(),
            current_page_idx: 0,
            current_offset: PAGE_HEADER + 2,
        };

        for (page_idx, &page_num) in pages.iter().enumerate() {
            let page = pager.read_page(page_num)?;
            let entry_count = st.page_entry_count(&page);
            let mut offset = PAGE_HEADER + 2;

            for _ in 0..entry_count {
                let len = u16::from_le_bytes([page.data[offset], page.data[offset + 1]]) as usize;
                let s = std::str::from_utf8(&page.data[offset + 2..offset + 2 + len])
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

                let id = st.id_to_str.len() as u32;
                st.str_to_id.insert(s.to_string(), id);
                st.id_to_str.push(s.to_string());

                offset += 2 + len;
            }

            st.current_page_idx = page_idx;
            st.current_offset = offset;
        }

        Ok(st)
    }

    /// Get the list of page numbers used by this string table.
    pub fn page_numbers(&self) -> &[u32] {
        &self.pages
    }

    /// Number of interned strings.
    pub fn len(&self) -> usize {
        self.id_to_str.len()
    }

    fn page_entry_count(&self, page: &Page) -> u16 {
        u16::from_le_bytes([page.data[PAGE_HEADER], page.data[PAGE_HEADER + 1]])
    }

    fn set_page_entry_count(&self, page: &mut Page, count: u16) {
        let bytes = count.to_le_bytes();
        page.data[PAGE_HEADER] = bytes[0];
        page.data[PAGE_HEADER + 1] = bytes[1];
    }

    fn flush_entry_count(&self, pager: &mut Pager) -> std::io::Result<()> {
        // Entry count is written on every intern, so this is a no-op
        // but kept for clarity if we batch writes later.
        let _ = pager;
        Ok(())
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

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_intern_and_resolve() {
        let path = temp_path("st_basic.gql");
        cleanup(&path);

        let mut pager = Pager::create(&path).unwrap();
        let mut st = StringTable::new();
        st.init(&mut pager).unwrap();

        let id0 = st.intern("hello", &mut pager).unwrap();
        let id1 = st.intern("world", &mut pager).unwrap();
        let id2 = st.intern("hello", &mut pager).unwrap(); // dedup

        assert_eq!(id0, 0);
        assert_eq!(id1, 1);
        assert_eq!(id2, 0); // same as first
        assert_eq!(st.len(), 2);

        assert_eq!(st.resolve(0), Some("hello"));
        assert_eq!(st.resolve(1), Some("world"));
        assert_eq!(st.resolve(99), None);

        cleanup(&path);
    }

    #[test]
    fn test_load_from_disk() {
        let path = temp_path("st_load.gql");
        cleanup(&path);

        let pages;
        {
            let mut pager = Pager::create(&path).unwrap();
            let mut st = StringTable::new();
            st.init(&mut pager).unwrap();
            st.intern("Account", &mut pager).unwrap();
            st.intern("Transfer", &mut pager).unwrap();
            st.intern("owner", &mut pager).unwrap();
            pages = st.page_numbers().to_vec();
        }

        // Reopen and load
        {
            let mut pager = Pager::open(&path).unwrap();
            let st = StringTable::load(&pages, &mut pager).unwrap();
            assert_eq!(st.len(), 3);
            assert_eq!(st.resolve(0), Some("Account"));
            assert_eq!(st.resolve(1), Some("Transfer"));
            assert_eq!(st.resolve(2), Some("owner"));
        }

        cleanup(&path);
    }

    #[test]
    fn test_many_strings_span_pages() {
        let path = temp_path("st_multipage.gql");
        cleanup(&path);

        let mut pager = Pager::create(&path).unwrap();
        let mut st = StringTable::new();
        st.init(&mut pager).unwrap();

        // Insert enough strings to span multiple pages
        // Each string ~100 bytes + 2 overhead, page has ~4086 usable → ~39 per page
        for i in 0..100 {
            let s = format!("string_number_{:0>90}", i); // ~105 bytes each
            st.intern(&s, &mut pager).unwrap();
        }

        assert_eq!(st.len(), 100);
        assert!(st.page_numbers().len() > 1);

        // Verify all resolve correctly
        for i in 0..100 {
            let expected = format!("string_number_{:0>90}", i);
            assert_eq!(st.resolve(i as u32), Some(expected.as_str()));
        }

        // Load from disk and verify
        let pages = st.page_numbers().to_vec();
        let st2 = StringTable::load(&pages, &mut pager).unwrap();
        assert_eq!(st2.len(), 100);
        for i in 0..100 {
            let expected = format!("string_number_{:0>90}", i);
            assert_eq!(st2.resolve(i as u32), Some(expected.as_str()));
        }

        cleanup(&path);
    }
}
