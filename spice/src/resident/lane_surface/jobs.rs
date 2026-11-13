//! Operator access to keeper records, independent of agent result delivery.

use super::*;
use wire::CommandKind;
use wire::intent::{LaneJobAnswer, LaneJobCatalog, LaneJobRef, LaneJobScope, LaneJobsAnswer};

fn snapshot(
    bus: &Bus,
    epoch: CanonicalUuid,
) -> CommandResult<(journal::JournalHead<wire::ResidencyHead>, wire::Seq)> {
    let (head, cursor) = bus
        .read(|read| Ok((read.residency_head()?, read.cursor()?)))
        .map_err(|error| store_fault("job::residency-unreadable", &error.to_string()))?;
    match (head, cursor) {
        (Some(head), Some(cursor)) if head.body().epoch == epoch => Ok((head, cursor)),
        _ => Err(store_fault(
            "resident::epoch-moved",
            "job control requires the current residency",
        )),
    }
}

pub(in crate::resident) fn catalog(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
) -> Outcome<CommandAnswer> {
    if !command.holds(wire::principal::PrincipalRole::Operator) {
        return authority_refusal(command.actor(), "operator");
    }
    let (head, cursor) = match snapshot(bus, epoch) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return typed_outcome(outcome),
    };
    let mut lanes = Vec::new();
    for (slot, lane) in head.body().lanes.iter().enumerate() {
        let jobs = match terminal::Jobs::for_lane(paths, lane.lane)
            .kept()
            .and_then(|jobs| {
                jobs.into_iter()
                    .map(terminal::Job::listing)
                    .collect::<io::Result<Vec<_>>>()
            }) {
            Ok(jobs) => jobs,
            Err(error) => {
                return environment_fault("job::catalog-unreadable", &error.to_string());
            }
        };
        if !jobs.is_empty() {
            lanes.push(LaneJobCatalog {
                scope: LaneJobScope::new(head.body().topology_revision, slot),
                name: lane.name.clone(),
                worktree: lane.worktree.clone(),
                jobs,
            });
        }
    }
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(cursor),
        body: CommandAnswer::LaneJobs(Box::new(LaneJobsAnswer { lanes })),
    })
}

pub(in crate::resident) fn control(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    target: &LaneJobRef,
) -> Outcome<CommandAnswer> {
    if !command.holds(wire::principal::PrincipalRole::Operator) {
        return authority_refusal(command.actor(), "operator");
    }
    let (head, cursor) = match snapshot(bus, epoch) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return typed_outcome(outcome),
    };
    let Some(slot) = target
        .scope
        .slot(head.body().topology_revision, head.body().lanes.len())
    else {
        return lane_refusal(
            "job::scope-moved",
            Some("one slot at the current topology".to_owned()),
            Some(format!("{}/{}", target.scope.topology, target.scope.lane)),
            "refresh the job catalog and retry with its current reference",
        );
    };
    let jobs = terminal::Jobs::for_lane(paths, head.body().lanes[slot].lane);
    let answer = match inspect(&jobs, target, command.kind() == CommandKind::LaneJobStop) {
        Ok(answer) => answer,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return lane_refusal(
                "job::missing",
                Some("one retained job".to_owned()),
                Some(target.id.clone()),
                "the job is no longer available",
            );
        }
        Err(error) => return environment_fault("job::control-failed", &error.to_string()),
    };
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(cursor),
        body: CommandAnswer::LaneJob(Box::new(answer)),
    })
}

fn inspect(jobs: &terminal::Jobs, target: &LaneJobRef, stop: bool) -> io::Result<LaneJobAnswer> {
    let job = jobs.open(&target.id)?;
    if job.id() != target.id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "job identity differs from its directory",
        ));
    }
    if stop && job.state()?.is_none_or(|state| state.ending.is_none()) {
        job.stop_by_operator()?;
    }
    job.inspect(target.clone())
}

#[cfg(test)]
mod tests;
