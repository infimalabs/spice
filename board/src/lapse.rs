//! Journal-backed expiry of task possession.

use std::error::Error;
use std::fmt;

use journal::{
    BuildError, CommitError, FactCause, GuardError, Journal, JournalGuard, JournalHead,
    JournalWrite, ReadError,
};
use wire::{
    CanonicalUuid, ClaimDisposition, Command, CommandKind, Instant, Producer, Seq, TaskHandle,
    WireVersion, Witness, body, intent,
};

use crate::Entry;
use crate::round::{Closure, Retraction, RoundError};

const CHANGED: &str = "lapse inputs changed before expiry could commit";

/// Durable timer inputs for one claim expiry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request {
    /// Identity stored with the canonical command receipt.
    pub command: CanonicalUuid,
    /// Occupant whose lease timer fired.
    pub occupant: CanonicalUuid,
    /// Resident generation under which the timer was armed.
    pub epoch: CanonicalUuid,
    /// Exact durable expiry carried by the fired timer.
    pub expiry: Instant,
    /// Current instant at which expiry is evaluated.
    pub at: Instant,
    /// Exact task named by the lapse command.
    pub intent: intent::TaskRef,
}

/// The lapsed task and its one-fact committed range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lapsed {
    task: TaskHandle,
    first: Seq,
    last: Seq,
}

impl Lapsed {
    /// Task made eligible for allocation again.
    #[must_use]
    pub const fn task(&self) -> &TaskHandle {
        &self.task
    }

    /// First fact position assigned to the lapse transaction.
    #[must_use]
    pub const fn first(&self) -> Seq {
        self.first
    }

    /// Last fact position assigned to the lapse transaction.
    #[must_use]
    pub const fn last(&self) -> Seq {
        self.last
    }
}

/// Expected reasons a timer cannot lapse possession.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    MissingTask(TaskHandle),
    MissingClaim,
    MissingLease,
    Inactive(ClaimDisposition),
    ClaimLeaseEpoch {
        claim: CanonicalUuid,
        lease: CanonicalUuid,
    },
    ClaimEpoch {
        holding: CanonicalUuid,
        named: CanonicalUuid,
    },
    ClaimOccupant {
        holding: CanonicalUuid,
        named: CanonicalUuid,
    },
    IncoherentLease,
    StaleExpiry {
        stored: Instant,
        fired: Instant,
    },
    Early {
        at: Instant,
        expiry: Instant,
    },
    MissingResidency,
    ResidentState(wire::ResidentState),
    Changed(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTask(task) => write!(formatter, "task {task} has no current heads"),
            Self::MissingClaim => formatter.write_str("lapse requires a claim head"),
            Self::MissingLease => formatter.write_str("lapse requires a lease head"),
            Self::Inactive(disposition) => {
                write!(formatter, "claim is not active: {disposition:?}")
            }
            Self::ClaimLeaseEpoch { claim, lease } => {
                write!(
                    formatter,
                    "claim epoch {claim} does not match lease epoch {lease}"
                )
            }
            Self::ClaimEpoch { holding, named } => {
                write!(formatter, "claim epoch is {holding}, not {named}")
            }
            Self::ClaimOccupant { holding, named } => {
                write!(formatter, "claim occupant is {holding}, not {named}")
            }
            Self::IncoherentLease => {
                formatter.write_str("lease must satisfy start <= renewal <= expiry <= maximum")
            }
            Self::StaleExpiry { stored, fired } => {
                write!(
                    formatter,
                    "lease expires at {stored}, not fired expiry {fired}"
                )
            }
            Self::Early { at, expiry } => {
                write!(formatter, "lapse instant {at} precedes expiry {expiry}")
            }
            Self::MissingResidency => formatter.write_str("lapse requires a residency head"),
            Self::ResidentState(state) => {
                write!(formatter, "resident state {state:?} refuses lapse")
            }
            Self::Changed(detail) => write!(formatter, "{CHANGED}: {detail}"),
        }
    }
}

