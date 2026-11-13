//! The resident's reading of typed Design contributions. A contribution's
//! text and question item identity arrive with its fact, but whether each cited
//! transcript, journal fact, or retained artifact still yields its bytes, and
//! the text of the item it weighs, only the resident can say. A selected Design
//! reads each such contribution once; the connection's bounded pump takes
//! owed reads as mailbox capacity allows.

use wire::intent::{RoomContributionReadAnswer, RoomFactSummary};
use wire::rooms::{RoomAnchor, RoomAuthorityStamp, RoomSourceView};
use wire::{Outcome, Response};

use super::{AppRuntime, Contribution};
use crate::socket::Admitted;

/// One contribution's sources and question item as the resident resolved them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContributionRead {
    /// A read is in flight.
    Loading,
    /// The resident's answer.
    Read {
        /// Each cited source, in the author's order, as a link it can still
        /// follow or the typed reason it cannot.
        sources: Vec<RoomSourceView>,
        /// What each available cited journal fact is, in citation order.
        facts: Vec<RoomFactSummary>,
        /// The weighed question item's text, absent once the statement the
        /// contribution witnessed has been revised.
        question: Option<String>,
    },
    /// The resident refused or failed the read, with its stable reason.
    Failed(String),
}

impl AppRuntime {
    /// The resolved sources of one contribution, absent until a read starts.
    #[must_use]
    pub fn contribution_read(&self, publication: &RoomAuthorityStamp) -> Option<&ContributionRead> {
        self.contribution_reads.get(publication)
    }

    /// A contribution owes a read when it cites any source or names a
    /// question item and has no read.
    fn contribution_owes_read(&self, contribution: &Contribution<'_>) -> bool {
        (!contribution.sources.is_empty() || contribution.question.is_some())
            && !self
                .contribution_reads
                .contains_key(contribution.publication)
    }

    /// Take up to `limit` owed reads, oldest contribution first, each now
    /// marked loading. A contribution owes one when a task-detail surface
    /// shows its Design, it cites any source or names a question item, and it
    /// has no read.
    pub fn take_contribution_reads(
        &mut self,
        limit: usize,
    ) -> Vec<(RoomAnchor, RoomAuthorityStamp)> {
        let mut owed = Vec::new();
        for room in self.task_details.shown() {
            for contribution in self.rooms.contributions(room) {
                if owed.len() == limit {
                    break;
                }
                if self.contribution_owes_read(&contribution) {
                    owed.push((room.clone(), contribution.publication.clone()));
                }
            }
        }
        for (_, publication) in &owed {
            self.contribution_reads
                .insert(publication.clone(), ContributionRead::Loading);
        }
        owed
    }

    /// Owe a taken read again because the connection could not send it.
    pub fn restore_contribution_read(&mut self, publication: &RoomAuthorityStamp) {
        self.contribution_reads.remove(publication);
    }

    /// Apply one correlated sources read without crossing capability scope
    /// or contribution identity; only a read still in flight takes it.
    pub fn apply_contribution_read(
        &mut self,
        publication: &RoomAuthorityStamp,
        admitted: &Admitted<Response<RoomContributionReadAnswer>>,
    ) {
        if admitted.scope() != self.accepted.scope()
            || self.contribution_reads.get(publication) != Some(&ContributionRead::Loading)
        {
            return;
        }
        let sources = match &admitted.get().outcome {
            Outcome::Reply(reply) if reply.body.publication == *publication => {
                ContributionRead::Read {
                    sources: reply.body.sources.clone(),
                    facts: reply.body.facts.clone(),
                    question: reply.body.question.clone(),
                }
            }
            Outcome::Reply(_) => {
                ContributionRead::Failed("room::contribution-read-identity-mismatch".to_owned())
            }
            Outcome::Refusal(refusal) => ContributionRead::Failed(
                refusal
                    .diagnostic()
                    .map_or(
                        "room::contribution-read-refused",
                        wire::RefusalDiagnostic::reason,
                    )
                    .to_owned(),
            ),
            Outcome::Fault(fault) => ContributionRead::Failed(fault.reason.clone()),
        };
        self.contribution_reads.insert(publication.clone(), sources);
    }

    /// A reconnect re-reads every source: a read in flight never answers, and
    /// a settled one may have lost its owning bytes since.
    pub(super) fn forget_contribution_reads(&mut self) {
        self.contribution_reads.clear();
    }
}
