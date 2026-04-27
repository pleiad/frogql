use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::header::FileHeader;
use super::page::{Page, PageType, PAGE_SIZE};

/// Default page cache size (number of pages).
const DEFAULT_CACHE_SIZE: usize = 2000;

/// The Pager manages a single database file as a sequence of fixed-size pages.
///
/// All page reads go through an LRU cache. The cache holds up to `cache_size`
/// pages in memory. On a miss, the least recently used page is evicted.
///
/// Page 0 is always the file header. Pages 1+ are data/index pages.
pub struct Pager {
    file: File,
    path: PathBuf,
    pub header: FileHeader,
    // LRU page cache: page_num → (page_data, last_access_tick)
    cache: HashMap<u32, CacheEntry>,
    cache_size: usize,
    tick: u64,
    cache_hits: u64,
    cache_misses: u64,
}

struct CacheEntry {
    page: Page,
    last_access: u64,
    dirty: bool,
}

impl Pager {
    /// Create a new database file. Fails if the file already exists.
    pub fn create(path: &Path) -> io::Result<Self> {
        Self::create_with_cache(path, DEFAULT_CACHE_SIZE)
    }

    pub fn create_with_cache(path: &Path, cache_size: usize) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;

        let header = FileHeader::new();
        let mut pager = Pager {
            file,
            path: path.to_path_buf(),
            header,
            cache: HashMap::new(),
            cache_size,
            tick: 0,
            cache_hits: 0,
            cache_misses: 0,
        };

