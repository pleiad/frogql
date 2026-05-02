use std::cell::RefCell;
use std::collections::HashMap;

use crate::pager::page::{Page, PageType, PAGE_SIZE};
use crate::pager::pager::Pager;

/// Page header size for string table pages (reusing the standard 8-byte page header).
const PAGE_HEADER: usize = 8;

/// Marker value in the len field indicating an overflow string.
/// Normal strings have len < 0xFFFF (max 65534 bytes inline), so 0xFFFF is safe.
const OVERFLOW_MARKER: u16 = 0xFFFF;

/// Size of the overflow header written on the string table page after the marker:
/// [total_len: u32][first_overflow_page: u32] = 8 bytes.
const OVERFLOW_HEADER_SIZE: usize = 8;

/// Bytes available for payload on an overflow page (after page header + next-page pointer).
/// Layout: [page_header: 8][next_page: u32 = 4][payload: rest]
const OVERFLOW_PAGE_PAYLOAD: usize = PAGE_SIZE - PAGE_HEADER - 4;

/// Maximum usable space for a string entry on a string table page.
/// This is the page minus the page header and the 2-byte entry count.
const MAX_INLINE: usize = PAGE_SIZE - PAGE_HEADER - 2;

/// Deduplicated string storage with overflow page support.
///
/// Each unique string gets a u32 ID. Short strings are stored inline on
/// StringTable pages as length-prefixed entries. Strings too large to fit
/// on a single page use overflow pages (linked list of Overflow pages).
///
/// On-disk format per string table page (after the 8-byte page header):
/// ```text
/// [entry_count: u16 LE]
/// [entries...]
///   normal entry:   [len: u16 LE][utf8_bytes: len bytes]
///   overflow entry: [0xFFFF: u16 LE][total_len: u32 LE][first_overflow_page: u32 LE][inline_bytes...]
/// ```
///
/// Overflow pages (PageType::Overflow):
/// ```text
/// [page_header: 8 bytes]
/// [next_page: u32 LE]  (0 = last page in chain)
/// [payload: up to OVERFLOW_PAGE_PAYLOAD bytes]
/// ```
pub struct StringTable {
    /// String → ID mapping for deduplication. Built lazily: at `load` time
    /// the map is left empty (`None`), and the first caller that needs it
    /// (`intern` for writes, `id_for_str` for label-index lookups) pays the
    /// O(N) population cost. For read-only LazyGraphStore queries the map
    /// is never built — saves ~50% of `load` time on graphs with many
    /// interned strings (327K node user-ids in LDBC SF0.1).
    str_to_id: RefCell<Option<HashMap<String, u32>>>,
    /// ID → String for resolution.
    id_to_str: Vec<String>,
    /// Pages used by the string table (page numbers in the database file).
    pages: Vec<u32>,
    /// Current write page index (into `pages` vec).
    current_page_idx: usize,
    /// Byte offset within the current page for the next write.
    current_offset: usize,
}

impl Default for StringTable {
    fn default() -> Self {
        StringTable {
            str_to_id: RefCell::new(Some(HashMap::new())),
            id_to_str: Vec::new(),
            pages: Vec::new(),
            current_page_idx: 0,
            current_offset: PAGE_HEADER + 2, // skip page header + entry_count
        }
    }
}

impl StringTable {
    /// Create a new empty string table. Call `init` to allocate its first page.
    pub fn new() -> Self {
        Self::default()
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
        self.ensure_str_to_id_built();
        if let Some(&id) = self.str_to_id.borrow().as_ref().unwrap().get(s) {
            return Ok(id);
        }

        let id = self.id_to_str.len() as u32;
        let encoded_len = 2 + s.len(); // u16 len prefix + bytes

        // Does this string fit inline on a single page?
        if encoded_len <= MAX_INLINE {
            self.intern_inline(s, encoded_len, pager)?;
        } else {
            self.intern_overflow(s, pager)?;
        }

        self.str_to_id
            .borrow_mut()
            .as_mut()
            .unwrap()
            .insert(s.to_string(), id);
        self.id_to_str.push(s.to_string());

        Ok(id)
    }

    /// Resolve a string to its interned ID. Lazily builds the
    /// String→ID lookup map on first call (load() leaves it `None` to
    /// avoid the O(N) population cost on read-only opens).
    pub fn id_for_str(&self, s: &str) -> Option<u32> {
        self.ensure_str_to_id_built();
        self.str_to_id.borrow().as_ref().unwrap().get(s).copied()
    }

