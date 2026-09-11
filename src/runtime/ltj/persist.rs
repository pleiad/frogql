//! Persisting the LTJ index to a sidecar, so opening a database does not
//! rebuild it.
//!
//! # Why
//!
//! Building the index is `O(E log E)` and lands entirely at open. On LDBC
//! SF0.1 that is 670 ms and nobody notices. On a 617 M-edge RDF dump it is
//! **252 seconds of every session**, measured, for a structure that is a
//! pure function of a graph that did not change since the last time it was
//! computed.
//!
//! # Why a sidecar and not the `.gdb`
//!
//! `<db>.ltj` sits beside the database, exactly as the vector sidecars do.
//! That buys three things a header root would not:
//!
//! - the `.gdb` format is untouched, so no existing file can be broken by
//!   this and no reader needs to learn a new page type;
//! - the file can be deleted to force a rebuild, which is the whole
//!   recovery story;
//! - the `.gdb` does not grow by 25 GiB for users who never wanted it.
//!
//! # The invariant
//!
//! A stale index is worse than no index: it answers with edges that are
//! gone and misses edges that are new, and nothing downstream re-checks
//! it. Four things are therefore verified before a byte of payload is
//! trusted, and any mismatch is reported as "rebuild", never as an error:
//!
//! | guard | catches |
//! |---|---|
//! | magic | a file that is not one of these |
//! | version | a layout this build does not know |
//! | word size + endianness | a sidecar written on another machine |
//! | graph fingerprint | a `.gdb` that changed under it |
//!
//! The fingerprint mixes the graph's **identity stamp** with its node and
//! edge counts. The counts alone were the earlier signature and they miss
//! the case that matters most once the sidecar is written automatically:
//! `save` compacts ids, so a delete plus an equal-sized insert comes back
//! with both counts unchanged while every id past the deletion names a
//! different element — the sidecar is accepted and answers about the
//! wrong nodes, in silence. `FileHeader::graph_id` is stamped fresh on
//! every save, so it moves whenever the file was rewritten, whatever the
//! counts did. A legacy `.gdb` written before the slot existed reports
//! `0` and is judged on the counts alone, which is the guarantee it
//! already had.
//!
//! # Layout
//!
//! Little-endian, and every array starts on an 8-byte boundary. Both
//! properties are deliberate: they cost a few padding bytes and they are
//! what would let a later mmap hand out `&[u64]` views over the mapped
//! bytes with no copy at all. This module does not do that — it reads into
//! the same `Vec`s the builder produces, so a query cannot tell a loaded
//! index from a built one — but the format does not foreclose it.
//!
//! ```text
//! magic        8   b"FROGLTJ1"
//! version      4   u32
//! word_bits    4   u32   (8 * size_of::<usize>())
//! endian       8   u64   0x0102_0304_0506_0708, read back to detect a swap
//! fingerprint  8   u64   (graph id + node count + edge count)
//! repr         1   u8    0 = six sorted arrays, 1 = compact LOUDS
//! pad          7
//! labels          u64 count, then per label: u64 byte length + bytes + pad
//! payload         repr-specific, see `encode_array` / `encode_compact`
//! ```

use std::io;
use std::path::{Path, PathBuf};

use crate::model::graph_access::SidecarKey;

use super::compact::{CompactTrie, CompactTripleIndex, IntSeq, SelectBitVec};
use super::triple_index::{IndexEntry, IndexRepr, TripleIndex};

const MAGIC: &[u8; 8] = b"FROGLTJ1";
const VERSION: u32 = 2;
// v1 fingerprinted on the counts alone. The bump is what makes an old
// sidecar report "version 1 is not 2" instead of the misleading "graph
// changed" the new fingerprint would produce for a file nobody touched.
/// Written as-is and compared on read: a byte-swapped value means the
/// sidecar came from the other endianness and the payload cannot be
/// reinterpreted.
const ENDIAN_PROBE: u64 = 0x0102_0304_0506_0708;

const REPR_ARRAY: u8 = 0;
const REPR_COMPACT: u8 = 1;

