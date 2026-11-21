//! Projected task evidence shared by live filing and standalone preview.

use topic::evidence::{Prepared, prepare_fields};

use super::{Actor, Bus, CommandAnswer, Outcome, Repository, read_guard, routing};

mod current;
pub(crate) use current::TopicTree;
mod preview;
pub(crate) use preview::preview;
pub(super) mod seat;

pub(crate) fn prepare(head: &wire::TaskHead) -> Prepared {
    prepare_fields(
        head.born.as_str(),
        [
            std::slice::from_ref(&head.title),
            std::slice::from_ref(&head.pitch),
            head.description.as_slice(),
            &head.term,
        ],
    )
}
