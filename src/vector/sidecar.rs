//! On-disk format for a vector attribute.
//!
//! Vectors live **outside** the `.gdb`, one file per vector attribute:
//! `<db>.vec.<attr>`. Two reasons. First, a node record has no extra
//! area, so per-node vectors would have to become ordinary properties,
//! and then every `node_props()` call would decode a 768-float blob it
//! did not ask for. Second, the vectors and their index are built
//! offline, are read-only at query time, and are large; keeping them out
//! of the pager means the `.gdb` save path is untouched.
//!
//! ```text
//! 0   8   magic "FGVEC1\0\0"
//! 8   4   version u32
//! 12  4   dim u32
//! 16  4   count u32
//! 20  1   metric u8
//! 21  1   flags u8            bit 0 = an HNSW section follows
//! 22  2   reserved u16
//! 24  8   fingerprint u64     guards against stale internal node ids
//! 32  4   attr_len u32
//! 36  N   attr, utf-8
//!     4·count        ids, ascending, graph-internal node ids
//!     4·count·dim    data, f32 LE, row i belongs to ids[i]
//!     ...            HNSW section when the flag is set
//! ```
//!
//! The `ids` array defines the stored grouping order for the attribute
//! and is the only mapping from a row to a node. It is kept ascending so
//! `row()` is a binary search.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::vector::hnsw::Hnsw;
use crate::vector::metric::Metric;

pub const MAGIC: &[u8; 8] = b"FGVEC1\0\0";
pub const VERSION: u32 = 1;
pub const FLAG_HAS_HNSW: u8 = 1;
const HEADER_FIXED: usize = 36;

/// The infix that marks a sidecar and separates it from the attribute
/// name: `movies.gdb` + `emb` becomes `movies.gdb.vec.emb`.
pub const SIDECAR_INFIX: &str = ".vec.";

/// Sequential little-endian reader over an in-memory buffer. The whole
/// sidecar is read into RAM before parsing: it is the working set for
/// every query that touches the attribute, so there is nothing to gain
/// from streaming it.
pub struct ByteReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        ByteReader { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "truncated sidecar: wanted {n} bytes at offset {}, {} left",
                    self.pos,
                    self.remaining()
                ),
            ));
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    pub fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> io::Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> io::Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> io::Result<u64> {
        let b = self.take(8)?;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(b);
        Ok(u64::from_le_bytes(arr))
    }

    pub fn bytes(&mut self, n: usize) -> io::Result<&'a [u8]> {
        self.take(n)
    }

    pub fn u32_vec(&mut self, n: usize) -> io::Result<Vec<u32>> {
        let b = self.take(n * 4)?;
        Ok(b.chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect())
    }

    pub fn f32_vec(&mut self, n: usize) -> io::Result<Vec<f32>> {
        let b = self.take(n * 4)?;
        Ok(b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect())
    }
}

/// A decoded sidecar, before the derived state (`VectorSet`) is built.
#[derive(Debug, Clone, PartialEq)]
pub struct Sidecar {
    pub attr: String,
    pub dim: usize,
    pub metric: Metric,
    pub fingerprint: u64,
    /// Graph-internal node ids, ascending, one per row.
    pub ids: Vec<u32>,
    /// Row-major `count × dim`.
    pub data: Vec<f32>,
    pub hnsw: Option<Hnsw>,
}

impl Sidecar {
    pub fn count(&self) -> usize {
        self.ids.len()
    }

    /// The sidecar path for `attr` next to `db_path`.
    pub fn path_for(db_path: &Path, attr: &str) -> PathBuf {
        let mut name = db_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        name.push_str(SIDECAR_INFIX);
        name.push_str(attr);
        match db_path.parent() {
            Some(dir) => dir.join(name),
            None => PathBuf::from(name),
        }
    }

    /// Recover the attribute name from a sidecar path, given the database
    /// it belongs to. `None` when the file is not a sidecar of `db_path`.
    pub fn attr_from_path(db_path: &Path, sidecar: &Path) -> Option<String> {
        let db_name = db_path.file_name()?.to_string_lossy().into_owned();
        let file = sidecar.file_name()?.to_string_lossy().into_owned();
        let prefix = format!("{db_name}{SIDECAR_INFIX}");
        let attr = file.strip_prefix(&prefix)?;
        if attr.is_empty() {
            None
        } else {
            Some(attr.to_string())
        }
    }

