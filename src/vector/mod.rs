//! Vector attributes and nearest-neighbour search.
//!
//! A vector attribute is per-node side data stored **outside** the
//! `.gdb`, one sidecar file per attribute (`<db>.vec.<attr>`), built
//! offline and read-only at query time. See `sidecar` for the format and
//! the reasoning behind keeping it out of the pager.
//!
//! The layer is deliberately unaware of GQL: it knows node ids, vectors,
//! distances, and how to enumerate neighbours incrementally. Which
//! neighbours a query actually wants, and how that interleaves with
//! pattern matching, is the job of `runtime::vsearch`.

pub mod cursor;
pub mod hnsw;
pub mod metric;
pub mod sidecar;
pub mod store;

pub use cursor::{BruteForceCursor, EmptyCursor, NnCursor, NnStream};
pub use hnsw::{Hnsw, HnswCursor, HnswParams};
pub use metric::Metric;
pub use sidecar::Sidecar;
pub use store::{VectorSet, VectorStore};
