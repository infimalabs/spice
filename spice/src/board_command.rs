//! Board and task commands over the sole resident door.

use board::{Entry, View};
use bus::{Bus, CommitOutcome};
use clap::ArgMatches;
use journal::{FactCause, GuardError, JournalGuard, JournalRead, JournalWrite, PendingFact};
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::body::GitBoundary;
use wire::intent::{
    BoardAnswer, BoardClaim, BoardGraph, BoardHandout, BoardRead, BoardTask, ClaimWin,
    CommandAnswer, DocLedger, NextWorkCounts, NextWorkState, NextWorkTask, Positioned,
    ReviewFollowup, TASK_HISTORY_PAGE_LIMIT_MAX, TASK_HISTORY_QUERY_BYTES_MAX, TaskAdd, TaskClose,
    TaskConclude, TaskDefer, TaskDelete, TaskDepends, TaskDone, TaskEdit, TaskFile,
    TaskHistoryCursor, TaskHistoryPage, TaskJoin, TaskNote, TaskOopsAdd, TaskRef, TaskReview,
    TaskTarget, TaskWake, TextAnswer,
};
use wire::rooms::{
    RoomListenerAvailability, RoomListenerDelivery, RoomPost, RoomPostAnswer, RoomPostListener,
    RoomPostNext, RoomPostOutcome, RoomPublicationContent, RoomPublished,
};
use wire::{
    Actor, ClaimDisposition, ClaimHead, Command, CommandKind, Digest, Domain, Instant, LeaseHead,
    Name, Outcome, Producer, Refusal, Reply, ReplyPosition, Seq, SeqRange, TaskHandle, TaskHead,
    TaskStamp, WireVersion, Witness, WorkflowHead, body,
};

use crate::cli::Contract;
use crate::command_client::Host;

fn claim_capture_from_board(board: &BoardAnswer) -> gate::ClaimCapture {
    let mut active = board
        .tasks
        .iter()
        .filter(|task| task.held_by(&board.principal).is_some());
    let Some(task) = active.next() else {
        return gate::ClaimCapture::Idle;
    };
    if active.next().is_some() {
        return gate::ClaimCapture::Absent {
            reason: "board claim read found more than one active claim for this principal"
                .to_owned(),
        };
    }
    gate::ClaimCapture::Active {
        task: task.statement.body.handle.clone(),
        phase: task.workflow.body.phase,
    }
}

fn require_no_planning_implementation(
    board: &BoardAnswer,
    action: &str,
) -> Result<(), Outcome<CommandAnswer>> {
    match claim_capture_from_board(board) {
        gate::ClaimCapture::Active { task, phase }
            if gate::lacks_commit_authority(phase.as_str()) =>
        {
            Err(refusal(
                "task::planning-implementation",
                Some(format!(
                    "active task={task}; phase={phase}; refused action={action}"
                )),
                concat!(
                    "this phase holds no commit authority: record planning findings with task ",
                    "add, task modify, task depends, task note, or task artifact add; ",
                    "only a claimed implementation phase changes repository history",
                ),
            ))
        }
        gate::ClaimCapture::Absent { reason } => {
            Err(protocol_fault("claim::active-ambiguous", &reason))
        }
        _ => Ok(()),
    }
}

pub(crate) use spice::claim_lease::{AUTOMATIC_MAXIMUM_SECONDS, AUTOMATIC_PROVISIONAL_SECONDS};
pub(crate) const AUTOMATIC_HEARTBEAT_SECONDS: i64 = 20;
pub(crate) const AUTOMATIC_ROLLING_SECONDS: i64 = 100;

/// Present each authored statement item with the unique handle derived from
/// the complete current list. Durable statements keep the authored text alone.
pub(crate) fn statement_items_with_handles(items: &[String]) -> Vec<String> {
    let identities = wire::StatementItem::each(items).collect::<Vec<_>>();
    identities
        .iter()
        .zip(items)
        .map(|(item, text)| format!("{} {text}", item.handle(&identities)))
        .collect()
}

#[cfg(test)]
pub(crate) use read_guard::next_work::read_next_work;
pub(crate) use read_guard::next_work::{OccupantMatch, OccupantProfiles, RetainedMatch};
const CAPTURE_WORK_RULE: &str = "capturework";
const RECOVER_STEAL_RULE: &str = "recoversteal";
fn claim_authority_refusal(reason: &str, observed: Option<String>) -> Outcome<CommandAnswer> {
    let command = wire::SpiceCommand::new("task", ["status"])
        .expect("task status is a structural Spice recovery command");
    Outcome::Refusal(Refusal::repairable(
        command,
        wire::RefusalDiagnostic::new(reason, None, None, observed).with_detail(
            "run spice task status to read current authority; when it reports no claim for this lane, run spice task next",
        ),
    ))
}