    /// List the sidecars sitting next to `db_path`, as
    /// `(attribute, path)` pairs sorted by attribute. A missing or
    /// unreadable directory yields an empty list: sidecars are optional.
    pub fn discover(db_path: &Path) -> Vec<(String, PathBuf)> {
        let dir = match db_path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<(String, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                Sidecar::attr_from_path(db_path, &path).map(|attr| (attr, path))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn encode(&self) -> Vec<u8> {
        let attr_bytes = self.attr.as_bytes();
        let mut flags = 0u8;
        if self.hnsw.is_some() {
            flags |= FLAG_HAS_HNSW;
        }
        let mut out = Vec::with_capacity(
            HEADER_FIXED + attr_bytes.len() + self.ids.len() * 4 + self.data.len() * 4,
        );
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.dim as u32).to_le_bytes());
        out.extend_from_slice(&(self.ids.len() as u32).to_le_bytes());
        out.push(self.metric.as_u8());
        out.push(flags);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.fingerprint.to_le_bytes());
        out.extend_from_slice(&(attr_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(attr_bytes);
        for &id in &self.ids {
            out.extend_from_slice(&id.to_le_bytes());
        }
        for &v in &self.data {
            out.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(h) = &self.hnsw {
            h.encode(&mut out);
        }
        out
    }

    pub fn decode(buf: &[u8]) -> io::Result<Sidecar> {
        let mut rd = ByteReader::new(buf);
        let magic = rd.bytes(8)?;
        if magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a froGQL vector sidecar (bad magic)",
            ));
        }
        let version = rd.u32()?;
        if version != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("sidecar version {version} is not supported (expected {VERSION})"),
            ));
        }
        let dim = rd.u32()? as usize;
        let count = rd.u32()? as usize;
        let metric_tag = rd.u8()?;
        let metric = Metric::from_u8(metric_tag).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown metric tag {metric_tag}"),
            )
        })?;
        let flags = rd.u8()?;
        let _reserved = rd.u16()?;
        let fingerprint = rd.u64()?;
        let attr_len = rd.u32()? as usize;
        let attr = String::from_utf8(rd.bytes(attr_len)?.to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        if dim == 0 && count > 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sidecar declares dim 0 with a non-empty row set",
            ));
        }

        let ids = rd.u32_vec(count)?;
        if ids.windows(2).any(|w| w[0] >= w[1]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sidecar ids must be strictly ascending",
            ));
        }
        let data = rd.f32_vec(count * dim)?;

        let hnsw = if flags & FLAG_HAS_HNSW != 0 {
            let h = Hnsw::decode(&mut rd)?;
            h.validate(count)?;
            Some(h)
        } else {
            None
        };

        Ok(Sidecar {
            attr,
            dim,
            metric,
            fingerprint,
            ids,
            data,
            hnsw,
        })
    }

    pub fn read_from_path(path: &Path) -> io::Result<Sidecar> {
        let buf = fs::read(path)?;
        Sidecar::decode(&buf)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
    }

    /// Write atomically: full contents to `<path>.tmp`, then rename. The
    /// same discipline as `save_graph_with_catalog_and_indexes_atomic`,
    /// so a crash mid-write never leaves a half-parsed sidecar that
    /// would be read as authoritative.
    pub fn write_to_path(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension(format!(
            "{}tmp",
            path.extension()
                .map(|e| format!("{}.", e.to_string_lossy()))
                .unwrap_or_default()
        ));
        fs::write(&tmp, self.encode())?;
        fs::rename(&tmp, path)
    }
}