    /// Build the str_to_id map on demand from id_to_str. Idempotent.
    fn ensure_str_to_id_built(&self) {
        if self.str_to_id.borrow().is_some() {
            return;
        }
        let mut map = HashMap::with_capacity(self.id_to_str.len());
        for (i, s) in self.id_to_str.iter().enumerate() {
            map.insert(s.clone(), i as u32);
        }
        *self.str_to_id.borrow_mut() = Some(map);
    }

    /// Write a short string inline on the current (or a fresh) string table page.
    fn intern_inline(
        &mut self,
        s: &str,
        encoded_len: usize,
        pager: &mut Pager,
    ) -> std::io::Result<()> {
        // Ensure there's room on the current page
        if self.current_offset + encoded_len > PAGE_SIZE {
            self.allocate_string_page(pager)?;
        }

        let page_num = self.pages[self.current_page_idx];
        let mut page = pager.read_page(page_num)?;

        let len_bytes = (s.len() as u16).to_le_bytes();
        page.data[self.current_offset] = len_bytes[0];
        page.data[self.current_offset + 1] = len_bytes[1];
        page.data[self.current_offset + 2..self.current_offset + 2 + s.len()]
            .copy_from_slice(s.as_bytes());

        self.current_offset += encoded_len;
        self.bump_entry_count(&mut page);
        pager.write_page(page_num, &page)?;
        Ok(())
    }

    /// Write a large string using overflow pages.
    ///
    /// On the string table page we write:
    ///   [OVERFLOW_MARKER: u16][total_len: u32][first_overflow_page: u32][inline_chunk...]
    ///
    /// The inline_chunk uses whatever space remains on the current page.
    /// The rest of the string goes into a chain of Overflow pages.
    fn intern_overflow(&mut self, s: &str, pager: &mut Pager) -> std::io::Result<()> {
        // We need at least the header (2 + 8 = 10 bytes) on the string table page.
        let header_size = 2 + OVERFLOW_HEADER_SIZE; // marker + total_len + first_page_ptr
        if self.current_offset + header_size > PAGE_SIZE {
            self.allocate_string_page(pager)?;
        }

        let bytes = s.as_bytes();
        let total_len = bytes.len();

        // How many bytes can we inline after the header on the current page?
        let inline_capacity = PAGE_SIZE - self.current_offset - header_size;
        let inline_len = inline_capacity.min(total_len);
        let remaining = &bytes[inline_len..];

        // Write overflow pages for the remaining bytes (build chain in reverse
        // so we know each page's next pointer at write time).
        let overflow_start = self.write_overflow_chain(remaining, pager)?;

        // Write the header + inline chunk on the string table page.
        let page_num = self.pages[self.current_page_idx];
        let mut page = pager.read_page(page_num)?;
        let off = self.current_offset;

        // Marker
        page.data[off..off + 2].copy_from_slice(&OVERFLOW_MARKER.to_le_bytes());
        // Total length
        page.data[off + 2..off + 6].copy_from_slice(&(total_len as u32).to_le_bytes());
        // First overflow page number
        page.data[off + 6..off + 10].copy_from_slice(&overflow_start.to_le_bytes());
        // Inline chunk
        if inline_len > 0 {
            page.data[off + 10..off + 10 + inline_len].copy_from_slice(&bytes[..inline_len]);
        }

        self.current_offset += header_size + inline_len;
        self.bump_entry_count(&mut page);
        pager.write_page(page_num, &page)?;
        Ok(())
    }

    /// Write a chain of overflow pages for `data` and return the first page number.
    /// If data is empty, returns 0 (no overflow pages needed).
    fn write_overflow_chain(&self, data: &[u8], pager: &mut Pager) -> std::io::Result<u32> {
        if data.is_empty() {
            return Ok(0);
        }

        // Split data into chunks of OVERFLOW_PAGE_PAYLOAD bytes.
        let chunks: Vec<&[u8]> = data.chunks(OVERFLOW_PAGE_PAYLOAD).collect();

        // Allocate all pages first, then write in order with correct next pointers.
        let mut page_nums = Vec::with_capacity(chunks.len());
        for _ in &chunks {
            page_nums.push(pager.allocate_page()?);
        }

        for (i, chunk) in chunks.iter().enumerate() {
            let mut page = Page::new(PageType::Overflow);
            let next_page: u32 = if i + 1 < page_nums.len() {
                page_nums[i + 1]
            } else {
                0
            };
            // Write next-page pointer at offset PAGE_HEADER
            page.data[PAGE_HEADER..PAGE_HEADER + 4].copy_from_slice(&next_page.to_le_bytes());
            // Write payload
            let payload_start = PAGE_HEADER + 4;
            page.data[payload_start..payload_start + chunk.len()].copy_from_slice(chunk);
            pager.write_page(page_nums[i], &page)?;
        }

        Ok(page_nums[0])
    }