// Ordinary `mod` children, never `include!`: a child module already reads its
// ancestors' private items, so the guard tests below call `claim_plan` and
// `commit_claim` unchanged, and the formatters gate covers each file by
// construction rather than by an enumeration kept by hand.
mod term;
use term::*;
pub(crate) use term::{claim_contracts, completion_contract, substantive_term};
mod research;
pub(crate) use research::{design_coverage, design_research};
mod client;
pub(super) use client::*;
mod client_sweep;
use client_sweep::run_sweep;
mod dependency_witness;
use dependency_witness::*;
mod design_return;
pub(crate) use design_return::{ReturnOwed, owed_returns, record_return};
mod followup;
use followup::*;
mod handoff;
pub(crate) use handoff::{phase_handoff, topic_origin_at};
mod peer_handoff;
pub(crate) use peer_handoff::independent_handoff_awaits_peer;
mod next_action;
use next_action::*;
mod automatic_lifecycle;
pub(crate) use automatic_lifecycle::{
    AutomaticClaim, AutomaticSeat, claim_automatic, is_automatic_claim, release_automatic,
};
pub(crate) mod drive;
mod mutation;
mod placement;
pub(crate) mod resources;
mod routing;
pub(crate) use mutation::task_defer;
pub(crate) use mutation::{accept_invitation, task_invite, task_join};

/// What a join decides against beyond the journal: the resident repository
/// its claim prepares Git in and the one resource ledger it reserves from.
#[derive(Clone, Copy)]
pub(crate) struct Joining<'a> {
    pub(crate) host: Host<'a>,
    pub(crate) resources: &'a resources::ResourceClaims,
}

/// One resident's owned join context, for tests that drive join paths.
#[cfg(test)]
pub(crate) struct JoiningFixture {
    repository: spice::repository::Repository,
    ambient: spice::ambient::Ambient,
    resources: resources::ResourceClaims,
}

#[cfg(test)]
impl JoiningFixture {
    pub(crate) fn discover(root: &std::path::Path) -> Self {
        let repository =
            spice::repository::Repository::discover(root).expect("fixture repository resolves");
        let config = sill::LockConfig::new(75, 76, 77, std::iter::empty())
            .expect("empty lock configuration");
        let resources = resources::ResourceClaims::new(sill::LockTopology::new(
            repository.worktree_root(),
            &config,
        ));
        Self {
            repository,
            ambient: spice::ambient::Ambient::default(),
            resources,
        }
    }

    pub(crate) fn joining(&self) -> Joining<'_> {
        Joining {
            host: Host {
                repository: &self.repository,
                ambient: &self.ambient,
            },
            resources: &self.resources,
        }
    }
}
use mutation::*;
mod planning;
use planning::*;
mod preparation;
use preparation::*;
mod progress;
use progress::*;
pub(crate) mod ownership;
mod read_guard;
use read_guard::*;
mod room;
use room::room_post;
pub(crate) use room::{
    derive_attention, project_attention, read_room_duty_rows, read_room_publications, room_duties,
};
mod room_leave;
pub(crate) use room_leave::room_leave;
mod room_contribute;
pub(crate) use room_contribute::room_contribute;
mod deactivation;
use deactivation::Deactivation;
mod steering_origin;
use steering_origin::*;
mod task_step;
use task_step::*;
mod task_text;
pub(crate) use task_text::{
    TaskText, collapsed_description, collapsed_term, current_task, replace_term, replace_text,
    term_text,
};
#[cfg(test)]
use task_text::{term_edit, term_items, text_edit};
mod taste_review;
use taste_review::*;

pub(crate) fn parse_origin(
    matches: &ArgMatches,
) -> Result<Option<wire::Origin>, Outcome<CommandAnswer>> {
    matches
        .get_one::<String>("origin")
        .map(|written| {
            written.parse::<wire::Origin>().map_err(|error| {
                refusal(
                    "task::origin-invalid",
                    Some(written.clone()),
                    error.to_string(),
                )
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{
    moved_uri_join_outcomes, register_policy_repository, unregister_policy_repository,
};
