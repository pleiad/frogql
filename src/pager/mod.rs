pub mod header;
pub mod page;
// The `pager/` directory groups Pager (in pager.rs) with its
// supporting header and page modules; reaching it as
// pager::pager::Pager is intentional.
#[allow(clippy::module_inception)]
pub mod pager;

pub use header::FileHeader;
pub use page::{Page, PageType, PAGE_SIZE};
pub use pager::Pager;
