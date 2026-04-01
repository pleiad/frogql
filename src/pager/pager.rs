use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::header::FileHeader;
use super::page::{Page, PageType, PAGE_SIZE};

/// The Pager manages a single database file as a sequence of fixed-size pages.
///
/// Page 0 is always the file header. Pages 1+ are data/index pages.
/// The pager handles:
/// - Reading and writing individual pages by page number
/// - Allocating new pages (either from the free list or by extending the file)
/// - Freeing pages (adding them to the free list)
/// - Persisting the file header
pub struct Pager {
    file: File,
    path: PathBuf,
    pub header: FileHeader,
}

impl Pager {
    /// Create a new database file. Fails if the file already exists.
    pub fn create(path: &Path) -> io::Result<Self> {
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
        };

        // Write the initial header page
        pager.write_header()?;
        Ok(pager)
    }

    /// Open an existing database file.
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;

        // Read page 0
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
        })
    }

    /// Read a page by page number.
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

        let offset = page_num as u64 * PAGE_SIZE as u64;
        self.file.seek(SeekFrom::Start(offset))?;

        let mut buf = [0u8; PAGE_SIZE];
        self.file.read_exact(&mut buf)?;
        Ok(Page::from_bytes(buf))
    }

    /// Write a page at the given page number.
    pub fn write_page(&mut self, page_num: u32, page: &Page) -> io::Result<()> {
        let offset = page_num as u64 * PAGE_SIZE as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&page.data)?;
        Ok(())
    }

    /// Allocate a new page. Reuses from the free list if available,
    /// otherwise extends the file.
    pub fn allocate_page(&mut self) -> io::Result<u32> {
        if self.header.free_list_head != 0 {
            // Pop from free list
            let page_num = self.header.free_list_head;
            let free_page = self.read_page(page_num)?;

            // The first 4 bytes of a free page store the next free page number
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
            // Extend the file
            let page_num = self.header.page_count;
            self.header.page_count += 1;

            // Write a zeroed page to extend the file
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

        // Write the current free list head into the first 4 bytes of the freed page
        let mut free_page = Page::new(PageType::Free);
        let next_bytes = self.header.free_list_head.to_le_bytes();
        free_page.data[0..4].copy_from_slice(&next_bytes);

        self.write_page(page_num, &free_page)?;
        self.header.free_list_head = page_num;
        self.write_header()?;

        Ok(())
    }

    /// Flush the file header to page 0.
    pub fn write_header(&mut self) -> io::Result<()> {
        let page = self.header.to_page();
        let offset = 0u64;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&page.data)?;
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

            // Write data to page 1
            let mut page = Page::new(PageType::NodeData);
            page.insert_cell(b"hello");
            pager.write_page(pg1, &page).unwrap();
        }

        // Reopen and verify
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

        let p1 = pager.allocate_page().unwrap(); // page 1
        let p2 = pager.allocate_page().unwrap(); // page 2
        let _p3 = pager.allocate_page().unwrap(); // page 3
        assert_eq!(pager.page_count(), 4);

        // Free page 2
        pager.free_page(p2).unwrap();
        assert_eq!(pager.header.free_list_head, 2);

        // Free page 1
        pager.free_page(p1).unwrap();
        assert_eq!(pager.header.free_list_head, 1);

        // Allocate should reuse page 1 first (LIFO)
        let reused1 = pager.allocate_page().unwrap();
        assert_eq!(reused1, 1);

        // Then page 2
        let reused2 = pager.allocate_page().unwrap();
        assert_eq!(reused2, 2);

        // Next allocation extends the file
        let p4 = pager.allocate_page().unwrap();
        assert_eq!(p4, 4);
        assert_eq!(pager.page_count(), 5);

        cleanup(&path);
    }

    #[test]
    fn test_free_list_persistence() {
        let path = temp_path("test_freelist_persist.gql");
        cleanup(&path);

        {
            let mut pager = Pager::create(&path).unwrap();
            pager.allocate_page().unwrap(); // 1
            pager.allocate_page().unwrap(); // 2
            pager.free_page(1).unwrap();
        }

        // Reopen — free list should still have page 1
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
}
