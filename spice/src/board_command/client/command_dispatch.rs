//! Dispatch admitted board commands and their automatic creation boundaries.

use super::*;

fn validate_explicit_resources(
    resources: &resources::ResourceClaims,
    command: &wire::AdmittedCommand,
) -> Result<(), Outcome<CommandAnswer>> {
    match command.body() {
        wire::intent::Intent::TaskAdd(add) => resources.validate(&add.requirements),
        wire::intent::Intent::TaskEdit(edit) => match &edit.requirements {
            Some(wire::intent::TaskRequirementsEdit::Set { requirements }) => {
                resources.validate(requirements)
            }
            Some(wire::intent::TaskRequirementsEdit::Clear {}) | None => Ok(()),
        },
        _ => Ok(()),
    }
}

pub(crate) fn dispatch_with_resources(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'_>>,
    allocation: &board::NextConfig,
    profiles: OccupantProfiles<'_>,
    resources: Option<&resources::ResourceClaims>,
) -> Option<Outcome<CommandAnswer>> {
    if let Some(outcome) = super::dispatch_preparation(bus, command, repository) {
        return Some(outcome);
    }
    if let Some(outcome) = dispatch_settlement(bus, command, repository.map(|host| host.repository))
    {
        return Some(outcome);
    }
    if let Some(outcome) = dispatch_intake(bus, command, repository) {
        return Some(outcome);
    }
    if let Some(resources) = resources
        && let Err(outcome) = validate_explicit_resources(resources, command)
    {
        return Some(outcome);
    }
    if let Some(outcome) = place::dispatch_placement(bus, command, repository) {
        return Some(outcome);
    }

    let outcome = match command.body() {
        wire::intent::Intent::BoardRead(read) => read_board(
            bus, read, command, allocation, profiles, resources, repository,
        ),
        wire::intent::Intent::TaskDetail(detail) => {
            read_task_detail(bus, repository, detail, command.actor().clone())
        }
        wire::intent::Intent::TaskRelations(relations) => read_task_relations(
            bus,
            repository.map(|host| host.repository),
            relations,
            command.actor().clone(),
        ),
        wire::intent::Intent::TaskTimeline(timeline) => read_task_timeline(
            bus,
            repository.map(|host| host.repository),
            timeline,
            command.actor().clone(),
        ),
        wire::intent::Intent::BoardGraph(graph) => read_graph(
            bus,
            repository.map(|host| host.repository),
            graph,
            command.actor().clone(),
        ),
        wire::intent::Intent::BoardHandout(handout) => read_handout(
            bus,
            repository.map(|host| host.repository),
            handout,
            command.actor().clone(),
        ),
        wire::intent::Intent::DocLedger(ledger) => read_ledger(
            bus,
            repository.map(|host| host.repository),
            ledger,
            command.actor().clone(),
        ),
        wire::intent::Intent::RoomPost(post) => {
            room_post(bus, command, post, repository.map(|host| host.repository))
        }
        wire::intent::Intent::TaskNote(note) => task_note(bus, command, note, repository),
        wire::intent::Intent::TaskEdit(edit) => task_edit(bus, command, edit, repository),
        wire::intent::Intent::TaskWake(wake) => task_wake(bus, command, wake, repository),
        wire::intent::Intent::TaskTarget(target) => task_target(bus, command, target, repository),
        wire::intent::Intent::TaskJoin(join) => {
            task_join(bus, command, join, repository, resources)
        }
        wire::intent::Intent::TaskMedia(media) => task_media(bus, command, media, repository),
        wire::intent::Intent::TaskRef(task) if command.kind() == CommandKind::ClaimRefresh => {
            claim_refresh(bus, command, task, repository)
        }
        wire::intent::Intent::TaskRef(task) if command.kind() == CommandKind::ClaimFree => {
            claim_free(bus, command, task, repository)
        }
        wire::intent::Intent::TaskDepends(depends) => {
            task_depends(bus, command, depends, repository)
        }
        wire::intent::Intent::ClaimWin(win) => claim_win(
            bus, command, win, repository, allocation, profiles, resources,
        ),
        wire::intent::Intent::TaskDone(done) => task_done(bus, command, done, repository),
        wire::intent::Intent::TaskReview(review) => {
            super::routing::automatic(|| task_review(bus, command, review, repository))
        }
        _ => return None,
    };
    Some(outcome)
}
