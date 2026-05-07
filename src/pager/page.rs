/// Page size in bytes (4 KiB).
pub const PAGE_SIZE: usize = 4096;

/// Page header occupies the first 8 bytes of every page.
const PAGE_HEADER_SIZE: usize = 8;

/// Each cell pointer is a u16 offset into the page.
const CELL_PTR_SIZE: usize = 2;

/// Page types stored in the first byte of the page header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageType {
    /// File header page (page 0 only).
    FileHeader = 0,
    /// Node record storage.
    NodeData = 1,
    /// Edge record storage.
    EdgeData = 2,
    /// Adjacency list storage.
    Adjacency = 3,
    /// Label index storage.
    LabelIndex = 4,
    /// String table storage.
    StringTable = 5,
    /// Overflow page for large records.
    Overflow = 6,
    /// Free page (on the free list).
    Free = 7,
    /// Graph-type catalog page (chained, JSON payload).
    Catalog = 8,
    /// Persisted secondary-index DDL list (chained, JSON payload).
    SecondaryIndex = 9,
}

impl PageType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::FileHeader),
            1 => Some(Self::NodeData),
            2 => Some(Self::EdgeData),
            3 => Some(Self::Adjacency),
            4 => Some(Self::LabelIndex),
            5 => Some(Self::StringTable),
            6 => Some(Self::Overflow),
            7 => Some(Self::Free),
            8 => Some(Self::Catalog),
            9 => Some(Self::SecondaryIndex),
            _ => None,
        }
    }
}

/// A fixed-size database page.
///
/// Layout:
/// ```text
/// [Page Header: 8 bytes]
///   byte 0:    page_type (u8)
///   byte 1:    flags (u8, reserved)
///   bytes 2-3: cell_count (u16 LE)
///   bytes 4-5: cell_area_start (u16 LE) — offset where cell data begins (grows down from end)
///   bytes 6-7: reserved
///
/// [Cell Pointer Array: cell_count * 2 bytes]
///   Each entry is a u16 LE offset pointing to the start of a cell in the cell area.
///   Grows forward from byte 8.
///
/// [Free Space]
///
/// [Cell Data Area]
///   Cells are written from the end of the page growing backward.
///   cell_area_start points to the lowest used byte in this region.
/// ```
pub struct Page {
    pub data: [u8; PAGE_SIZE],
}

impl Page {
    /// Create a new empty page of the given type.
    pub fn new(page_type: PageType) -> Self {
        let mut data = [0u8; PAGE_SIZE];
        data[0] = page_type as u8;
        // cell_count = 0
        data[2] = 0;
        data[3] = 0;
        // cell_area_start = PAGE_SIZE (no cells yet)
        let page_size = PAGE_SIZE as u16;
        data[4] = (page_size & 0xFF) as u8;
        data[5] = (page_size >> 8) as u8;
        Page { data }
    }

    /// Create a page from raw bytes.
    pub fn from_bytes(data: [u8; PAGE_SIZE]) -> Self {
        Page { data }
    }

    pub fn page_type(&self) -> PageType {
        PageType::from_u8(self.data[0]).unwrap_or(PageType::Free)
    }

    pub fn set_page_type(&mut self, pt: PageType) {
        self.data[0] = pt as u8;
    }

    pub fn cell_count(&self) -> u16 {
        u16::from_le_bytes([self.data[2], self.data[3]])
    }

    fn set_cell_count(&mut self, count: u16) {
        let bytes = count.to_le_bytes();
        self.data[2] = bytes[0];
        self.data[3] = bytes[1];
    }

    /// Offset where the cell data area begins (lowest address of cell data).
    pub fn cell_area_start(&self) -> u16 {
        u16::from_le_bytes([self.data[4], self.data[5]])
    }

    fn set_cell_area_start(&mut self, offset: u16) {
        let bytes = offset.to_le_bytes();
        self.data[4] = bytes[0];
        self.data[5] = bytes[1];
    }

    /// End of the cell pointer array (start of free space).
    fn ptr_array_end(&self) -> usize {
        PAGE_HEADER_SIZE + self.cell_count() as usize * CELL_PTR_SIZE
    }

    /// Available free space in the page.
    pub fn free_space(&self) -> usize {
        let ptr_end = self.ptr_array_end();
        let cell_start = self.cell_area_start() as usize;
        cell_start.saturating_sub(ptr_end)
    }