        pager.write_header()?;
        Ok(pager)
    }

    /// Open an existing database file.
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_with_cache(path, DEFAULT_CACHE_SIZE)
    }

    pub fn open_with_cache(path: &Path, cache_size: usize) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;

        let mut buf = [0u8; PAGE_SIZE];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut buf)?;

        let page0 = Page::from_bytes(buf);
        let header = FileHeader::from_page(&page0)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        Ok(Pager {
            file,
            path: path.to_path_buf(),
            header,
            cache: HashMap::new(),
            cache_size,
            tick: 0,
            cache_hits: 0,
            cache_misses: 0,
        })
    }

    /// Read a page by page number. Goes through the LRU cache.
    pub fn read_page(&mut self, page_num: u32) -> io::Result<Page> {
        if page_num >= self.header.page_count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "page {page_num} out of range (total: {})",
                    self.header.page_count
                ),
            ));
        }

        self.tick += 1;

        // Cache hit
        if let Some(entry) = self.cache.get_mut(&page_num) {
            entry.last_access = self.tick;
            self.cache_hits += 1;
            return Ok(entry.page.clone());
        }

        // Cache miss — read from disk
        self.cache_misses += 1;
        let page = self.read_page_from_disk(page_num)?;

        // Insert into cache (evict LRU if full)
        self.cache_insert(page_num, page.clone(), false);

        Ok(page)
    }

    /// Write a page at the given page number. Updates cache and writes through to disk.
    pub fn write_page(&mut self, page_num: u32, page: &Page) -> io::Result<()> {
        // Write to disk immediately (write-through)
        self.write_page_to_disk(page_num, page)?;

        // Update cache
        self.tick += 1;
        if self.cache.contains_key(&page_num) {
            let entry = self.cache.get_mut(&page_num).unwrap();
            entry.page = page.clone();
            entry.last_access = self.tick;
            entry.dirty = false;
        } else {
            self.cache_insert(page_num, page.clone(), false);
        }

        Ok(())
    }

    /// Allocate a new page. Reuses from the free list if available,
    /// otherwise extends the file.
    pub fn allocate_page(&mut self) -> io::Result<u32> {
        if self.header.free_list_head != 0 {
            let page_num = self.header.free_list_head;
            let free_page = self.read_page(page_num)?;

            let next = u32::from_le_bytes([
                free_page.data[0],
                free_page.data[1],
                free_page.data[2],
                free_page.data[3],
            ]);
            self.header.free_list_head = next;
            self.write_header()?;

            Ok(page_num)
        } else {
            let page_num = self.header.page_count;
            self.header.page_count += 1;

            let blank = Page::new(PageType::Free);
            self.write_page(page_num, &blank)?;
            self.write_header()?;

            Ok(page_num)
        }
    }

    /// Free a page, adding it to the free list.
    pub fn free_page(&mut self, page_num: u32) -> io::Result<()> {
        if page_num == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot free page 0 (file header)",
            ));
        }

        let mut free_page = Page::new(PageType::Free);
        let next_bytes = self.header.free_list_head.to_le_bytes();
        free_page.data[0..4].copy_from_slice(&next_bytes);

        self.write_page(page_num, &free_page)?;
        self.header.free_list_head = page_num;
        self.write_header()?;

        // Evict from cache
        self.cache.remove(&page_num);

        Ok(())
    }

    /// Flush the file header to page 0.
    pub fn write_header(&mut self) -> io::Result<()> {
        let page = self.header.to_page();
        self.write_page_to_disk(0, &page)?;
        self.file.flush()?;
        Ok(())
    }

    /// Total number of pages in the file.
    pub fn page_count(&self) -> u32 {
        self.header.page_count
    }

    /// The file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Cache statistics.
    pub fn cache_stats(&self) -> (u64, u64) {
        (self.cache_hits, self.cache_misses)
    }

    // --- Internal ---

    fn read_page_from_disk(&mut self, page_num: u32) -> io::Result<Page> {
        let offset = page_num as u64 * PAGE_SIZE as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut buf = [0u8; PAGE_SIZE];
        self.file.read_exact(&mut buf)?;
        Ok(Page::from_bytes(buf))
    }

    fn write_page_to_disk(&mut self, page_num: u32, page: &Page) -> io::Result<()> {
        let offset = page_num as u64 * PAGE_SIZE as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&page.data)?;
        Ok(())
    }

    fn cache_insert(&mut self, page_num: u32, page: Page, dirty: bool) {
        if self.cache.len() >= self.cache_size {
            self.evict_lru();
        }
        self.cache.insert(
            page_num,
            CacheEntry {
                page,
                last_access: self.tick,
                dirty,
            },
        );
    }

    fn evict_lru(&mut self) {
        if let Some((&evict_key, _)) = self.cache.iter().min_by_key(|(_, entry)| entry.last_access)
        {
            // If dirty, flush to disk (not used currently — write-through)
            // but included for future write-back cache mode
            if self.cache[&evict_key].dirty {
                let page = &self.cache[&evict_key].page;
                let _ = self.write_page_to_disk(evict_key, &page.clone());
            }
            self.cache.remove(&evict_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("gqlrust_test");
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_create_and_open() {
        let path = temp_path("test_create.gql");
        cleanup(&path);

        {
            let pager = Pager::create(&path).unwrap();
            assert_eq!(pager.page_count(), 1);
        }

        {
            let pager = Pager::open(&path).unwrap();
            assert_eq!(pager.page_count(), 1);
            assert_eq!(pager.header.format_version, 1);
        }

        cleanup(&path);
    }

    #[test]
    fn test_allocate_and_write() {
        let path = temp_path("test_alloc.gql");
        cleanup(&path);

        {
            let mut pager = Pager::create(&path).unwrap();

            let pg1 = pager.allocate_page().unwrap();
            assert_eq!(pg1, 1);
            assert_eq!(pager.page_count(), 2);

            let pg2 = pager.allocate_page().unwrap();
            assert_eq!(pg2, 2);
            assert_eq!(pager.page_count(), 3);

            let mut page = Page::new(PageType::NodeData);
            page.insert_cell(b"hello");
            pager.write_page(pg1, &page).unwrap();
        }

        {
            let mut pager = Pager::open(&path).unwrap();
            assert_eq!(pager.page_count(), 3);

            let page = pager.read_page(1).unwrap();
            assert_eq!(page.page_type(), PageType::NodeData);
            assert_eq!(page.cell_count(), 1);
            let offset = page.cell_offset(0).unwrap() as usize;
            assert_eq!(page.read_at(offset, 5), b"hello");
        }

        cleanup(&path);
    }

    #[test]
    fn test_free_list() {
        let path = temp_path("test_freelist.gql");
        cleanup(&path);

        let mut pager = Pager::create(&path).unwrap();

        let p1 = pager.allocate_page().unwrap();
        let p2 = pager.allocate_page().unwrap();
        let _p3 = pager.allocate_page().unwrap();
        assert_eq!(pager.page_count(), 4);

        pager.free_page(p2).unwrap();
        pager.free_page(p1).unwrap();

        let reused1 = pager.allocate_page().unwrap();
        assert_eq!(reused1, 1);
        let reused2 = pager.allocate_page().unwrap();
        assert_eq!(reused2, 2);

        let p4 = pager.allocate_page().unwrap();
        assert_eq!(p4, 4);

        cleanup(&path);
    }

    #[test]
    fn test_free_list_persistence() {
        let path = temp_path("test_freelist_persist.gql");
        cleanup(&path);

        {
            let mut pager = Pager::create(&path).unwrap();
            pager.allocate_page().unwrap();
            pager.allocate_page().unwrap();
            pager.free_page(1).unwrap();
        }

        {
            let mut pager = Pager::open(&path).unwrap();
            assert_eq!(pager.header.free_list_head, 1);
            let reused = pager.allocate_page().unwrap();
            assert_eq!(reused, 1);
        }

        cleanup(&path);
    }

    #[test]
    fn test_cannot_free_page_zero() {
        let path = temp_path("test_no_free_zero.gql");
        cleanup(&path);
        let mut pager = Pager::create(&path).unwrap();
        assert!(pager.free_page(0).is_err());
        cleanup(&path);
    }

    #[test]
    fn test_read_out_of_range() {
        let path = temp_path("test_out_of_range.gql");
        cleanup(&path);
        let mut pager = Pager::create(&path).unwrap();
        assert!(pager.read_page(99).is_err());
        cleanup(&path);
    }

    #[test]
    fn test_cache_hits() {
        let path = temp_path("test_cache_hits.gql");
        cleanup(&path);

        let mut pager = Pager::create_with_cache(&path, 10).unwrap();
        let pg = pager.allocate_page().unwrap();

        let mut page = Page::new(PageType::NodeData);
        page.insert_cell(b"cached");
        pager.write_page(pg, &page).unwrap();

        // First read: miss (page was in cache from write, actually a hit)
        let _ = pager.read_page(pg).unwrap();
        // Second read: definitely a hit
        let _ = pager.read_page(pg).unwrap();
        let _ = pager.read_page(pg).unwrap();

        let (hits, _misses) = pager.cache_stats();
        assert!(hits >= 2, "expected cache hits, got hits={hits}");

        cleanup(&path);
    }

    #[test]
    fn test_cache_eviction() {
        let path = temp_path("test_cache_evict.gql");
        cleanup(&path);

        // Tiny cache: 3 pages
        let mut pager = Pager::create_with_cache(&path, 3).unwrap();

        // Allocate 5 pages
        let pages: Vec<u32> = (0..5).map(|_| pager.allocate_page().unwrap()).collect();

        // Write data to each
        for &pg in &pages {
            let mut page = Page::new(PageType::NodeData);
            page.insert_cell(format!("page{pg}").as_bytes());
            pager.write_page(pg, &page).unwrap();
        }

        // Cache only holds 3, so reading all 5 forces evictions
        for &pg in &pages {
            let page = pager.read_page(pg).unwrap();
            assert_eq!(page.page_type(), PageType::NodeData);
        }

        let (_hits, misses) = pager.cache_stats();
        assert!(misses > 0, "expected some cache misses with tiny cache");

        cleanup(&path);
    }
}
