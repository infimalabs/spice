//! A Design room's typed contributions as their facts state them; the room
//! history reads them at their journal positions beside ordinary conversation.
//! Reading them moves no task, claim, candidate, decision, or authorization.

use wire::rooms::{RoomAuthorityStamp, RoomContributionKind, RoomSource, StatementItem};
use wire::{CanonicalUuid, Seq, TaskStamp};

use super::{RoomEventKind, RoomProjection};

/// One recorded contribution as its fact states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contribution<'a> {
    /// Stable identity of the contribution.
    pub publication: &'a RoomAuthorityStamp,
    /// Journal position that recorded it.
    pub position: Seq,
    /// Logical lane that authored it.
    pub lane: CanonicalUuid,
    /// What it offers: a charge or a question.
    pub contribution: RoomContributionKind,
    /// The Design question item it weighs, when it names one.
    pub question: Option<&'a StatementItem>,
    /// Its complete text.
    pub text: &'a str,
    /// Its cited sources, in the author's order.
    pub sources: &'a [RoomSource],
}

impl RoomProjection {
    /// `room`'s contributions, oldest first.
    #[must_use]
    pub fn contributions(&self, room: &TaskStamp) -> Vec<Contribution<'_>> {
        let mut contributions = self
            .rooms
            .get(room)
            .into_iter()
            .flat_map(|state| state.events.iter())
            .filter_map(|(publication, event)| match &event.kind {
                RoomEventKind::Contributed {
                    lane,
                    contribution,
                    question,
                    text,
                    sources,
                } => Some(Contribution {
                    publication,
                    position: event.position,
                    lane: *lane,
                    contribution: *contribution,
                    question: question.as_ref(),
                    text,
                    sources,
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        contributions.sort_by_key(|contribution| contribution.position);
        contributions
    }
}