/// The sidecar path for a database: `movies.gdb` -> `movies.gdb.ltj`.
pub fn path_for(db: &Path) -> PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push(".ltj");
    PathBuf::from(s)
}

/// Signature of the graph image the index was built from.
pub fn fingerprint(key: &SidecarKey<'_>) -> u64 {
    // Same mixing as `vector::sidecar::fingerprint`, kept separate so the
    // two sidecars can diverge without silently accepting each other's
    // values.
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in [key.graph_id, key.node_count as u64, key.edge_count as u64] {
        h ^= v;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Why a sidecar was not used. Every variant is a reason to rebuild, not
/// an error to report to a user: the index is a cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    Missing,
    TooShort,
    BadMagic,
    Version(u32),
    WordSize(u32),
    Endianness,
    /// The graph changed since the sidecar was written.
    Fingerprint {
        want: u64,
        got: u64,
    },
    /// The sidecar holds the other representation.
    Repr {
        want: u8,
        got: u8,
    },
    Truncated(&'static str),
    Io(String),
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reject::Missing => write!(f, "no sidecar"),
            Reject::TooShort => write!(f, "sidecar is shorter than its header"),
            Reject::BadMagic => write!(f, "not an LTJ sidecar"),
            Reject::Version(v) => write!(f, "sidecar version {v} is not {VERSION}"),
            Reject::WordSize(w) => write!(f, "sidecar was written on a {w}-bit build"),
            Reject::Endianness => write!(f, "sidecar was written with the other endianness"),
            Reject::Fingerprint { want, got } => {
                write!(
                    f,
                    "graph changed (fingerprint {got:#x}, expected {want:#x})"
                )
            }
            Reject::Repr { want, got } => {
                let name = |r: u8| {
                    if r == REPR_COMPACT {
                        "compact"
                    } else {
                        "array"
                    }
                };
                write!(
                    f,
                    "sidecar holds the {} index, this session wants {}",
                    name(*got),
                    name(*want)
                )
            }
            Reject::Truncated(what) => write!(f, "sidecar ends inside {what}"),
            Reject::Io(e) => write!(f, "{e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Append `n` zero bytes so the next array starts 8-byte aligned.
fn pad(out: &mut Vec<u8>) {
    while out.len() % 8 != 0 {
        out.push(0);
    }
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32_array(out: &mut Vec<u8>, v: &[u32]) {
    put_u64(out, v.len() as u64);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    pad(out);
}

fn put_u64_array(out: &mut Vec<u8>, v: &[u64]) {
    put_u64(out, v.len() as u64);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
}

/// Serialize an index. The graph's signature goes in so the reader can
/// tell whether the `.gdb` moved underneath it.
pub fn encode(index: &TripleIndex, key: &SidecarKey<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(8 * std::mem::size_of::<usize>() as u32).to_le_bytes());
    put_u64(&mut out, ENDIAN_PROBE);
    put_u64(&mut out, fingerprint(key));
    out.push(match index.repr_ref() {
        IndexRepr::Array(_) => REPR_ARRAY,
        IndexRepr::Compact(_) => REPR_COMPACT,
    });
    pad(&mut out);

    // Label dictionary. Ids are positions, so only the strings travel.
    let labels = index.labels();
    put_u64(&mut out, labels.len() as u64);
    for l in labels {
        put_u64(&mut out, l.len() as u64);
        out.extend_from_slice(l.as_bytes());
        pad(&mut out);
    }

    match index.repr_ref() {
        IndexRepr::Array(orderings) => {
            for ord in orderings {
                put_u64(&mut out, ord.len() as u64);
                for (a, b, c, d) in ord {
                    out.extend_from_slice(&a.to_le_bytes());
                    out.extend_from_slice(&b.to_le_bytes());
                    out.extend_from_slice(&c.to_le_bytes());
                    out.extend_from_slice(&d.to_le_bytes());
                }
                pad(&mut out);
            }
        }
        IndexRepr::Compact(c) => {
            put_u64(&mut out, c.raw_len() as u64);
            put_u32_array(&mut out, c.leaf_offsets());
            put_u32_array(&mut out, c.leaf_eids());
            for trie in c.tries() {
                let bv = trie.bv();
                put_u64(&mut out, bv.bit_len() as u64);
                put_u64(&mut out, bv.num_zeros() as u64);
                put_u64_array(&mut out, bv.words());
                put_u32_array(&mut out, bv.samples());
                let seq = trie.seq();
                put_u64(&mut out, seq.width() as u64);
                put_u64(&mut out, seq.seq_len() as u64);
                put_u64_array(&mut out, seq.data());
                put_u64(&mut out, trie.leaf_base() as u64);
            }
        }
    }
    out
}

/// Write the sidecar atomically: full contents to `<path>.tmp`, then
/// rename. A crash mid-write must never leave a half-parsed index that
/// the header would nonetheless accept.
pub fn write_to_path(index: &TripleIndex, path: &Path, key: &SidecarKey<'_>) -> io::Result<()> {
    let tmp = path.with_extension("ltj.tmp");
    std::fs::write(&tmp, encode(index, key))?;
    std::fs::rename(&tmp, path)
}

/// Write the sidecar for the database `key` names. The path is derived
/// rather than passed, so a caller cannot write one graph's index beside
/// another graph's file.
pub fn write_for(index: &TripleIndex, key: &SidecarKey<'_>) -> io::Result<()> {
    write_to_path(index, &path_for(key.path), key)
}

/// Remove the sidecar beside `db`, if there is one. Absence is success:
/// the caller wants "no stale index here", not "a file was deleted".
pub fn remove_for(db: &Path) -> io::Result<()> {
    match std::fs::remove_file(path_for(db)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// A cursor over the sidecar bytes. Every read is bounds-checked and
/// reports which field ran out, because a truncated sidecar is a rebuild
/// and not a panic.
struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn align(&mut self) {
        while self.at % 8 != 0 && self.at < self.buf.len() {
            self.at += 1;
        }
    }

    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], Reject> {
        if self.at + n > self.buf.len() {
            return Err(Reject::Truncated(what));
        }
        let s = &self.buf[self.at..self.at + n];
        self.at += n;
        Ok(s)
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, Reject> {
        let b = self.take(4, what)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self, what: &'static str) -> Result<u64, Reject> {
        let b = self.take(8, what)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    fn usize_(&mut self, what: &'static str) -> Result<usize, Reject> {
        Ok(self.u64(what)? as usize)
    }

    fn u32_array(&mut self, what: &'static str) -> Result<Vec<u32>, Reject> {
        let n = self.usize_(what)?;
        let bytes = self.take(n.checked_mul(4).ok_or(Reject::Truncated(what))?, what)?;
        let out = bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        self.align();
        Ok(out)
    }

    fn u64_array(&mut self, what: &'static str) -> Result<Vec<u64>, Reject> {
        let n = self.usize_(what)?;
        let bytes = self.take(n.checked_mul(8).ok_or(Reject::Truncated(what))?, what)?;
        Ok(bytes
            .chunks_exact(8)
            .map(|c| {
                let mut a = [0u8; 8];
                a.copy_from_slice(c);
                u64::from_le_bytes(a)
            })
            .collect())
    }
}

/// Parse a sidecar, rejecting anything that does not describe *this*
/// graph in *this* representation.
///
/// `want_compact` is the representation the session is configured for:
/// loading the other one would silently change which algorithm runs, so
/// it is a rejection rather than a conversion.
pub fn decode(buf: &[u8], key: &SidecarKey<'_>, want_compact: bool) -> Result<TripleIndex, Reject> {
    let mut c = Cursor { buf, at: 0 };
    if buf.len() < 40 {
        return Err(Reject::TooShort);
    }
    if c.take(8, "magic")? != MAGIC {
        return Err(Reject::BadMagic);
    }
    let version = c.u32("version")?;
    if version != VERSION {
        return Err(Reject::Version(version));
    }
    let word_bits = c.u32("word size")?;
    if word_bits != 8 * std::mem::size_of::<usize>() as u32 {
        return Err(Reject::WordSize(word_bits));
    }
    if c.u64("endian probe")? != ENDIAN_PROBE {
        return Err(Reject::Endianness);
    }
    let want_fp = fingerprint(key);
    let got_fp = c.u64("fingerprint")?;
    if got_fp != want_fp {
        return Err(Reject::Fingerprint {
            want: want_fp,
            got: got_fp,
        });
    }
    let repr = c.take(1, "repr")?[0];
    let want_repr = if want_compact {
        REPR_COMPACT
    } else {
        REPR_ARRAY
    };
    if repr != want_repr {
        return Err(Reject::Repr {
            want: want_repr,
            got: repr,
        });
    }
    c.align();

    let n_labels = c.usize_("label count")?;
    let mut labels: Vec<String> = Vec::with_capacity(n_labels);
    for _ in 0..n_labels {
        let len = c.usize_("label length")?;
        let bytes = c.take(len, "label")?;
        labels.push(String::from_utf8_lossy(bytes).into_owned());
        c.align();
    }

    let repr = if repr == REPR_COMPACT {
        let raw_len = c.usize_("raw_len")?;
        let leaf_offsets = c.u32_array("leaf_offsets")?;
        let leaf_eids = c.u32_array("leaf_eids")?;
        // `[T; 6]` has no `FromIterator`, and `CompactTrie` is not
        // `Default`, so the tries are collected then unwrapped rather
        // than filled in place.
        let mut tries: Vec<CompactTrie> = Vec::with_capacity(6);
        for _ in 0..6 {
            let bit_len = c.usize_("bitvector length")?;
            let num_zeros = c.usize_("zero count")?;
            let words = c.u64_array("bitvector words")?;
            let samples = c.u32_array("select samples")?;
            let width = c.u64("sequence width")? as u32;
            let seq_len = c.usize_("sequence length")?;
            let data = c.u64_array("sequence data")?;
            let leaf_base = c.usize_("leaf base")?;
            tries.push(CompactTrie::from_parts(
                SelectBitVec::from_parts(words, bit_len, samples, num_zeros),
                IntSeq::from_parts(data, width, seq_len),
                leaf_base,
            ));
        }
        let tries: [CompactTrie; 6] = tries
            .try_into()
            .map_err(|_| Reject::Truncated("the six tries"))?;
        IndexRepr::Compact(Box::new(CompactTripleIndex::from_parts(
            tries,
            leaf_offsets,
            leaf_eids,
            raw_len,
        )))
    } else {
        let mut orderings: Vec<Vec<IndexEntry>> = Vec::with_capacity(6);
        for _ in 0..6 {
            let n = c.usize_("ordering length")?;
            let bytes = c.take(
                n.checked_mul(16).ok_or(Reject::Truncated("ordering"))?,
                "ordering",
            )?;
            orderings.push(
                bytes
                    .chunks_exact(16)
                    .map(|q| {
                        let g = |i: usize| u32::from_le_bytes([q[i], q[i + 1], q[i + 2], q[i + 3]]);
                        (g(0), g(4), g(8), g(12))
                    })
                    .collect(),
            );
            c.align();
        }
        let orderings: [Vec<IndexEntry>; 6] = orderings
            .try_into()
            .map_err(|_| Reject::Truncated("the six orderings"))?;
        IndexRepr::Array(orderings)
    };

    Ok(TripleIndex::from_parts(repr, labels))
}

/// Load a sidecar for `db`, or say why it cannot be used.
pub fn read_for(key: &SidecarKey<'_>, want_compact: bool) -> Result<TripleIndex, Reject> {
    let path = path_for(key.path);
    if !path.exists() {
        return Err(Reject::Missing);
    }
    let buf = std::fs::read(&path).map_err(|e| Reject::Io(e.to_string()))?;
    decode(&buf, key, want_compact)
}
