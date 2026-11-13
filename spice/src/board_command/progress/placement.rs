//! Placement of a draft by the completion that closes it. Only draft may be
//! unplaced, so this is the one close that places.

use super::super::mutation::activation::{Activation, Destination};
use super::*;

/// The topic a draft close commits, guarded by every observation the
/// phase-zero validator rested on.
pub(in crate::board_command) struct ClosePlacement {
    /// The task as it reads once placed.
    pub(in crate::board_command) entry: Entry,
    activation: Activation,
    destination: Destination,
    /// Whether the close moves the topic, or only validates a placed draft.
    moved: bool,
}

impl ClosePlacement {
    /// Run the phase-zero validator when this completion closes a draft: the
    /// saved exact topic, else the router judging the current statement at
    /// the phase it enters. A row that has left draft needs no placement, nor
    /// does a disposition that settles it. Every validator refusal refuses
    /// the completion before it publishes or moves anything; the draft is
    /// already in its holder's hands, so no submitted mark is written.
    pub(in crate::board_command) fn at_close(
        bus: &Bus,
        snapshot: &ValidatedSnapshot,
        command: &wire::AdmittedCommand,
        repository: Option<Host<'_>>,
        entry: &Entry,
        next: &wire::WorkflowHead,
        disposed: bool,
    ) -> Result<Option<Self>, Outcome<CommandAnswer>> {
        if !entry.is_draft() || (disposed && !next.status.is_pending()) {
            return Ok(None);
        }
        let destination = Destination::resolve(bus, snapshot, entry.statement(), &next.phase)?;
        let activation = mutation::activation::validate(
            snapshot,
            command,
            repository,
            entry.statement().clone(),
            entry.workflow(),
            &destination,
            &[],
        )
        .map_err(mutation::activation::AdvanceRefusal::into_outcome)?;
        let placed = entry
            .clone()
            .placed_in_topic(activation.statement.topic.clone());
        Ok(Some(Self {
            entry: placed,
            activation,
            destination,
            moved: entry.topic().is_none(),
        }))
    }

    /// The task as this close leaves it: placed when `placement` is present.
    pub(in crate::board_command) fn entry_or<'a>(
        placement: Option<&'a Self>,
        entry: &'a Entry,
    ) -> &'a Entry {
        placement.map_or(entry, |placement| &placement.entry)
    }

    pub(in crate::board_command) fn guard(&self, write: &mut JournalWrite) {
        self.activation.guard(write);
        self.destination.guard(write);
    }

    /// Guard the validator's observations and, when the close moved the
    /// topic, append the placed statement as a consequence of the completion
    /// fact.
    pub(in crate::board_command) fn append(
        &self,
        write: &mut JournalWrite,
        completed: PendingFact,
    ) -> Result<PendingFact, Outcome<CommandAnswer>> {
        self.guard(write);
        if !self.moved {
            return Ok(completed);
        }
        let statement = self.entry.statement().clone();
        let revision = u32::try_from(statement.revision.get())
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        let edited = write
            .fact(
                body::TaskEdited {
                    intent: None,
                    task: statement.born.clone(),
                    revision,
                    dependencies: None,
                    flow: None,
                },
                FactCause::Pending(completed),
            )
            .map_err(|error| store_fault("task::placement-build", error.to_string()))?;
        write.task_head(edited, move |position| TaskHead {
            revision: position,
            ..statement
        });
        Ok(edited)
    }
}