    /// Read an overflow string given its total length and first overflow page.
    fn read_overflow(
        &self,
        total_len: usize,
        inline_data: &[u8],
        first_overflow_page: u32,
        pager: &mut Pager,
    ) -> std::io::Result<String> {
        let mut buf = Vec::with_capacity(total_len);
        buf.extend_from_slice(inline_data);

        let mut next_page = first_overflow_page;
        while next_page != 0 && buf.len() < total_len {
            let page = pager.read_page(next_page)?;
            next_page = u32::from_le_bytes([
                page.data[PAGE_HEADER],
                page.data[PAGE_HEADER + 1],
                page.data[PAGE_HEADER + 2],
                page.data[PAGE_HEADER + 3],
            ]);
            let payload_start = PAGE_HEADER + 4;
            let need = total_len - buf.len();
            let available = OVERFLOW_PAGE_PAYLOAD.min(need);
            buf.extend_from_slice(&page.data[payload_start..payload_start + available]);
        }

        String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Resolve a string ID to its string.
    pub fn resolve(&self, id: u32) -> Option<&str> {
        self.id_to_str.get(id as usize).map(|s| s.as_str())
    }

    /// Load the string table from disk into memory.
    pub fn load(pages: &[u32], pager: &mut Pager) -> std::io::Result<Self> {
        // `str_to_id = None` defers the String→ID HashMap until something
        // actually needs it (intern, label-index lookup). For read-only
        // queries through LazyGraphStore the map is never built — saves
        // ~50% of load time on graphs with many interned strings.
        let mut st = StringTable {
            str_to_id: RefCell::new(None),
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
                let len_field = u16::from_le_bytes([page.data[offset], page.data[offset + 1]]);

                let s = if len_field == OVERFLOW_MARKER {
                    // Overflow entry
                    let total_len = u32::from_le_bytes([
                        page.data[offset + 2],
                        page.data[offset + 3],
                        page.data[offset + 4],
                        page.data[offset + 5],
                    ]) as usize;
                    let first_overflow_page = u32::from_le_bytes([
                        page.data[offset + 6],
                        page.data[offset + 7],
                        page.data[offset + 8],
                        page.data[offset + 9],
                    ]);

                    // Inline chunk: from offset+10 to end of page (or total_len, whichever is less)
                    let inline_start = offset + 10;
                    let inline_available = PAGE_SIZE - inline_start;
                    let inline_len = inline_available.min(total_len);
                    let inline_data = &page.data[inline_start..inline_start + inline_len];

                    let s = st.read_overflow(total_len, inline_data, first_overflow_page, pager)?;
                    offset = inline_start + inline_len;
                    s
                } else {
                    // Normal inline entry
                    let len = len_field as usize;
                    let s = std::str::from_utf8(&page.data[offset + 2..offset + 2 + len])
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
                        .to_string();
                    offset += 2 + len;
                    s
                };

                // Skip str_to_id population — it's `None` until first access.
                st.id_to_str.push(s);
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

    /// True when no strings have been interned.
    pub fn is_empty(&self) -> bool {
        self.id_to_str.is_empty()
    }

    /// Allocate a new string table page and switch to it.
    fn allocate_string_page(&mut self, pager: &mut Pager) -> std::io::Result<()> {
        let new_page_num = pager.allocate_page()?;
        let page = Page::new(PageType::StringTable);
        pager.write_page(new_page_num, &page)?;
        self.pages.push(new_page_num);
        self.current_page_idx = self.pages.len() - 1;
        self.current_offset = PAGE_HEADER + 2;
        Ok(())
    }

    /// Increment the entry count on a string table page.
    fn bump_entry_count(&self, page: &mut Page) {
        let count = self.page_entry_count(page) + 1;
        self.set_page_entry_count(page, count);
    }

    fn page_entry_count(&self, page: &Page) -> u16 {
        u16::from_le_bytes([page.data[PAGE_HEADER], page.data[PAGE_HEADER + 1]])
    }

    fn set_page_entry_count(&self, page: &mut Page, count: u16) {
        let bytes = count.to_le_bytes();
        page.data[PAGE_HEADER] = bytes[0];
        page.data[PAGE_HEADER + 1] = bytes[1];
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

    #[test]
    fn test_overflow_single_large_string() {
        let path = temp_path("st_overflow_single.gql");
        cleanup(&path);

        let mut pager = Pager::create(&path).unwrap();
        let mut st = StringTable::new();
        st.init(&mut pager).unwrap();

        // Create a string larger than one page (8000 bytes > 4096)
        let big = "X".repeat(8000);
        let id = st.intern(&big, &mut pager).unwrap();
        assert_eq!(id, 0);
        assert_eq!(st.resolve(0).unwrap(), big);

        // Verify it survives load from disk
        let pages = st.page_numbers().to_vec();
        let st2 = StringTable::load(&pages, &mut pager).unwrap();
        assert_eq!(st2.len(), 1);
        assert_eq!(st2.resolve(0).unwrap(), big);

        cleanup(&path);
    }

    #[test]
    fn test_overflow_very_large_string() {
        let path = temp_path("st_overflow_very_large.gql");
        cleanup(&path);

        let mut pager = Pager::create(&path).unwrap();
        let mut st = StringTable::new();
        st.init(&mut pager).unwrap();

        // 20KB string — needs multiple overflow pages
        let big = "A".repeat(20_000);
        let id = st.intern(&big, &mut pager).unwrap();
        assert_eq!(id, 0);
        assert_eq!(st.resolve(0).unwrap(), big);

        // Load from disk
        let pages = st.page_numbers().to_vec();
        let st2 = StringTable::load(&pages, &mut pager).unwrap();
        assert_eq!(st2.resolve(0).unwrap(), big);

        cleanup(&path);
    }

    #[test]
    fn test_overflow_mixed_with_inline() {
        let path = temp_path("st_overflow_mixed.gql");
        cleanup(&path);

        let mut pager = Pager::create(&path).unwrap();
        let mut st = StringTable::new();
        st.init(&mut pager).unwrap();

        // Mix of short and long strings
        let short1 = "hello";
        let big = "B".repeat(10_000);
        let short2 = "world";
        let big2 = "C".repeat(5_000);
        let short3 = "!";

        st.intern(short1, &mut pager).unwrap();
        st.intern(&big, &mut pager).unwrap();
        st.intern(short2, &mut pager).unwrap();
        st.intern(&big2, &mut pager).unwrap();
        st.intern(short3, &mut pager).unwrap();

        assert_eq!(st.len(), 5);
        assert_eq!(st.resolve(0).unwrap(), short1);
        assert_eq!(st.resolve(1).unwrap(), big);
        assert_eq!(st.resolve(2).unwrap(), short2);
        assert_eq!(st.resolve(3).unwrap(), big2);
        assert_eq!(st.resolve(4).unwrap(), short3);

        // Load from disk
        let pages = st.page_numbers().to_vec();
        let st2 = StringTable::load(&pages, &mut pager).unwrap();
        assert_eq!(st2.len(), 5);
        assert_eq!(st2.resolve(0).unwrap(), short1);
        assert_eq!(st2.resolve(1).unwrap(), big);
        assert_eq!(st2.resolve(2).unwrap(), short2);
        assert_eq!(st2.resolve(3).unwrap(), big2);
        assert_eq!(st2.resolve(4).unwrap(), short3);

        cleanup(&path);
    }

    #[test]
    fn test_overflow_dedup() {
        let path = temp_path("st_overflow_dedup.gql");
        cleanup(&path);

        let mut pager = Pager::create(&path).unwrap();
        let mut st = StringTable::new();
        st.init(&mut pager).unwrap();

        let big = "D".repeat(8000);
        let id1 = st.intern(&big, &mut pager).unwrap();
        let id2 = st.intern(&big, &mut pager).unwrap();

        assert_eq!(id1, id2); // deduplication works
        assert_eq!(st.len(), 1);

        cleanup(&path);
    }

    #[test]
    fn test_overflow_exact_page_boundary() {
        let path = temp_path("st_overflow_boundary.gql");
        cleanup(&path);

        let mut pager = Pager::create(&path).unwrap();
        let mut st = StringTable::new();
        st.init(&mut pager).unwrap();

        // String exactly at the inline limit (MAX_INLINE - 2 bytes for len prefix)
        let exact = "E".repeat(MAX_INLINE - 2);
        st.intern(&exact, &mut pager).unwrap();
        assert_eq!(st.resolve(0).unwrap(), exact);

        // String one byte over the inline limit — triggers overflow
        let over = "F".repeat(MAX_INLINE - 1);
        st.intern(&over, &mut pager).unwrap();
        assert_eq!(st.resolve(1).unwrap(), over);

        // Load from disk
        let pages = st.page_numbers().to_vec();
        let st2 = StringTable::load(&pages, &mut pager).unwrap();
        assert_eq!(st2.resolve(0).unwrap(), exact);
        assert_eq!(st2.resolve(1).unwrap(), over);

        cleanup(&path);
    }
}
