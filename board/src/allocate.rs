//! The one writer that mints a claim.
//!
//! Every claim the port commits in production is minted here. The resident's
//! claim command assembles the transaction around this writer -- selection,
//! residency admission, the change guard that re-reads the head set inside the
//! write, and the command receipt all live with the command that needs them --
//! and calls in to record the claim itself. Keeping the lifecycle write in one
//! place is what stops a field added to a claim from reaching one caller and
//! not another.

use std::error::Error;
use std::fmt;

use journal::{BuildError, FactCause, JournalWrite, PendingFact};
use wire::{
    Actor, CanonicalUuid, ClaimCell, ClaimDisposition, ClaimHead, Instant, LeaseHead, TaskStamp,
    WorkflowHead, body,
};

/// Everything one claim writes, apart from the transaction it writes into.
///
/// A caller fills this in and the writer below turns it into the claim's fact
/// and its two heads. Callers differ in how they choose a task and what they
/// guard the write with; they do not differ in what a claim is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mint<'a> {
    /// Task the claim takes possession of.
    pub task: &'a TaskStamp,
    /// Principal the claim is minted for.
    pub actor: &'a Actor,
    /// Workflow head as it stands, read for the review assignment and rewritten
    /// with it when the phase calls for one.
    pub workflow: &'a WorkflowHead,
    /// Topic the task lives in at claim time; absent while it is unplaced.
    pub topic: Option<&'a wire::Topic>,
    /// Lane whose live occupant holds the claim.
    pub lane: CanonicalUuid,
    /// Occupant authenticated for that lane.
    pub occupant: CanonicalUuid,
    /// Resident generation minting the claim.
    pub epoch: CanonicalUuid,
    /// Lease start, which is also its first renewal.
    pub at: Instant,
    /// Initial lease expiry.
    pub expiry: Instant,
    /// Maximum instant through which the lease may be renewed.
    pub maximum: Instant,
    /// Worktree boundary recorded with the claim, for callers that have one.
    pub git: Option<body::GitBoundary>,
    /// Dirty-handoff transaction authorized by this claim, when any.
    pub recovery: Option<String>,
    /// Accounted transcript the won phase opens at.
    ///
    /// Read by the caller rather than here, because this writer is handed an
    /// already-open transaction and never holds a journal read of its own.
    pub source: body::SourceBoundary,
    /// Fact of the same transaction this claim descends from, when the claim
    /// is a consequence of it rather than a chain root of its own; a review
    /// assignment written ahead of the claim takes its place.
    pub cause: Option<PendingFact>,
}

/// Which of a claim's facts failed to build.
///
/// The two are kept apart because a caller reporting the failure names the fact
/// it could not write, and a review assignment and a claim are not the same
/// report.
#[derive(Debug)]
pub enum MintError {
    /// The review assignment recorded ahead of the claim.
    Assignment(BuildError),
    /// The claim itself.
    Claim(BuildError),
}

impl MintError {
    /// The build failure underneath, whichever fact raised it.
    #[must_use]
    pub fn into_build(self) -> BuildError {
        match self {
            Self::Assignment(error) | Self::Claim(error) => error,
        }
    }
}

impl fmt::Display for MintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Assignment(error) => write!(formatter, "review assignment: {error}"),
            Self::Claim(error) => write!(formatter, "claim: {error}"),
        }
    }
}

impl Error for MintError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Assignment(error) | Self::Claim(error) => Some(error),
        }
    }
}

/// Record the review assignment a claim on a review phase carries ahead of it.
///
/// The claim then hangs off the assignment rather than off the root, so the two
/// commit together or not at all. Every seated reviewer gets its own
/// assignment; none moves the workflow, so a further reviewer joins a round
/// without staling the reviewers already seated.
fn assignment_cause(write: &mut JournalWrite, request: &Mint<'_>) -> Result<FactCause, MintError> {
    if request.workflow.phase.as_str() != "review" {
        return Ok(request.cause.map_or(FactCause::Root, FactCause::Pending));
    }
    let assigned = write
        .fact(
            body::TaskAssigned {
                task: request.task.clone(),
                reviewer: request.actor.clone(),
                phase: request.workflow.phase,
            },
            FactCause::Root,
        )
        .map_err(MintError::Assignment)?;
    Ok(FactCause::Pending(assigned))
}

/// Write one claim's fact and heads into an open transaction.
///
/// This is the only place a claim is minted, so a claim written by any caller
/// carries the same fact, the same pair of heads, and the same review
/// assignment ahead of it.
pub fn mint(write: &mut JournalWrite, request: Mint<'_>) -> Result<PendingFact, MintError> {
    let cause = assignment_cause(write, &request)?;
    let task = request.task.clone();
    let actor = request.actor.clone();
    let cell = ClaimCell {
        topic: request.topic.cloned(),
        phase: request.workflow.phase,
    };
    let Mint {
        lane,
        occupant,
        epoch,
        at,
        expiry,
        maximum,
        git,
        recovery,
        source,
        ..
    } = request;
    let won = write
        .fact(
            body::ClaimWon {
                task: task.clone(),
                actor: actor.clone(),
                lane,
                occupant,
                epoch,
                cell: cell.clone(),
                until: expiry,
                git,
                recovery: recovery.clone(),
                source,
            },
            cause,
        )
        .map_err(MintError::Claim)?;
    write.claim_head(task.clone(), won, move |fact| ClaimHead {
        actor,
        fact,
        claimed: fact,
        cell,
        lane,
        occupant,
        epoch,
        recovery,
        disposition: ClaimDisposition::Active,
        // The commit settles a won claim to the generation its transaction
        // leaves the task at, which a review assignment above advances.
        acknowledged: 0,
    });
    write.lease_head(task, won, move |claimed| LeaseHead {
        claimed,
        start: at,
        renewal: at,
        expiry,
        maximum,
        epoch,
    });
    Ok(won)
}