impl Error for Refusal {}

/// A typed refusal or fault from claim expiry.
#[derive(Debug)]
pub enum LapseError {
    Refused(Refusal),
    Read(GuardError),
    Build(BuildError),
    Commit(CommitError),
}

impl fmt::Display for LapseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(error) => write!(formatter, "claim lapse refused: {error}"),
            Self::Read(error) => write!(formatter, "claim lapse read fault: {error}"),
            Self::Build(error) => write!(formatter, "claim lapse build fault: {error}"),
            Self::Commit(error) => write!(formatter, "claim lapse commit fault: {error}"),
        }
    }
}

impl Error for LapseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Refused(error) => Some(error),
            Self::Read(error) => Some(error),
            Self::Build(error) => Some(error),
            Self::Commit(error) => Some(error),
        }
    }
}

impl From<Refusal> for LapseError {
    fn from(error: Refusal) -> Self {
        Self::Refused(error)
    }
}

/// Re-prove and atomically lapse one expired active claim.
pub fn lapse(
    journal: &mut Journal,
    request: Request,
    owners: &crate::OwnershipSnapshot,
    authority: JournalGuard,
) -> Result<Lapsed, LapseError> {
    lapse_after_observation(journal, request, owners, authority, |_| Ok(()))
}

fn lapse_after_observation<F>(
    journal: &mut Journal,
    request: Request,
    owners: &crate::OwnershipSnapshot,
    authority: JournalGuard,
    after: F,
) -> Result<Lapsed, LapseError>
where
    F: FnOnce(&mut Journal) -> Result<(), LapseError>,
{
    let observed = read_observation(journal, &request)?;
    after(journal)?;
    let task = request.intent.task.clone();
    let mut write = build_write(request, observed, owners)?;
    write.guard(authority);
    let applied = journal.apply(write).map_err(map_commit_error)?;
    Ok(Lapsed {
        task,
        first: applied.first(),
        last: applied.last(),
    })
}

