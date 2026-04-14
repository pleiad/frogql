use super::page::{Page, PageType, PAGE_SIZE};

/// Magic bytes identifying a GQL database file.
pub const MAGIC: &[u8; 6] = b"GQLDB\0";

/// Current file format version.
pub const FORMAT_VERSION: u16 = 1;

/// The file header lives on page 0 and stores database-level metadata.
///
/// Layout (within the page 0 data, after the standard page header at byte 0):
/// ```text
/// bytes  0-5:  magic "GQLDB\0"
/// bytes  6-7:  format version (u16 LE)
/// bytes  8-11: page size (u32 LE, always 4096 for now)
/// bytes 12-15: total page count (u32 LE)
/// bytes 16-19: free list head (u32 LE, page number of first free page; 0 = none)
/// bytes 20-23: node count (u32 LE)
/// bytes 24-27: edge count (u32 LE)
/// bytes 28-31: string table root page (u32 LE)
/// bytes 32-35: node data first page (u32 LE)
/// bytes 36-39: edge data first page (u32 LE)
/// bytes 40-43: node label index root page (u32 LE)
/// bytes 44-47: adjacency root page (u32 LE)
/// bytes 48-51: edge label index root page (u32 LE)
/// bytes 52-55: node locations index root (u32 LE, 0 = legacy file without fast index)
/// bytes 56-59: edge topology index root (u32 LE, 0 = legacy file without fast index)
/// bytes 60+:   reserved (zeroed)
/// ```
///
/// Note: page 0 does NOT use the standard slotted-page cell machinery.
/// The entire page is dedicated to the file header.
pub struct FileHeader {
    pub format_version: u16,
    pub page_size: u32,
    pub page_count: u32,
    pub free_list_head: u32,
    pub node_count: u32,
    pub edge_count: u32,
    pub string_table_root: u32,
    pub node_data_first: u32,
    pub edge_data_first: u32,
    pub label_index_root: u32,   // node labels
    pub adjacency_root: u32,
    pub edge_label_index_root: u32,
    /// Root page for node record locations index (0 = not present, legacy file).
    pub node_locs_root: u32,
    /// Root page for edge topology index (0 = not present, legacy file).
    pub edge_topo_root: u32,
}

impl FileHeader {
    /// Create a fresh header for a new database.
    pub fn new() -> Self {
        FileHeader {
            format_version: FORMAT_VERSION,
            page_size: PAGE_SIZE as u32,
            page_count: 1,
            free_list_head: 0,
            node_count: 0,
            edge_count: 0,
            string_table_root: 0,
            node_data_first: 0,
            edge_data_first: 0,
            label_index_root: 0,
            adjacency_root: 0,
            edge_label_index_root: 0,
            node_locs_root: 0,
            edge_topo_root: 0,
        }
    }

    /// Serialize the header into page 0.
    pub fn to_page(&self) -> Page {
        let mut page = Page::new(PageType::FileHeader);
        let d = &mut page.data;

        d[0..6].copy_from_slice(MAGIC);
        d[6..8].copy_from_slice(&self.format_version.to_le_bytes());
        d[8..12].copy_from_slice(&self.page_size.to_le_bytes());
        d[12..16].copy_from_slice(&self.page_count.to_le_bytes());
        d[16..20].copy_from_slice(&self.free_list_head.to_le_bytes());
        d[20..24].copy_from_slice(&self.node_count.to_le_bytes());
        d[24..28].copy_from_slice(&self.edge_count.to_le_bytes());
        d[28..32].copy_from_slice(&self.string_table_root.to_le_bytes());
        d[32..36].copy_from_slice(&self.node_data_first.to_le_bytes());
        d[36..40].copy_from_slice(&self.edge_data_first.to_le_bytes());
        d[40..44].copy_from_slice(&self.label_index_root.to_le_bytes());
        d[44..48].copy_from_slice(&self.adjacency_root.to_le_bytes());
        d[48..52].copy_from_slice(&self.edge_label_index_root.to_le_bytes());
        d[52..56].copy_from_slice(&self.node_locs_root.to_le_bytes());
        d[56..60].copy_from_slice(&self.edge_topo_root.to_le_bytes());

        page
    }

    /// Deserialize the header from page 0 bytes.
    pub fn from_page(page: &Page) -> Result<Self, String> {
        let d = &page.data;

        if &d[0..6] != MAGIC {
            return Err("not a GQL database file (bad magic)".into());
        }

        Ok(FileHeader {
            format_version: u16::from_le_bytes([d[6], d[7]]),
            page_size: u32::from_le_bytes([d[8], d[9], d[10], d[11]]),
            page_count: u32::from_le_bytes([d[12], d[13], d[14], d[15]]),
            free_list_head: u32::from_le_bytes([d[16], d[17], d[18], d[19]]),
            node_count: u32::from_le_bytes([d[20], d[21], d[22], d[23]]),
            edge_count: u32::from_le_bytes([d[24], d[25], d[26], d[27]]),
            string_table_root: u32::from_le_bytes([d[28], d[29], d[30], d[31]]),
            node_data_first: u32::from_le_bytes([d[32], d[33], d[34], d[35]]),
            edge_data_first: u32::from_le_bytes([d[36], d[37], d[38], d[39]]),
            label_index_root: u32::from_le_bytes([d[40], d[41], d[42], d[43]]),
            adjacency_root: u32::from_le_bytes([d[44], d[45], d[46], d[47]]),
            edge_label_index_root: u32::from_le_bytes([d[48], d[49], d[50], d[51]]),
            node_locs_root: u32::from_le_bytes([d[52], d[53], d[54], d[55]]),
            edge_topo_root: u32::from_le_bytes([d[56], d[57], d[58], d[59]]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_roundtrip() {
        let mut h = FileHeader::new();
        h.page_count = 42;
        h.free_list_head = 7;
        h.node_count = 100;
        h.edge_count = 200;
        h.string_table_root = 2;
        h.node_data_first = 3;
        h.edge_data_first = 10;
        h.label_index_root = 15;
        h.adjacency_root = 20;

        let page = h.to_page();
        let h2 = FileHeader::from_page(&page).unwrap();

        assert_eq!(h2.format_version, FORMAT_VERSION);
        assert_eq!(h2.page_size, PAGE_SIZE as u32);
        assert_eq!(h2.page_count, 42);
        assert_eq!(h2.free_list_head, 7);
        assert_eq!(h2.node_count, 100);
        assert_eq!(h2.edge_count, 200);
        assert_eq!(h2.string_table_root, 2);
        assert_eq!(h2.node_data_first, 3);
        assert_eq!(h2.edge_data_first, 10);
        assert_eq!(h2.label_index_root, 15);
        assert_eq!(h2.adjacency_root, 20);
    }

    #[test]
    fn test_bad_magic() {
        let mut page = Page::new(PageType::FileHeader);
        page.data[0] = b'X'; // corrupt magic
        assert!(FileHeader::from_page(&page).is_err());
    }
}
