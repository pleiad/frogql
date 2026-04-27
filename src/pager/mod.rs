pub mod header;
pub mod page;
// `pager::pager::Pager` — flat name preserved; the inner `pager` module
// holds Pager itself while this `pager/` directory hosts header + page.
#[allow(clippy::module_inception)]
pub mod pager;

pub use header::FileHeader;
pub use page::{Page, PageType, PAGE_SIZE};
pub use pager::Pager;