fn read_observation(journal: &Journal, request: &Request) -> Result<Observation, LapseError> {
    journal
        .read_with(|read| observe(read, request))
        .map_err(|error| match error {
            ReadError::Journal(error) | ReadError::Answer(ObserveError::Fault(error)) => {
                LapseError::Read(error)
            }
            ReadError::Answer(ObserveError::Refused(error)) => LapseError::Refused(error),
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Observation {
    statement: JournalHead<wire::TaskHead>,
    workflow: JournalHead<wire::WorkflowHead>,
    claim: JournalHead<wire::ClaimHead>,
    lease: JournalHead<wire::LeaseHead>,
    residency: JournalHead<wire::ResidencyHead>,
    retraction: Option<Retraction>,
    closure: Option<Closure>,
}

enum ObserveError {
    Refused(Refusal),
    Fault(GuardError),
}

impl From<Refusal> for ObserveError {
    fn from(error: Refusal) -> Self {
        Self::Refused(error)
    }
}

impl From<GuardError> for ObserveError {
    fn from(error: GuardError) -> Self {
        Self::Fault(error)
    }
}

fn observe(
    read: &journal::JournalRead<'_>,
    request: &Request,
) -> Result<Observation, ObserveError> {
    let task = &request.intent.task;
    let stamp = task.born().map_err(|error| {
        GuardError::Invalid(format!("task {task} has no canonical birth stamp: {error}"))
    })?;
    let heads = read
        .task_heads_for(&stamp)?
        .ok_or_else(|| Refusal::MissingTask(task.clone()))?;
    let held = heads
        .claims()
        .iter()
        .find(|held| {
            held.claim().body().is_active() && held.claim().body().occupant == request.occupant
        })
        .or_else(|| {
            heads
                .claims()
                .iter()
                .find(|held| held.claim().body().is_active())
        })
        .or_else(|| heads.claims().last())
        .ok_or(Refusal::MissingClaim)?;
    let claim = held.claim().clone();
    let lease = held.lease().cloned().ok_or(Refusal::MissingLease)?;
    let statement = heads.statement().clone();
    let workflow = heads.workflow().clone();
    let mut bodies = heads.into_bodies();
    let ending = bodies
        .claims
        .iter_mut()
        .find(|(current, _)| current.claimed == claim.body().claimed)
        .expect("observed claim remains in its task heads");
    ending.0.disposition = ClaimDisposition::Lapsed;
    let post = Entry::new(
        bodies.statement,
        bodies.workflow,
        bodies.completion,
        bodies.claims,
    )
    .map_err(|error| GuardError::Invalid(format!("task {task} is invalid: {error}")))?;
    let retraction = Retraction::owed(&post);
    let post = match &retraction {
        Some(retraction) => retraction.apply(post),
        None => post,
    };
    let closure = Closure::owed(&post, request.at, |seq| {
        read.room_fact(seq)
            .map(|found| found.map(|envelope| envelope.fact))
    })
    .map_err(|error| match error {
        RoundError::Read(error) => error,
        refused => GuardError::Invalid(refused.to_string()),
    })?;
    admit_expiry(claim.body(), lease.body(), request)?;
    let residency = read.residency_head()?.ok_or(Refusal::MissingResidency)?;
    // Exact claim and lease heads invalidate superseded timers. Residency only
    // gates whether a resident may write; its identity can move before expiry.
    if residency.body().state != wire::ResidentState::Seated {
        return Err(Refusal::ResidentState(residency.body().state).into());
    }
    Ok(Observation {
        statement,
        workflow,
        claim,
        lease,
        residency,
        retraction,
        closure,
    })
}

fn admit_expiry(
    claim: &wire::ClaimHead,
    lease: &wire::LeaseHead,
    request: &Request,
) -> Result<(), Refusal> {
    if claim.disposition != ClaimDisposition::Active {
        return Err(Refusal::Inactive(claim.disposition));
    }
    if claim.epoch != lease.epoch {
        return Err(Refusal::ClaimLeaseEpoch {
            claim: claim.epoch,
            lease: lease.epoch,
        });
    }
    if claim.epoch != request.epoch {
        return Err(Refusal::ClaimEpoch {
            holding: claim.epoch,
            named: request.epoch,
        });
    }
    if claim.occupant != request.occupant {
        return Err(Refusal::ClaimOccupant {
            holding: claim.occupant,
            named: request.occupant,
        });
    }
    if !(lease.start <= lease.renewal
        && lease.renewal <= lease.expiry
        && lease.expiry <= lease.maximum)
    {
        return Err(Refusal::IncoherentLease);
    }
    if lease.expiry != request.expiry {
        return Err(Refusal::StaleExpiry {
            stored: lease.expiry,
            fired: request.expiry,
        });
    }
    if request.at < lease.expiry {
        return Err(Refusal::Early {
            at: request.at,
            expiry: lease.expiry,
        });
    }
    Ok(())
}

fn build_write(
    request: Request,
    mut observed: Observation,
    owners: &crate::OwnershipSnapshot,
) -> Result<JournalWrite, LapseError> {
    let task = request.intent.task.clone();
    let stamp = observed.statement.body().born.clone();
    let holder = observed.claim.body().actor.clone();
    let lane = observed.claim.body().lane;
    let phase = observed
        .closure
        .as_ref()
        .map_or(&observed.workflow.body().phase, |closure| {
            &closure.transition().next().phase
        });
    let ownership = owners.owners_for(
        &observed.statement.body().topic,
        phase,
        observed.statement.body().creation.creator_team(),
    );
    let origin = format!("task:{stamp}")
        .parse()
        .expect("task stamp forms an origin");
    let mut write = JournalWrite::new(holder.clone(), origin, request.at, Producer::BusTimer);
    write.guard(change_guard(request.clone(), observed.clone()));
    let fact = write
        .fact(
            body::ClaimLapsed {
                task: stamp.clone(),
                occupant: request.occupant,
                expiry: request.expiry,
            },
            FactCause::Root,
        )
        .map_err(LapseError::Build)?;
    let mut next = observed.claim.body().clone();
    next.disposition = ClaimDisposition::Lapsed;
    write.claim_head(stamp, fact, move |position| {
        next.fact = position;
        next
    });
    if let Some(retraction) = observed.retraction.take() {
        retraction
            .append(&mut write, FactCause::Pending(fact))
            .map_err(LapseError::Build)?;
    }
    let closed = match observed.closure.take() {
        Some(closure) => {
            closure
                .append(&mut write, None, FactCause::Pending(fact))
                .map_err(LapseError::Build)?;
            true
        }
        None => false,
    };
    write
        .command_result(
            Command {
                v: WireVersion::CURRENT,
                id: request.command,
                epoch: request.epoch,
                kind: CommandKind::ClaimLapse,
                expected: witnesses(&request, &observed, lane, closed),
                body: request.intent,
            },
            request.epoch,
            intent::ClaimAnswer {
                task,
                ownership: Some(intent::TaskOwnership {
                    teams: ownership.teams.clone(),
                    reserved: ownership.reserved,
                }),
                statement: None,
                holder: None,
                acknowledged: None,
                held: false,
                warnings: Box::default(),
                creation_context: None,
                origin_context: None,
                room_context: None,
                next: None,
                phase: None,
                nothing_stolen: false,
                transplant: None,
            },
        )
        .map_err(LapseError::Build)?;
    Ok(write)
}

fn witnesses(
    request: &Request,
    observed: &Observation,
    lane: CanonicalUuid,
    closed: bool,
) -> Vec<Witness> {
    let task = observed.statement.body().born.clone();
    let mut expected = vec![
        Witness::Claim {
            task: task.clone(),
            claimed: Some(observed.claim.body().claimed),
            head: Some(observed.claim.position()),
        },
        Witness::Lease {
            task: task.clone(),
            claimed: observed.claim.body().claimed,
            head: observed.lease.position(),
        },
        Witness::Residency {
            lane,
            occupant: Some(request.occupant),
            epoch: request.epoch,
        },
    ];
    if closed {
        expected.push(Witness::Workflow {
            task,
            head: observed.workflow.position(),
            phase: observed.workflow.body().phase,
        });
    }
    expected
}

fn change_guard(request: Request, expected: Observation) -> JournalGuard {
    JournalGuard::new(move |read| match observe(read, &request) {
        Ok(current) if current == expected => Ok(()),
        Ok(_) => Err(GuardError::refused(format!(
            "{CHANGED}: authoritative heads changed"
        ))),
        Err(ObserveError::Refused(error)) => {
            Err(GuardError::refused(format!("{CHANGED}: {error}")))
        }
        Err(ObserveError::Fault(error)) => Err(error),
    })
}

fn map_commit_error(error: CommitError) -> LapseError {
    match error {
        CommitError::Refused(refusal) if refusal.reason().starts_with(CHANGED) => {
            let detail = refusal
                .reason()
                .strip_prefix(CHANGED)
                .expect("prefix matched")
                .trim_start_matches(": ")
                .to_owned();
            LapseError::Refused(Refusal::Changed(detail))
        }
        error => LapseError::Commit(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{LapseError, Refusal, Request, lapse, lapse_after_observation};
    use crate::allocate::{Mint, mint};
    use journal::{FactCause, Journal, JournalWrite};
    use wire::{
        Actor, CanonicalUuid, Command, CommandKind, Instant, LaneResidency, Origin, Phase,
        Producer, ResidencyHead, ResidentState, TaskHandle, TaskHead, WireVersion, Witness,
        WorkflowHead, body, intent,
    };

    fn uuid(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131164{last:02}")
            .parse()
            .expect("uuid")
    }

    fn instant(minute: u8) -> Instant {
        format!("2026-08-01T07:{minute:02}:00.000Z")
            .parse()
            .expect("instant")
    }

    fn task() -> TaskHandle {
        TaskHandle::for_topic(
            &"/board/possession"
                .parse::<wire::Topic>()
                .expect("fixture topic"),
            &born(),
        )
    }

    fn born() -> wire::TaskStamp {
        "1kKRRsDPx".parse().expect("task stamp")
    }

    fn lapse_request(command: u8) -> Request {
        Request {
            command: uuid(command),
            occupant: uuid(21),
            epoch: uuid(22),
            expiry: instant(20),
            at: instant(20),
            intent: intent::TaskRef { task: task() },
        }
    }

    /// Seed one active claim the way the claim command writes it.
    ///
    /// The shared minter writes the fact and both heads, and the transaction
    /// carries the command receipt beside them, so `command` is spent here
    /// exactly as a real claim would spend it.
    fn claim(journal: &mut Journal, command: u8) {
        let selected = journal
            .read_with(|read| read.task_heads_for(&born()))
            .expect("heads before the claim")
            .expect("the fixture task is on the board");
        let workflow = selected.workflow().body().clone();
        let expected = vec![
            Witness::Task {
                task: born(),
                head: selected.statement().position(),
            },
            Witness::Workflow {
                task: born(),
                head: selected.workflow().position(),
                phase: workflow.phase,
            },
            Witness::Claim {
                task: born(),
                claimed: None,
                head: selected.claim_plane(),
            },
            Witness::Eligibility {
                rule: "next".parse().expect("rule"),
            },
            Witness::Residency {
                lane: uuid(20),
                occupant: Some(uuid(21)),
                epoch: uuid(22),
            },
        ];
        let actor = Actor::Lane(uuid(20));
        let mut write = JournalWrite::new(
            actor.clone(),
            "task:BOARD-a1".parse().expect("claim origin"),
            instant(5),
            Producer::CommandDoor,
        );
        mint(
            &mut write,
            Mint {
                task: &born(),
                actor: &actor,
                workflow: &workflow,
                topic: Some(&"/board/possession".parse().expect("fixture topic")),
                lane: uuid(20),
                occupant: uuid(21),
                epoch: uuid(22),
                at: instant(5),
                expiry: instant(20),
                maximum: instant(35),
                git: None,
                recovery: None,
                source: body::SourceBoundary::Proven { heads: Vec::new() },
                cause: None,
            },
        )
        .expect("the claim mints");
        write
            .command_result(
                Command {
                    v: WireVersion::CURRENT,
                    id: uuid(command),
                    epoch: uuid(22),
                    kind: CommandKind::ClaimWin,
                    expected,
                    body: intent::ClaimWin {
                        rule: "next".parse().expect("rule"),
                        recovery: None,
                    },
                },
                uuid(22),
                intent::ClaimAnswer {
                    ownership: None,
                    task: task(),
                    statement: None,
                    holder: Some(actor),
                    acknowledged: None,
                    held: false,
                    warnings: Box::default(),
                    creation_context: None,
                    origin_context: None,
                    room_context: None,
                    next: None,
                    phase: None,
                    nothing_stolen: false,
                    transplant: None,
                },
            )
            .expect("the claim receipt builds");
        journal.apply(write).expect("the claim commits");
    }

    fn claimed() -> Journal {
        let phase = "todo".parse::<Phase>().expect("phase");
        let origin = "task:BOARD-root".parse::<Origin>().expect("origin");
        let mut journal = Journal::in_memory().expect("journal");
        let mut write = JournalWrite::new(
            Actor::Spice,
            origin.clone(),
            instant(0),
            Producer::CommandDoor,
        );
        let fact = write
            .fact(
                body::TaskAdded {
                    preparation: None,
                    intent: None,
                    task: born(),
                    title: "Lapse".to_owned(),
                    pitch: "A lapse must hand the claim back.".to_owned(),
                    description: None,
                    term: Vec::new(),
                    requirements: wire::EnvironmentRequirements::default(),
                    topic: "/board/possession".parse().expect("fixture topic"),
                    submitted: false,
                    priority: None,
                    taste: None,
                    depends: Vec::new(),
                    flow: vec![wire::Phase::Draft, phase],
                    phase,
                    revision: 1,
                    creation: body::TaskCreationContext::unavailable(
                        body::TaskCreationUnavailable::SpiceActor,
                    ),
                },
                FactCause::Root,
            )
            .expect("task fact");
        write.task_head(fact, move |position| TaskHead {
            preparation: None,
            intent: None,
            born: born(),
            topic_origin: position,
            title: "Lapse".to_owned(),
            pitch: "A lapse must hand the claim back.".to_owned(),
            description: None,
            term: Vec::new(),
            requirements: wire::EnvironmentRequirements::default(),
            resources: wire::ResourceRequirements::default(),
            topic: "/board/possession".parse().expect("fixture topic"),
            priority: None,
            taste: None,
            dependencies: Vec::new(),
            provenance: wire::TaskProvenance::Task(born()),
            creation: body::TaskCreationContext::unavailable(
                body::TaskCreationUnavailable::SpiceActor,
            ),
            revision: position,
        });
        write.workflow_head(born(), fact, move |_| WorkflowHead {
            flow: vec![wire::Phase::Draft, phase],
            phase,
            status: wire::WorkflowStatus::pending(),
            deferral: None,
            submitted: false,
            generation: 0,
        });
        write.residency_head(fact, |_| ResidencyHead {
            epoch: uuid(22),
            state: ResidentState::Seated,
            topology_revision: 0,
            lanes: vec![LaneResidency {
                lane: uuid(20),
                team: uuid(20),
                name: None,
                worktree: "/worktrees/lane-a".to_owned(),
                life: "drive".to_owned(),
                vendor: "omp".parse().expect("fixture vendor"),
                model: None,
                effort: None,
                fast_mode: false,
                occupant: Some(uuid(21)),
                lineage: Vec::new(),
                capabilities: Vec::new(),
                session: None,
                disposition: "occupied".to_owned(),
            }],
            subscriptions: vec![wire::TeamSubscription {
                team: uuid(20),
                formed_at: None,
                drive: true,
                drain: true,
                name: None,
                fast_mode: false,
            }],
            renewals: Vec::new(),
        });
        journal.apply(write).expect("seed");
        claim(&mut journal, 31);
        journal
    }

    #[test]
    fn expiry_changed_after_observation_refuses_before_a_second_fact() {
        let mut journal = claimed();
        let residency = journal
            .read_with(|read| read.residency_head())
            .unwrap()
            .unwrap();
        let owners = crate::OwnershipSnapshot::from_residency(
            crate::team_policy::Config::default(),
            residency.body(),
        );
        let position = residency.position();
        let authority = || {
            journal::JournalGuard::new(move |read| {
                if read
                    .residency_head()?
                    .is_some_and(|head| head.position() == position)
                {
                    Ok(())
                } else {
                    Err(journal::GuardError::refused("fixture membership moved"))
                }
            })
        };
        let outer = lapse_after_observation(
            &mut journal,
            lapse_request(32),
            &owners,
            authority(),
            |journal| {
                lapse(journal, lapse_request(33), &owners, authority())?;
                Ok(())
            },
        );
        assert!(matches!(
            outer,
            Err(LapseError::Refused(Refusal::Changed(_)))
        ));
        assert_eq!(journal.fact_snapshot().expect("facts").facts().len(), 3);
        assert_eq!(journal.verify().expect("verify").receipts, 2);
    }
}