    /// Insert a cell (variable-length byte slice) into this page.
    /// Returns the cell index (0-based) on success, or None if not enough space.
    pub fn insert_cell(&mut self, cell_data: &[u8]) -> Option<u16> {
        let needed = CELL_PTR_SIZE + cell_data.len();
        if self.free_space() < needed {
            return None;
        }

        let count = self.cell_count();

        // Allocate space in the cell area (growing down from the end)
        let new_cell_start = self.cell_area_start() as usize - cell_data.len();
        self.data[new_cell_start..new_cell_start + cell_data.len()].copy_from_slice(cell_data);
        self.set_cell_area_start(new_cell_start as u16);

        // Write the cell pointer at the end of the pointer array
        let ptr_offset = PAGE_HEADER_SIZE + count as usize * CELL_PTR_SIZE;
        let cell_offset_bytes = (new_cell_start as u16).to_le_bytes();
        self.data[ptr_offset] = cell_offset_bytes[0];
        self.data[ptr_offset + 1] = cell_offset_bytes[1];

        self.set_cell_count(count + 1);
        Some(count)
    }

    /// Get a reference to the cell data at the given index.
    /// Returns (offset, length) — but we need to know the cell length.
    /// For variable-length cells, the length is encoded in the cell itself.
    /// This returns the raw bytes from the cell offset to the next cell (or page end).
    pub fn cell_offset(&self, index: u16) -> Option<u16> {
        if index >= self.cell_count() {
            return None;
        }
        let ptr_pos = PAGE_HEADER_SIZE + index as usize * CELL_PTR_SIZE;
        Some(u16::from_le_bytes([
            self.data[ptr_pos],
            self.data[ptr_pos + 1],
        ]))
    }

    /// Read `len` bytes starting at `offset` within the page.
    pub fn read_at(&self, offset: usize, len: usize) -> &[u8] {
        &self.data[offset..offset + len]
    }

    /// Write bytes at a specific offset (for updating in place).
    pub fn write_at(&mut self, offset: usize, data: &[u8]) {
        self.data[offset..offset + data.len()].copy_from_slice(data);
    }
}

impl Clone for Page {
    fn clone(&self) -> Self {
        Page { data: self.data }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_page() {
        let p = Page::new(PageType::NodeData);
        assert_eq!(p.page_type(), PageType::NodeData);
        assert_eq!(p.cell_count(), 0);
        assert_eq!(p.cell_area_start(), PAGE_SIZE as u16);
        assert_eq!(p.free_space(), PAGE_SIZE - PAGE_HEADER_SIZE);
    }

    #[test]
    fn test_insert_cell() {
        let mut p = Page::new(PageType::NodeData);
        let data = b"hello world";
        let idx = p.insert_cell(data).unwrap();
        assert_eq!(idx, 0);
        assert_eq!(p.cell_count(), 1);

        // Read it back
        let offset = p.cell_offset(0).unwrap() as usize;
        assert_eq!(p.read_at(offset, data.len()), data);
    }

    #[test]
    fn test_insert_multiple_cells() {
        let mut p = Page::new(PageType::NodeData);
        let d1 = b"first";
        let d2 = b"second";
        let d3 = b"third";

        let i0 = p.insert_cell(d1).unwrap();
        let i1 = p.insert_cell(d2).unwrap();
        let i2 = p.insert_cell(d3).unwrap();
        assert_eq!(i0, 0);
        assert_eq!(i1, 1);
        assert_eq!(i2, 2);
        assert_eq!(p.cell_count(), 3);

        // Cells grow downward — first inserted is closest to end
        let o0 = p.cell_offset(0).unwrap() as usize;
        let o1 = p.cell_offset(1).unwrap() as usize;
        let o2 = p.cell_offset(2).unwrap() as usize;
        assert!(o0 > o1); // first cell is at higher offset
        assert!(o1 > o2);

        assert_eq!(p.read_at(o0, d1.len()), d1);
        assert_eq!(p.read_at(o1, d2.len()), d2);
        assert_eq!(p.read_at(o2, d3.len()), d3);
    }

    #[test]
    fn test_page_full() {
        let mut p = Page::new(PageType::NodeData);
        // Fill the page with large cells
        let big = vec![0xABu8; 2000];
        assert!(p.insert_cell(&big).is_some());
        assert!(p.insert_cell(&big).is_some());
        // Third should fail — not enough space
        assert!(p.insert_cell(&big).is_none());
    }

    #[test]
    fn test_free_space_accounting() {
        let mut p = Page::new(PageType::NodeData);
        let initial_free = p.free_space();
        let data = vec![0u8; 100];
        p.insert_cell(&data);
        // Lost: 100 bytes for cell + 2 bytes for pointer
        assert_eq!(p.free_space(), initial_free - 102);
    }
}
