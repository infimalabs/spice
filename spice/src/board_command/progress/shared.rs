use std::convert::Infallible;

use board::round::{Closure, RoundError, RoundEvidence};

use super::*;

/// Whether `entry`'s held phase completes through a round: a shared target,
/// or an open Design, whose every pass ends in a round awaiting the operator.
pub(in crate::board_command) fn shared_round(entry: &Entry) -> bool {
    wire::intent::is_open_design(entry.statement().intent, entry.workflow())
        || entry.completion().target(&entry.workflow().phase).get() > 1
        || !entry.completion().arrivals.is_empty()
        || entry.active_claims().count() > 1
}

pub(super) fn task_done_shared(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    done: &TaskDone,
    repository: Option<Host<'_>>,
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    operation: wire::CanonicalUuid,
) -> Outcome<CommandAnswer> {
    let held = entry
        .claim_of(command.actor())
        .expect("shared completion requires an active claim");
    let claimed = held.claimed();
    let occupant = held.claim().occupant;
    let phase = entry.workflow().phase;
    let round = entry.completion().round;
    let closes = entry.round_satisfied(Some(command.actor()));
    let (at, lane, _, placement) = match done_transition(
        bus,
        snapshot,
        entry,
        command,
        repository,
        true,
        done.disposition.as_ref(),
    ) {
        Ok(value) => value,
        Err(outcome) => return outcome,
    };
    let source = match bus.source_boundary(&lane) {
        Ok(source) => source,
        Err(error) => return store_fault("task::boundary-unreadable", error.to_string()),
    };
    let origin = match task_origin(entry.born()) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let phase_name = phase.as_str();
    let parked = if gate::lacks_commit_authority(phase_name) {
        match release::prepare_release(snapshot, command, claimed, repository, Some(phase_name)) {
            Ok(parked) => parked,
            Err(outcome) => return outcome,
        }
    } else {
        None
    };
    let published = match publish_task_boundary(repository, snapshot, command, entry) {
        Ok(published) => published,
        Err(outcome) => return rollback_release(parked, outcome),
    };
    let arrival = body::TaskArrived {
        task: entry.born().clone(),
        operation: Some(operation),
        claimed,
        lane,
        occupant,
        phase,
        round,
        validation: done.validation.clone(),
        git: published,
        source,
        disposition: done.disposition.clone(),
    };
    let closure = match closes
        .then(|| closing_round(snapshot, entry, command.actor(), &arrival, at))
        .transpose()
    {
        Ok(closure) => closure,
        Err(error) => return rollback_release(parked, round_fault(error)),
    };
    let placed = super::placement::ClosePlacement::entry_or(placement.as_ref(), entry);
    let (next, ownership, status) =
        shared_receipt(snapshot, placed, command.actor(), closure.as_ref());
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    snapshot.guard_ownership(&mut write);
    if let Some(closure) = &closure
        && let Err(outcome) = preparation::release::completed_plan(
            bus,
            command,
            repository,
            snapshot,
            entry,
            closure.transition().next(),
            &mut write,
        )
    {
        return rollback_release(parked, outcome);
    }
    let arrived = match append_arrival(&mut write, entry, command.actor(), arrival, FactCause::Root)
    {
        Ok(arrived) => arrived,
        Err(outcome) => return rollback_release(parked, outcome),
    };
    let released_by = match append_closure(&mut write, closure, &next, arrived) {
        Ok(released) => released,
        Err(outcome) => return rollback_release(parked, outcome),
    };
    commit_shared_arrival(
        bus,
        write,
        SharedCommit {
            command,
            done,
            entry: placed,
            placement: placement.as_ref(),
            parked,
            released_by,
            status,
            next,
            ownership,
        },
    )
}

fn append_closure(
    write: &mut JournalWrite,
    closure: Option<Closure>,
    next: &Option<Box<str>>,
    arrived: PendingFact,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    match closure {
        Some(closure) => closure
            .append(write, next.clone(), FactCause::Pending(arrived))
            .map_err(|error| store_fault("task::round-completion-build", error.to_string())),
        None => Ok(arrived),
    }
}