/// Coarse guard against a sidecar outliving the database it was built
/// from.
///
/// The `ids` in a sidecar are graph-internal, and `save()` renumbers
/// every node when it compacts tombstones away, so a sidecar built
/// before a delete-then-save silently points at the wrong nodes. Node
/// and edge counts change under exactly that operation, which is what
/// this catches. It does **not** catch a delete plus an equal-sized
/// insert; the second line of defence is that vector search is disabled
/// outright once a session has performed any DML.
pub fn fingerprint(node_count: usize, edge_count: usize) -> u64 {
    // FNV-1a over the two counts. Cheap, no dependency, and only ever
    // compared for equality.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in (node_count as u64)
        .to_le_bytes()
        .iter()
        .chain((edge_count as u64).to_le_bytes().iter())
    {
        h ^= *byte as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Sidecar {
        Sidecar {
            attr: "emb".to_string(),
            dim: 2,
            metric: Metric::L2Sq,
            fingerprint: 0xdead_beef,
            ids: vec![1, 4, 9],
            data: vec![0.0, 0.0, 1.0, 1.0, 2.0, -2.0],
            hnsw: None,
        }
    }

    #[test]
    fn encode_decode_round_trips() {
        let s = sample();
        assert_eq!(Sidecar::decode(&s.encode()).expect("decode"), s);
    }

    #[test]
    fn encode_decode_round_trips_with_hnsw() {
        let mut s = sample();
        s.hnsw = Some(Hnsw {
            m: 4,
            m0: 8,
            ef_construction: 32,
            entry: 0,
            levels: vec![vec![vec![1, 2]], vec![vec![0]], vec![vec![0]]],
        });
        assert_eq!(Sidecar::decode(&s.encode()).expect("decode"), s);
    }

    #[test]
    fn empty_sidecar_round_trips() {
        let s = Sidecar {
            attr: "e".to_string(),
            dim: 0,
            metric: Metric::Cosine,
            fingerprint: 0,
            ids: vec![],
            data: vec![],
            hnsw: None,
        };
        assert_eq!(Sidecar::decode(&s.encode()).expect("decode"), s);
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let mut bytes = sample().encode();
        bytes[0] = b'X';
        assert!(Sidecar::decode(&bytes).is_err());
    }

    #[test]
    fn decode_rejects_a_future_version() {
        let mut bytes = sample().encode();
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert!(Sidecar::decode(&bytes).is_err());
    }

    #[test]
    fn decode_rejects_an_unknown_metric() {
        let mut bytes = sample().encode();
        bytes[20] = 200;
        assert!(Sidecar::decode(&bytes).is_err());
    }

    #[test]
    fn decode_rejects_truncation() {
        let bytes = sample().encode();
        assert!(Sidecar::decode(&bytes[..bytes.len() - 4]).is_err());
    }

    #[test]
    fn decode_rejects_unsorted_ids() {
        let mut s = sample();
        s.ids = vec![9, 4, 1];
        assert!(Sidecar::decode(&s.encode()).is_err());
    }

    #[test]
    fn decode_rejects_duplicate_ids() {
        let mut s = sample();
        s.ids = vec![1, 1, 9];
        assert!(Sidecar::decode(&s.encode()).is_err());
    }

    #[test]
    fn decode_rejects_an_out_of_range_hnsw_neighbour() {
        let mut s = sample();
        s.hnsw = Some(Hnsw {
            m: 4,
            m0: 8,
            ef_construction: 32,
            entry: 0,
            levels: vec![vec![vec![77]], vec![vec![0]], vec![vec![0]]],
        });
        assert!(Sidecar::decode(&s.encode()).is_err());
    }

    #[test]
    fn path_for_appends_the_infix() {
        let p = Sidecar::path_for(Path::new("/tmp/movies.gdb"), "emb");
        assert_eq!(p, PathBuf::from("/tmp/movies.gdb.vec.emb"));
    }

    #[test]
    fn path_for_handles_a_bare_filename() {
        let p = Sidecar::path_for(Path::new("movies.gdb"), "emb");
        assert_eq!(p, PathBuf::from("movies.gdb.vec.emb"));
    }

    #[test]
    fn attr_from_path_is_the_inverse_of_path_for() {
        let db = Path::new("/tmp/movies.gdb");
        let p = Sidecar::path_for(db, "imgEmb");
        assert_eq!(Sidecar::attr_from_path(db, &p).as_deref(), Some("imgEmb"));
    }

    #[test]
    fn attr_from_path_rejects_foreign_files() {
        let db = Path::new("/tmp/movies.gdb");
        assert_eq!(
            Sidecar::attr_from_path(db, Path::new("/tmp/movies.gdb")),
            None
        );
        assert_eq!(
            Sidecar::attr_from_path(db, Path::new("/tmp/other.gdb.vec.emb")),
            None
        );
        assert_eq!(
            Sidecar::attr_from_path(db, Path::new("/tmp/movies.gdb.vec.")),
            None
        );
    }

    #[test]
    fn write_read_round_trips_on_disk() {
        let dir = std::env::temp_dir().join(format!("frogql_vec_{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        let db = dir.join("t.gdb");
        let s = sample();
        let path = Sidecar::path_for(&db, &s.attr);
        s.write_to_path(&path).expect("write");
        assert_eq!(Sidecar::read_from_path(&path).expect("read"), s);

        let found = Sidecar::discover(&db);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "emb");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discover_on_a_missing_directory_is_empty() {
        assert!(Sidecar::discover(Path::new("/nonexistent-dir-xyz/t.gdb")).is_empty());
    }

    #[test]
    fn fingerprint_separates_different_shapes() {
        assert_eq!(fingerprint(10, 20), fingerprint(10, 20));
        assert_ne!(fingerprint(10, 20), fingerprint(20, 10));
        assert_ne!(fingerprint(10, 20), fingerprint(10, 21));
    }
}
