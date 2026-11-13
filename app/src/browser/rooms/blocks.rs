//! A Design room's collapsible blocks as one strip of summaries over one
//! shared panel. Every summary keeps its place in the strip whatever is open,
//! and the one opened block reads in a full-width panel beneath it, so a
//! short block never stands beside a long one's empty height.

use yew::prelude::*;

/// One block in the strip: `summary` is its always-visible line and `body`
/// what the panel shows while it is open. `key` names the block in
/// `data-sheet-block`.
#[derive(Clone, PartialEq)]
pub(in crate::browser) struct RoomBlock {
    pub(in crate::browser) key: &'static str,
    pub(in crate::browser) label: &'static str,
    pub(in crate::browser) summary: Html,
    pub(in crate::browser) body: Html,
}