/// Read `entry`'s round evidence, add the final `arrival` by `actor`, and
/// close it.
fn closing_round(
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    actor: &Actor,
    arrival: &body::TaskArrived,
    at: Instant,
) -> Result<Closure, RoundError<Infallible>> {
    let mut evidence = RoundEvidence::read(entry, snapshot_fact(snapshot))?;
    evidence.arrive(
        body::RoundParticipant {
            claimed: arrival.claimed,
            actor: actor.clone(),
            lane: arrival.lane,
            occupant: arrival.occupant,
            validation: arrival.validation.clone(),
            git: arrival.git.clone(),
            source: arrival.source.clone(),
        },
        arrival.disposition.clone(),
    )?;
    Closure::new(entry, evidence, at)
}

fn shared_receipt(
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    actor: &Actor,
    closure: Option<&Closure>,
) -> (Option<Box<str>>, wire::intent::TaskOwnership, String) {
    let status = closure
        .map_or("arrived", |closure| {
            closure.transition().next().status.as_str()
        })
        .to_owned();
    let (next, ownership) = match closure {
        Some(closure) => {
            let post = freed_after(entry, actor, closure.transition().next());
            (next_action(snapshot, &post), snapshot.ownership(&post))
        }
        None => (Some("spice task next".into()), snapshot.ownership(entry)),
    };
    (next, ownership, status)
}

struct SharedCommit<'a> {
    command: &'a wire::AdmittedCommand,
    done: &'a TaskDone,
    entry: &'a Entry,
    /// The placement the round's closing arrival commits.
    placement: Option<&'a super::placement::ClosePlacement>,
    parked: Option<spice::task_git::ParkedRelease<'a>>,
    released_by: PendingFact,
    status: String,
    next: Option<Box<str>>,
    ownership: wire::intent::TaskOwnership,
}

