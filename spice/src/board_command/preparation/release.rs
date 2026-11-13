//! Controlled Draft release uses the same phase-zero validator as author activation.

use super::*;
use crate::board_command::mutation::activation::{self, Activation, Destination, Sibling};

pub(super) struct Release {
    entries: Vec<(Entry, Activation, Destination)>,
}

impl Release {
    pub(super) fn append(self, write: &mut JournalWrite) -> Result<(), Outcome<CommandAnswer>> {
        for (entry, activation, destination) in self.entries {
            activation.guard(write);
            destination.guard(write);
            let cause = if activation.statement != *entry.statement() {
                FactCause::Pending(crate::board_command::mutation::append_activation_statement(
                    write,
                    &entry,
                    activation.statement,
                )?)
            } else {
                FactCause::Root
            };
            activation::append_advance(write, entry.born(), activation.workflow, cause)?;
        }
        Ok(())
    }
}

/// The controller discharges only its own gate; an unfinished immediate parent
/// still owns construction. Already activated parents prove their ancestor gate
/// was discharged, without erasing that ancestry.
pub(super) fn selected(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'_>>,
    outputs: &[TaskStamp],
    controller: &TaskStamp,
    write: &mut JournalWrite,
) -> Result<Release, Outcome<CommandAnswer>> {
    let snapshot = validated_working_snapshot_for_tasks(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        outputs,
    )?;
    guard_task_projection(write, snapshot.answer.task_barrier.clone());
    let entries = snapshot
        .entries
        .iter()
        .filter(|entry| {
            outputs.contains(entry.born())
                && entry.is_draft()
                && entry
                    .statement()
                    .preparation
                    .as_ref()
                    .is_some_and(|preparation| {
                        preparation.parent == *controller
                            || snapshot.entries.iter().any(|parent| {
                                parent.born() == &preparation.parent
                                    && parent.workflow().concluded_positively()
                            })
                    })
        })
        .collect::<Vec<_>>();
    prepare(bus, command, repository, &snapshot, &entries)
}

/// A positively completed Plan witnesses and releases its current Draft outputs.
/// Retained ancestor approval is lineage, not a second gate on the Plan's work.
pub(in crate::board_command) fn completed_plan(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'_>>,
    snapshot: &ValidatedSnapshot,
    parent: &Entry,
    next: &WorkflowHead,
    write: &mut JournalWrite,
) -> Result<(), Outcome<CommandAnswer>> {
    if !planning::declares_plan(parent.statement().intent, &parent.workflow().flow)
        || !next.concluded_positively()
    {
        return Ok(());
    }
    guard_task_projection(write, snapshot.answer.task_barrier.clone());
    let entries = snapshot
        .entries
        .iter()
        .filter(|entry| {
            entry.is_draft()
                && entry.workflow().status.is_pending()
                && entry
                    .statement()
                    .preparation
                    .as_ref()
                    .is_some_and(|preparation| preparation.parent == *parent.born())
        })
        .collect::<Vec<_>>();
    prepare(bus, command, repository, snapshot, &entries)?.append(write)
}

fn prepare(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'_>>,
    snapshot: &ValidatedSnapshot,
    entries: &[&Entry],
) -> Result<Release, Outcome<CommandAnswer>> {
    let siblings = entries
        .iter()
        .map(|entry| Sibling {
            born: entry.born().clone(),
            lends: term::substantive_term(&entry.statement().term),
            dependencies: entry.statement().dependencies.clone(),
        })
        .collect::<Vec<_>>();
    let mut prepared = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.active_claims().next().is_some() {
            return Err(refusal(
                "preparation::output-claimed",
                Some(entry.handle().to_string()),
                "release only unclaimed controlled drafts",
            ));
        }
        let phase = entry.workflow().flow.get(1).ok_or_else(|| {
            refusal(
                "task::flow-invalid",
                Some(entry.handle().to_string()),
                "declare a substantive phase after draft",
            )
        })?;
        let destination = Destination::resolve(bus, snapshot, entry.statement(), phase)?;
        let activation = activation::validate(
            snapshot,
            command,
            repository,
            entry.statement().clone(),
            entry.workflow(),
            &destination,
            &siblings,
        )
        .map_err(activation::AdvanceRefusal::into_outcome)?;
        prepared.push(((*entry).clone(), activation, destination));
    }
    Ok(Release { entries: prepared })
}
