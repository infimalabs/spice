//! Claim intents and the answers that move task possession.

use serde::{Deserialize, Serialize};

use super::TaskOwnership;
use crate::envelope::Actor;
use crate::ident::CanonicalUuid;
use crate::witness::{Name, Phase, TaskHandle};

/// The answer a claim command returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimAnswer {
    /// Resolved ownership when this receipt was sealed.
    #[serde(deserialize_with = "crate::optional::required")]
    pub ownership: Option<TaskOwnership>,
    /// Task whose possession moved.
    pub task: TaskHandle,
    /// The won task's statement on fresh assignment, whole, because its
    /// holder works from it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub statement: Option<Box<ClaimStatement>>,
    /// Actor holding it after the transaction, absent when possession was released.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub holder: Option<Actor>,
    /// Task generation accepted by `task next`, so the holder knows completion
    /// is admitted against the current statement.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub acknowledged: Option<u64>,
    /// The lease sits at the indefinite approval horizon and stayed there
    /// while `task next` accepted the generation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// Non-blocking overlap warnings discovered from the claim-time board.
    #[serde(default, skip_serializing_if = "claim_warnings_empty")]
    pub warnings: Box<[ClaimWarning]>,
    /// A steal found no live holder: it claimed the task with nobody
    /// displaced, as a capture would have.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub nothing_stolen: bool,
    /// Executable creation-context command or stable unavailable reason on fresh assignment.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub creation_context: Option<Box<str>>,
    /// Originating steering directive rendered for fresh task hydration.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub origin_context: Option<Box<str>>,
    /// The task's room conversation, newest first under the interactive read
    /// bounds, with the `task show` command that pages the rest; absent when
    /// the room holds no publication.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub room_context: Option<Box<str>>,
    /// Exact CLI command admitted as the next action, when one is available.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub next: Option<Box<str>>,
    /// Phase the claim landed in, with the one deliverable it authorizes.
    ///
    /// A lane reads this where it wins the work, because the phase decides
    /// whether the next action touches board state or repository files.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub phase: Option<ClaimPhase>,
    /// What claiming did with commits a previous holder left parked on the task.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub transplant: Option<ClaimTransplant>,
    /// Commits and dirty work a release parked on the task, with the command
    /// that restores them onto a lane.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub parked: Option<ClaimParked>,
    /// Why a steal could not copy the displaced holder's work onto this lane.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub uncarried: Option<Box<str>>,
}

fn claim_warnings_empty(warnings: &[ClaimWarning]) -> bool {
    warnings.is_empty()
}

/// Work a release parked on its task and the claim that restores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimParked {
    /// Parked commits, their baseline, and the dirty-worktree snapshot.
    pub commits: crate::body::TaskParkedCommits,
    /// Exact CLI command whose claim replays the parked work onto the lane.
    pub restore: Box<str>,
}

/// A won task's statement as its holder works from it. Description text stays
/// whole; each term row is prefixed by its current statement-item handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimStatement {
    /// Authored title.
    pub title: String,
    /// One plain sentence saying who can do what once the task lands and why.
    pub pitch: String,
    /// Longer authored context.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub description: Option<String>,
    /// Criteria that decide whether the work is done, each prefixed by its
    /// current statement-item handle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub term: Vec<String>,
    /// Current own, linked, or unspecified acceptance contract.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub completion_contract: Option<Box<str>>,
    /// Current Design research obligation, independent of the completion contract.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub design_research: Option<Box<str>>,
}

/// The phase one claim answer was assigned in, and what it authorizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimPhase {
    /// Phase as the workflow spells it.
    pub name: Phase,
    /// The one deliverable this phase authorizes, when it names one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub deliverable: Option<Box<str>>,
    /// Number of distinct arrivals required in this phase.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub target: u32,
    /// Participants whose claims are active before this assignment lands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partners: Vec<ClaimParticipant>,
    /// Participants that already arrived in the current round.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arrivals: Vec<ClaimParticipant>,
}

fn one() -> u32 {
    1
}

fn is_one(value: &u32) -> bool {
    *value == 1
}

/// One active or arrived participant presented with a shared assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimParticipant {
    /// Participant actor.
    pub actor: Actor,
    /// Lane carrying the participation.
    pub lane: CanonicalUuid,
    /// Current proved peer address, when the lane is seated.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub address: Option<crate::OccupantAddress>,
}

/// Parked commits replayed onto the lane HEAD a claim fast-forwarded to.
///
/// Every parked commit lands in exactly one list, except the one the replay
/// stopped on, which `conflict` names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimTransplant {
    /// Lane HEAD the parked commits were replayed onto.
    pub onto: String,
    /// Commits replayed cleanly, oldest first, with their new identities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied: Vec<TransplantedCommit>,
    /// Commits whose changes the lane already held, so nothing was replayed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub present: Vec<String>,
    /// Commits Git no longer holds, which could not be replayed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// The replay Git stopped on, left in the worktree for the claimant.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub conflict: Option<TransplantConflict>,
    /// Worktree snapshot restored uncommitted, or waiting behind a paused replay.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub snapshot: Option<String>,
}

/// One parked commit and the commit its replay created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransplantedCommit {
    /// Parked commit identity.
    pub parked: String,
    /// Commit created on the claimant's lane.
    pub commit: String,
}

/// A replay paused in the worktree as a Git cherry-pick with conflicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransplantConflict {
    /// Parked commit whose cherry-pick stopped.
    pub commit: String,
    /// Paths Git left conflicted.
    pub paths: Vec<String>,
    /// Later parked commits queued behind it in Git's cherry-pick sequence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remaining: Vec<String>,
    /// How to finish the replay.
    pub remedy: Box<str>,
}

/// One active claim whose acceptance names the same implementation surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimWarning {
    /// Other in-flight task.
    pub task: TaskHandle,
    /// Lane holding the other claim.
    pub lane: CanonicalUuid,
    /// Rare exact identifiers both statements name, then their strongest
    /// shared weighted terms.
    pub surface: Vec<String>,
}

/// Run the named eligibility rule and atomically win its claim.
///
/// Ordinary allocation rules select from board state. Explicit recovery binds
/// the exact task through its sealed transaction and matching witnesses, so the
/// recovery identity is the sole additional selector carried by this intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimWin {
    /// Named eligibility rule the query runs.
    pub rule: Name,
    /// Exact dirty-handoff transaction, present only for explicit recovery.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub recovery: Option<String>,
}