fn commit_shared_arrival(
    bus: &Bus,
    mut write: JournalWrite,
    shared: SharedCommit<'_>,
) -> Outcome<CommandAnswer> {
    let SharedCommit {
        command,
        done,
        entry,
        placement,
        parked,
        released_by,
        status,
        next,
        ownership,
    } = shared;
    if let Some(placement) = placement
        && let Err(outcome) = placement.append(&mut write, released_by)
    {
        return rollback_release(parked, outcome);
    }
    if let Err(outcome) = free_claim(
        &mut write,
        entry,
        command.actor(),
        released_by,
        parked.as_ref().map(|release| release.parked()),
    ) {
        return rollback_release(parked, outcome);
    }
    let answer = wire::intent::TaskAnswer {
        diagnostic: None,
        placement: None,
        created: Default::default(),
        taste: None,
        completion_contract: None,
        task: entry.handle().clone(),
        ownership: Some(ownership),
        status,
        completion: None,
        claim: None,
        closed: Vec::new(),
        next,
    };
    if let Err(error) = write.command_result(
        request(command, done.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return rollback_release(parked, store_fault("task::done-receipt", error.to_string()));
    }
    match parked {
        Some(parked) => committed_with_task_git(
            bus,
            write,
            CommandAnswer::Task(Box::new(answer)),
            PreparedTaskGit::Release(parked),
        ),
        None => committed(bus, write, CommandAnswer::Task(Box::new(answer))),
    }
}

fn append_arrival(
    write: &mut JournalWrite,
    entry: &Entry,
    actor: &Actor,
    arrival: body::TaskArrived,
    cause: FactCause,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    let claimed = arrival.claimed;
    let lane = arrival.lane;
    let arrived = write
        .fact(arrival, cause)
        .map_err(|error| store_fault("task::arrival-build", error.to_string()))?;
    let mut completion = entry.completion().clone();
    let actor = actor.clone();
    write.completion_head(entry.born().clone(), arrived, move |fact| {
        completion.arrivals.push(wire::Arrival {
            claimed,
            actor,
            lane,
            fact,
        });
        completion
    });
    Ok(arrived)
}

/// What one clean verdict brings to its shared review round.
pub(super) struct ReviewEvidence {
    pub(super) lane: wire::CanonicalUuid,
    pub(super) validation: String,
    pub(super) git: Option<body::GitBoundary>,
    pub(super) source: body::SourceBoundary,
}

/// One clean verdict's arrival in its shared review round, and the closure it
/// owes when it is the round's last required arrival.
pub(super) struct ReviewArrival {
    arrival: body::TaskArrived,
    closure: Option<Closure>,
}

impl ReviewArrival {
    /// The arrival `actor`'s clean verdict records. A disposed Plan's review
    /// arrivals carry its disposition, so every closer of the round settles it
    /// or skips to its next review exactly as a sole reviewer's verdict would.
    pub(super) fn new(
        snapshot: &ValidatedSnapshot,
        entry: &Entry,
        actor: &Actor,
        at: Instant,
        evidence: ReviewEvidence,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        let held = entry
            .claim_of(actor)
            .expect("an admitted review verdict holds its claim");
        let arrival = body::TaskArrived {
            task: entry.born().clone(),
            operation: None,
            claimed: held.claimed(),
            lane: evidence.lane,
            occupant: held.claim().occupant,
            phase: entry.workflow().phase,
            round: entry.completion().round,
            validation: evidence.validation,
            git: evidence.git,
            source: evidence.source,
            disposition: snapshot.facts.plan_disposition(entry.born()).cloned(),
        };
        let closure = entry
            .round_satisfied(Some(actor))
            .then(|| closing_round(snapshot, entry, actor, &arrival, at))
            .transpose()
            .map_err(round_fault)?;
        Ok(Self { arrival, closure })
    }

    /// Whether this arrival completes its round.
    pub(super) const fn closes(&self) -> bool {
        self.closure.is_some()
    }

    /// Record the arrival, caused by the `reviewed` fact, and the round's
    /// completion when this arrival closes it.
    pub(super) fn append(
        self,
        write: &mut JournalWrite,
        entry: &Entry,
        actor: &Actor,
        reviewed: PendingFact,
        next: Option<Box<str>>,
    ) -> Result<(), Outcome<CommandAnswer>> {
        let arrived = append_arrival(
            write,
            entry,
            actor,
            self.arrival,
            FactCause::Pending(reviewed),
        )?;
        if let Some(closure) = self.closure {
            closure
                .append(write, next, FactCause::Pending(arrived))
                .map_err(|error| store_fault("task::round-completion-build", error.to_string()))?;
        }
        Ok(())
    }
}

/// The retained fact at each position of `snapshot`, as round evidence reads it.
pub(in crate::board_command) fn snapshot_fact<'a>(
    snapshot: &'a ValidatedSnapshot,
) -> impl FnMut(Seq) -> Result<Option<&'a wire::AnyFact>, Infallible> + 'a {
    |seq| Ok(snapshot.facts.at(seq).map(|envelope| &envelope.fact))
}

/// The command outcome for round evidence that cannot close its round.
pub(in crate::board_command) fn round_fault(
    error: RoundError<Infallible>,
) -> Outcome<CommandAnswer> {
    let detail = error.to_string();
    match error {
        RoundError::Read(never) => match never {},
        RoundError::Missing(_) => store_fault("task::arrival-missing", &detail),
        RoundError::NotArrival(_) | RoundError::Disagrees(_) => {
            store_fault("task::arrival-invalid", &detail)
        }
        RoundError::DispositionConflict => protocol_fault("task::disposition-conflict", &detail),
        RoundError::Phase(error) => refusal(
            "task::phase-refused",
            Some(error.to_string()),
            "close a shared round only while its workflow is pending",
        ),
    }
}
