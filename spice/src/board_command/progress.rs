use super::*;

mod placement;
mod refresh;
mod release;
mod replay;
mod shared;
use refresh::*;
pub(super) use release::claim_free;
use replay::replay_task_completion;
use shared::task_done_shared;
pub(in crate::board_command) use shared::{round_fault, shared_round, snapshot_fact};

pub(super) fn claim_refresh(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    refresh: &TaskRef,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    claim_refresh_with_clock(bus, command, refresh, repository, current_instant)
}

pub(super) fn claim_refresh_with_clock(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    refresh: &TaskRef,
    repository: Option<Host<'_>>,
    clock: impl FnOnce() -> Result<Instant, Outcome<CommandAnswer>>,
) -> Outcome<CommandAnswer> {
    let snapshot = match command_task_snapshot(
        bus,
        repository.map(|host| host.repository),
        command,
        &refresh.task,
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let entry = match refresh_entry(&snapshot, refresh) {
        Ok(entry) => entry,
        Err(outcome) => return outcome,
    };
    let at = match clock() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let state = match refresh_claim(bus, entry, &snapshot, command, refresh, at) {
        Ok(state) => state,
        Err(outcome) => return outcome,
    };
    let origin = match task_origin(entry.born()) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let held_claim = entry
        .claim_of(command.actor())
        .expect("refresh admission requires a claim");
    let acknowledged = held_claim
        .claim()
        .is_stale(entry.workflow())
        .then_some(entry.workflow().generation);
    let held = held_claim
        .lease()
        .is_some_and(spice::claim_lease::is_indefinite);
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    snapshot.guard_ownership(&mut write);
    if let Err(outcome) = append_refresh(&mut write, entry, command.actor(), state) {
        return outcome;
    }
    let answer = wire::intent::ClaimAnswer {
        task: entry.handle().clone(),
        ownership: Some(snapshot.ownership(entry)),
        statement: Some(Box::new(wire::intent::ClaimStatement {
            title: entry.statement().title.clone(),
            pitch: entry.statement().pitch.clone(),
            description: entry.statement().description.clone(),
            term: statement_items_with_handles(&entry.statement().term),
            design_research: snapshot
                .answer
                .tasks
                .iter()
                .find(|task| task.statement.body.born == *entry.born())
                .and_then(BoardTask::design_research)
                .map(String::into_boxed_str),
            completion_contract: Some(
                completion_contract(
                    &entry.statement().term,
                    entry.born(),
                    &entry.statement().dependencies,
                    entry.statement().intent,
                    snapshot.entries.iter(),
                )
                .into_boxed_str(),
            ),
        })),
        holder: Some(command.actor().clone()),
        acknowledged,
        held,
        warnings: Box::default(),
        creation_context: None,
        origin_context: None,
        room_context: None,
        next: None,
        phase: None,
        nothing_stolen: false,
        transplant: None,
    };
    if let Err(error) = write.command_result(
        request(command, refresh.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("claim::refresh-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::Claim(Box::new(answer)))
}

// Generation acknowledgement and residency recovery live in
// `progress/refresh.rs`, apart from ordinary workflow transitions.

pub(super) fn task_done(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    done: &TaskDone,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let snapshot = match command_task_snapshot(
        bus,
        repository.map(|host| host.repository),
        command,
        &done.task,
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let entry = match done_entry(&snapshot, done) {
        Ok(entry) => entry,
        Err(outcome) => return outcome,
    };
    let operation = match completion_operation(done, entry) {
        Ok(operation) => operation,
        Err(outcome) => return outcome,
    };
    if let Some(replayed) =
        replay_task_completion(bus, &snapshot.facts, command, done, entry, operation)
    {
        return replayed;
    }
    if shared_round(entry) {
        return task_done_shared(bus, command, done, repository, &snapshot, entry, operation);
    }
    let (at, lane, transition, placement) = match done_transition(
        bus,
        &snapshot,
        entry,
        command,
        repository,
        false,
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
    let parked = match release::for_completion(&snapshot, command, entry, repository) {
        Ok(parked) => parked,
        Err(outcome) => return outcome,
    };
    let published = match publish_task_boundary(repository, &snapshot, command, entry) {
        Ok(published) => published,
        Err(outcome) => return rollback_release(parked, outcome),
    };
    let carried = carries_claim(
        transition.kind(),
        &transition.next().flow,
        published.as_ref(),
        parked.is_some(),
    );
    let placed = placement::ClosePlacement::entry_or(placement.as_ref(), entry);
    let post = freed_after(placed, command.actor(), transition.next());
    let next = if carried {
        Some(claim_activation_next_action(placed))
    } else {
        next_action(&snapshot, &post)
    };
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    snapshot.guard_ownership(&mut write);
    let answer = match append_done(
        &mut write,
        command,
        done,
        placed,
        Completion {
            transition: &transition,
            at,
            published,
            source,
            next,
            parked: parked.as_ref().map(|release| release.parked()),
            carried,
            ownership: snapshot.ownership(&post),
            placement: placement.as_ref(),
        },
    ) {
        Ok(answer) => answer,
        Err(outcome) => return rollback_release(parked, outcome),
    };
    if let Err(outcome) = preparation::release::completed_plan(
        bus,
        command,
        repository,
        &snapshot,
        entry,
        transition.next(),
        &mut write,
    ) {
        return rollback_release(parked, outcome);
    }
    if let Err(error) = write.command_result(
        request(command, done.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return rollback_release(parked, store_fault("task::done-receipt", error.to_string()));
    }
    commit_done(bus, write, answer, parked)
}

fn commit_done(
    bus: &Bus,
    write: JournalWrite,
    answer: wire::intent::TaskAnswer,
    parked: Option<spice::task_git::ParkedRelease<'_>>,
) -> Outcome<CommandAnswer> {
    let answer = CommandAnswer::Task(Box::new(answer));
    match parked {
        Some(parked) => {
            committed_with_task_git(bus, write, answer, PreparedTaskGit::Release(parked))
        }
        None => committed(bus, write, answer),
    }
}

fn done_entry<'a>(
    snapshot: &'a ValidatedSnapshot,
    done: &TaskDone,
) -> Result<&'a Entry, Outcome<CommandAnswer>> {
    task_entry(&snapshot.entries, &done.task).ok_or_else(|| {
        refusal(
            "task::unknown",
            Some(done.task.to_string()),
            concat!(
                "read board and complete the task held by ",
                "this capability",
            ),
        )
    })
}

fn completion_operation(
    done: &TaskDone,
    entry: &Entry,
) -> Result<wire::CanonicalUuid, Outcome<CommandAnswer>> {
    done.operation.ok_or_else(|| {
        refusal(
            "task::completion-operation-required",
            Some(entry.handle().to_string()),
            "retry with a current client that supplies one durable completion operation",
        )
    })
}

/// Admit `command` completing `entry`'s held phase, with the placement that
/// closing a draft commits; in a shared round only the arrival that closes it
/// places.
fn done_transition(
    bus: &Bus,
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'_>>,
    shared: bool,
    disposition: Option<&body::PlanDisposition>,
) -> Result<DoneTransition, Outcome<CommandAnswer>> {
    let at = current_instant()?;
    let active = board::Active::admit(entry, command.actor(), at).map_err(|error| {
        refusal(
            "claim::active-refused",
            Some(error.to_string()),
            "run spice task next before completing its phase",
        )
    })?;
    if active.epoch() != command.epoch() {
        return Err(refusal(
            "claim::epoch-moved",
            Some(active.epoch().to_string()),
            format!(
                "run spice task next to rebind task {} under the current resident generation",
                entry.handle()
            ),
        ));
    }
    acknowledged(entry, command.actor())?;
    if !shared {
        sole_participant(entry, command.actor())?;
    }
    refuse_taste_hold(entry)?;
    if disposition.is_none()
        && wire::intent::is_open_design(entry.statement().intent, entry.workflow())
    {
        if entry.round_satisfied(Some(command.actor())) {
            require_term(snapshot, command, entry.born())?;
            let observed = bus
                .task_snapshot()
                .map_err(|error| store_fault("task::heads-unreadable", error.to_string()))?;
            preparation::require_question_coverage(&observed, entry.born())?;
            preparation::require_pass_outcome(&observed, entry.born(), command.actor())?;
        }
        let transition = board::Transition::after_round(entry, at, false).map_err(|error| {
            refusal(
                "task::phase-refused",
                Some(error.to_string()),
                "complete only the currently held workflow phase",
            )
        })?;
        return Ok((at, active.lane(), transition, None));
    }
    if entry.workflow().phase.as_str() == "review" {
        return Err(refusal(
            "task::review-required",
            Some(entry.handle().to_string()),
            "record an assigned native review verdict before advancing review",
        ));
    }
    let current = entry
        .workflow()
        .flow
        .iter()
        .position(|phase| phase == &entry.workflow().phase)
        .expect("validated workflow contains its phase");
    let transition = match (disposition, entry.workflow().flow.get(current + 1)) {
        (Some(_), _) => board::Transition::dispose(entry, &active, at),
        (None, Some(next)) => board::Transition::advance(entry, &active, *next),
        (None, None) => board::Transition::complete(entry, &active, at),
    }
    .map_err(|error| {
        refusal(
            "task::phase-refused",
            Some(error.to_string()),
            "complete only the currently held workflow phase",
        )
    })?;
    admit_completion(snapshot, entry, command, disposition)?;
    let placement = if !shared || entry.round_satisfied(Some(command.actor())) {
        placement::ClosePlacement::at_close(
            bus,
            snapshot,
            command,
            repository,
            entry,
            transition.next(),
            disposition.is_some(),
        )?
    } else {
        None
    };
    Ok((at, active.lane(), transition, placement))
}

type DoneTransition = (
    Instant,
    wire::CanonicalUuid,
    board::Transition,
    Option<placement::ClosePlacement>,
);

/// Refuse completing any phase of a task whose statement stands under a taste
/// hold, naming each match and the rewording that clears it.
fn refuse_taste_hold(entry: &Entry) -> Result<(), Outcome<CommandAnswer>> {
    entry
        .statement()
        .taste
        .as_ref()
        .map_or(Ok(()), |hold| Err(taste_hold_refusal(entry, hold)))
}

/// Admit a successful completion: a phase advance, a final completion, or a
/// clean review verdict.
///
/// A Plan's validated no-construction disposition closes its plan phase
/// without acceptance, and the one disposition recorded then stands for every
/// later phase; closing the plan phase again restates it. Any other completion
/// of a task whose intent is Plan rests on linked executable work; every other
/// successful completion rests on the task's acceptance contract.
fn admit_completion(
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    command: &wire::AdmittedCommand,
    disposition: Option<&body::PlanDisposition>,
) -> Result<(), Outcome<CommandAnswer>> {
    if let Some(disposition) = disposition {
        let plan = visible_task(snapshot, entry.born())?;
        require_expected(command, &statement_witness(plan))?;
        require_expected(command, &workflow_witness(plan))?;
        return admit_disposition(snapshot, entry, command, disposition);
    }
    // Leaving draft is admitted by the phase-zero validator the close runs;
    // a Plan's linked-output contract applies once it has left draft.
    if entry.is_draft() {
        return Ok(());
    }
    if entry.workflow().phase != wire::Phase::Plan
        && snapshot.facts.plan_disposition(entry.born()).is_some()
    {
        return Ok(());
    }
    if entry.statement().intent == Some(wire::TaskIntent::Plan) {
        return planning::require_output(snapshot, command, entry);
    }
    require_term(snapshot, command, entry.born())
}

/// Admit a Plan lane's no-construction disposition on the completion that
/// closes its plan phase.
///
/// Only the Plan's own plan phase records one, never beside work the Plan
/// constructed that is still live, and a cover names an existing task the
/// command witnessed or one full commit id.
fn admit_disposition(
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    command: &wire::AdmittedCommand,
    disposition: &body::PlanDisposition,
) -> Result<(), Outcome<CommandAnswer>> {
    let plan = entry.born();
    if snapshot
        .facts
        .plan_disposition(plan)
        .is_some_and(|recorded| recorded != disposition)
    {
        return Err(refusal(
            "task::disposition-conflict",
            Some(entry.handle().to_string()),
            "a Plan records one disposition; complete it without --disposition, or restate the \
             recorded one when closing its plan phase again",
        ));
    }
    let owner = planning::construction_owner(plan, entry.statement().preparation.as_ref());
    if !planning::planning_deliverable(entry) {
        return Err(refusal(
            "task::disposition-refused",
            Some(entry.handle().to_string()),
            "record a no-construction disposition only when completing a Plan's own plan phase; \
             remove an ordinary claimed duplicate with `spice task delete HANDLE REASON \
             --force-claimed`",
        ));
    }
    if owner != plan
        || entry.workflow().phase.as_str() != "plan"
        || !planning::declares_plan(entry.statement().intent, &entry.workflow().flow)
    {
        return Err(refusal(
            "task::disposition-refused",
            Some(entry.handle().to_string()),
            "record a no-construction disposition only when completing a Plan's own plan phase",
        ));
    }
    // Work the Plan constructed, or linked accepted output that answers its
    // need, contradicts a claim that no work is warranted.
    let constructed = snapshot.answer.tasks.iter().find(|task| {
        task.workflow.body.status.is_pending()
            && task
                .statement
                .body
                .preparation
                .as_ref()
                .is_some_and(|preparation| preparation.parent == *plan)
    });
    let linked = planning::plan_outputs(plan, snapshot.answer.tasks.iter());
    if let Some(output) = constructed.or(linked.first().copied()) {
        return Err(refusal(
            "task::disposition-beside-output",
            Some(output.statement.body.handle.to_string()),
            "a Plan with live or linked constructed work completes through that work; settle, \
             unlink, or delete it before recording that no work is warranted",
        ));
    }
    let witnessed = |cover: &wire::TaskStamp| {
        command
            .expected()
            .iter()
            .any(|witness| matches!(witness, Witness::Task { task, .. } if task == cover))
    };
    match disposition.cover() {
        Some(cover @ body::DispositionCover::Commit(_)) if !cover.is_well_formed() => Err(refusal(
            "task::disposition-cover-invalid",
            Some(format!("{cover:?}")),
            "name a covering commit by its full object id",
        )),
        Some(body::DispositionCover::Task(cover)) if cover == plan || !witnessed(cover) => {
            Err(refusal(
                "task::disposition-cover-invalid",
                Some(cover.to_string()),
                "name another existing task as the cover and witness its statement",
            ))
        }
        _ => Ok(()),
    }
}

/// Whether a completion carries its claim into the phase it enters rather than
/// freeing it: nothing published or parked, and another declared phase after
/// the one entered. A review phase is never carried, because review goes to a
/// peer before its author. Leaving draft always carries: the holder who
/// advanced the draft keeps the work it just made whole, as `task file
/// --claim` would have given it.
pub(super) fn carries_claim(
    kind: &board::TransitionKind,
    flow: &[wire::Phase],
    published: Option<&body::GitBoundary>,
    parks: bool,
) -> bool {
    let board::TransitionKind::Advance { from, to } = kind else {
        return false;
    };
    if parks || published.is_some_and(|boundary| boundary.local_commits > 0) {
        return false;
    }
    if *to == wire::Phase::Review {
        return false;
    }
    *from == wire::Phase::Draft
        || flow
            .iter()
            .position(|phase| phase == to)
            .is_some_and(|index| index + 1 < flow.len())
}

/// What one completion settled before the journal opened: the workflow change,
/// the boundaries it recorded, and whether the claim is carried or freed.
struct Completion<'a> {
    transition: &'a board::Transition,
    at: Instant,
    published: Option<body::GitBoundary>,
    source: body::SourceBoundary,
    next: Option<Box<str>>,
    parked: Option<&'a body::TaskParkedCommits>,
    carried: bool,
    ownership: wire::intent::TaskOwnership,
    /// The placement this close commits for an unplaced task.
    placement: Option<&'a placement::ClosePlacement>,
}

fn append_done(
    write: &mut JournalWrite,
    command: &wire::AdmittedCommand,
    done: &TaskDone,
    entry: &Entry,
    completion: Completion<'_>,
) -> Result<wire::intent::TaskAnswer, Outcome<CommandAnswer>> {
    let Completion {
        transition,
        at,
        published,
        source,
        next,
        parked,
        carried,
        ownership,
        placement,
    } = completion;
    let status = transition.next().status.as_str().to_owned();
    let terminal_status = completion_status(transition.next());
    let completed = write
        .fact(
            body::TaskDone {
                task: entry.born().clone(),
                validation: done.validation.clone(),
                terminal: Some(body::TaskCompletionTerminal {
                    operation: done
                        .operation
                        .expect("live task completion requires an operation"),
                    status: terminal_status,
                    next: next.clone(),
                }),
                git: published.clone(),
                source: source.clone(),
                disposition: done.disposition.clone(),
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("task::done-build", error.to_string()))?;
    placement
        .map(|placement| placement.append(write, completed))
        .transpose()?;
    let workflow_fact = match transition.kind() {
        board::TransitionKind::Advance { to, .. } => write
            .fact(
                body::TaskPhased {
                    task: entry.born().clone(),
                    phase: *to,
                },
                FactCause::Pending(completed),
            )
            .map_err(|error| store_fault("task::phase-build", error.to_string()))?,
        board::TransitionKind::Complete { .. } => completed,
        board::TransitionKind::Pass { .. } => unreachable!("open Designs complete in a round"),
    };
    let workflow = transition.next().clone();
    write.workflow_head(entry.born().clone(), workflow_fact, move |_| workflow);
    let claim = if carried {
        // Mint the completing lane's claim on the phase just entered, in place
        // of the claim it held, with the ordinary lease and the boundaries the
        // completion recorded, as a consequence of the phase fact. The held
        // claim ends freed under the new win, so the task retains it as ended
        // since that win.
        let held = entry
            .claim_of(command.actor())
            .expect("Active proof requires a claim head")
            .claim()
            .clone();
        let (expiry, maximum) = claim_lease(at)?;
        let won = mint_claim(
            write,
            board::allocate::Mint {
                task: entry.born(),
                actor: command.actor(),
                workflow: transition.next(),
                topic: entry.topic(),
                lane: held.lane,
                occupant: held.occupant,
                epoch: command.epoch(),
                at,
                expiry,
                maximum,
                git: published,
                recovery: None,
                source,
                cause: Some(workflow_fact),
            },
        )?;
        write.claim_head(entry.born().clone(), won, move |fact| ClaimHead {
            fact,
            disposition: ClaimDisposition::Freed,
            ..held
        });
        Some(Box::new(carried_claim_answer(
            entry,
            command.actor(),
            &transition.next().phase,
            Some(ownership.clone()),
        )))
    } else {
        free_claim(write, entry, command.actor(), completed, parked)?;
        None
    };
    Ok(wire::intent::TaskAnswer {
        diagnostic: None,
        duplicates: Vec::new(),
        duplicates_omitted: 0,
        placement: None,
        created: Default::default(),
        taste: None,
        completion_contract: None,
        task: entry.statement().handle(transition.next().phase),
        ownership: Some(ownership),
        status,
        completion: None,
        claim,
        closed: Vec::new(),
        next,
    })
}

/// The completion status a workflow entering `next` records.
fn completion_status(next: &wire::WorkflowHead) -> body::TaskCompletionStatus {
    if next.status.is_pending() {
        body::TaskCompletionStatus::Pending
    } else {
        body::TaskCompletionStatus::Completed
    }
}

/// Free `actor`'s completing claim, parking commits when the completion
/// released any.
fn free_claim(
    write: &mut JournalWrite,
    entry: &Entry,
    actor: &Actor,
    completed: PendingFact,
    parked: Option<&body::TaskParkedCommits>,
) -> Result<(), Outcome<CommandAnswer>> {
    let mut claim = entry
        .claim_of(actor)
        .expect("Active proof requires a claim head")
        .claim()
        .clone();
    claim.disposition = ClaimDisposition::Freed;
    let released = if let Some(parked) = parked {
        write
            .fact(
                body::ClaimFreed {
                    task: entry.born().clone(),
                    actor: claim.actor.clone(),
                    parked: Some(parked.clone()),
                },
                FactCause::Pending(completed),
            )
            .map_err(|error| store_fault("task::plan-release-build", error.to_string()))?
    } else {
        completed
    };
    write.claim_head(entry.born().clone(), released, move |fact| ClaimHead {
        fact,
        ..claim
    });
    Ok(())
}

/// The carried claim said where the lane reads the completion: the phase it
/// now holds and that phase's deliverable, with the statement it already works
/// from omitted.
fn carried_claim_answer(
    entry: &Entry,
    actor: &Actor,
    phase: &wire::Phase,
    ownership: Option<wire::intent::TaskOwnership>,
) -> wire::intent::ClaimAnswer {
    wire::intent::ClaimAnswer {
        task: entry.statement().handle(*phase),
        ownership,
        statement: None,
        holder: Some(actor.clone()),
        acknowledged: None,
        held: false,
        warnings: Box::default(),
        creation_context: None,
        origin_context: None,
        room_context: None,
        next: Some(claim_activation_next_action(entry)),
        phase: Some(wire::intent::ClaimPhase {
            name: *phase,
            deliverable: Some(Box::from(board::phase::deliverable(phase))),
            target: 1,
            partners: Vec::new(),
            arrivals: Vec::new(),
        }),
        nothing_stolen: false,
        transplant: None,
    }
}

/// `entry` once `actor` frees its active claim under `statement` and
/// `workflow`.
fn freed_projection(
    entry: &Entry,
    actor: &Actor,
    statement: wire::TaskHead,
    workflow: wire::WorkflowHead,
) -> Entry {
    let claims = entry
        .claims()
        .iter()
        .map(|held| {
            let mut claim = held.claim().clone();
            if held.is_active() && claim.actor == *actor {
                claim.disposition = ClaimDisposition::Freed;
            }
            (claim, held.lease().cloned())
        })
        .collect();
    Entry::new(statement, workflow, entry.completion().clone(), claims)
        .expect("an admitted transition preserves a valid task entry")
        .with_design(entry.design())
        .with_creator_team(entry.creator_team())
}

/// `entry` once `actor` frees its active claim and the workflow becomes `next`.
fn freed_after(entry: &Entry, actor: &Actor, next: &wire::WorkflowHead) -> Entry {
    freed_projection(entry, actor, entry.statement().clone(), next.clone())
}

/// Resolve the one entry this actor may settle, and the transition it completes.
///
/// Every refusal on the way to a verdict lives here: an unknown handle, a lane
/// whose verdict already arrived in the current round, a task this actor does
/// not review, a claim that is no longer active, and a phase that is not the
/// final one. Each seated reviewer holds its own review claim, so the verdict
/// needs no sole participant. The command's statement, workflow, and claim
/// witnesses bind the verdict to the exact revision, generation, round, and
/// claim it judged: a finding rewrites the statement and workflow heads when
/// it opens the fresh round, and a round that closed moved the workflow.
fn admit_review<'a>(
    snapshot: &'a ValidatedSnapshot,
    command: &'a wire::AdmittedCommand,
    review: &TaskReview,
) -> Result<(&'a Entry, board::Transition, Instant, wire::CanonicalUuid), Outcome<CommandAnswer>> {
    let Some(entry) = task_entry(&snapshot.entries, &review.task) else {
        return Err(refusal(
            "task::unknown",
            Some(review.task.to_string()),
            "read board and name one current review task",
        ));
    };
    if let Some(arrival) = entry
        .completion()
        .arrivals
        .iter()
        .find(|arrival| arrival.actor == *command.actor())
    {
        return Err(refusal(
            "review::already-arrived",
            Some(format!(
                "task {} round {} already holds this lane's clean verdict at position {}",
                entry.handle(),
                entry.completion().round,
                arrival.fact,
            )),
            "one lane records one verdict per review round; the round closes when the remaining reviewers arrive",
        ));
    }
    if entry.workflow().phase.as_str() != "review" || entry.claim_of(command.actor()).is_none() {
        return Err(refusal(
            "review::assignment-refused",
            Some(entry.handle().to_string()),
            "win the review phase with task next before recording its verdict",
        ));
    }
    let at = current_instant()?;
    let active = board::Active::admit(entry, command.actor(), at).map_err(|error| {
        refusal(
            "claim::active-refused",
            Some(error.to_string()),
            "win a current review claim before recording its verdict",
        )
    })?;
    acknowledged(entry, command.actor())?;
    let current = entry
        .workflow()
        .flow
        .iter()
        .position(|phase| phase == &entry.workflow().phase)
        .expect("validated workflow contains its phase");
    // A disposed Plan constructs nothing, so after review it enters only a later
    // review or settles.
    let disposed = snapshot.facts.plan_disposition(entry.born()).is_some();
    let transition = match (disposed, entry.workflow().flow.get(current + 1)) {
        (true, _) => board::Transition::dispose(entry, &active, at),
        (false, Some(next)) => board::Transition::advance(entry, &active, *next),
        (false, None) => board::Transition::complete(entry, &active, at),
    }
    .map_err(|error| {
        refusal(
            "review::phase-refused",
            Some(error.to_string()),
            "review only the currently assigned workflow phase",
        )
    })?;
    Ok((entry, transition, at, active.lane()))
}

/// Refuse an active claim that has not acknowledged its task's generation.
///
/// The task changed after the claim last took stock of it through a statement
/// edit, observation, deferral, or workflow move. The holder must read the
/// current statement through `task next` before completing or reviewing.
fn acknowledged(entry: &Entry, actor: &Actor) -> Result<(), Outcome<CommandAnswer>> {
    let claim = entry
        .claim_of(actor)
        .expect("an admitted active claim has a head")
        .claim();
    if !claim.is_stale(entry.workflow()) {
        return Ok(());
    }
    Err(refusal(
        "claim::generation-stale",
        Some(format!(
            "task {} is at generation {}; this claim acknowledged {}",
            entry.handle(),
            entry.workflow().generation,
            claim.acknowledged
        )),
        format!(
            "task {} changed since this claim acknowledged it; run spice task next to read and acknowledge the current statement",
            entry.handle(),
        ),
    ))
}

/// Refuse a completion that one participant cannot decide alone: the phase
/// targets several participants, or another claim is active on the round.
fn sole_participant(entry: &Entry, actor: &Actor) -> Result<(), Outcome<CommandAnswer>> {
    let target = entry.completion().target(&entry.workflow().phase).get();
    let siblings = entry
        .active_claims()
        .filter(|held| held.claim().actor != *actor)
        .count();
    if target == 1 && siblings == 0 {
        return Ok(());
    }
    Err(refusal(
        "completion::round-shared",
        Some(format!(
            "task {} phase {} targets {target} participants with {siblings} other active claims",
            entry.handle(),
            entry.workflow().phase,
        )),
        "free the other claims and set the phase target to one with spice task target before completing",
    ))
}

/// What one review verdict records: the verdict, a finding's text with its
/// note, a clean verdict's note, and the corrections the review linked.
struct ReviewRecord {
    verdict: body::Verdict,
    finding: Option<String>,
    note: Option<String>,
    followups: Vec<TaskStamp>,
}

/// Decide the verdict a review records and the text it carries.
///
/// A finding that tracks nothing is refused here rather than written, so the
/// change a reviewer asked for always has a task behind the reviewed one.
fn review_verdict(
    review: &TaskReview,
) -> Result<(body::Verdict, Option<String>, Option<String>), Outcome<CommandAnswer>> {
    let verdict = if review_is_clean(&review.finding) {
        body::Verdict::Clean
    } else {
        body::Verdict::Finding
    };
    if verdict == body::Verdict::Finding && review.then.is_empty() && review.followup.is_empty() {
        return Err(refusal(
            "review::followup-required",
            Some(review.finding.clone()),
            "track an unclean review with --then \"title=... | topic=/...\" or --followup HANDLE",
        ));
    }
    let note = review
        .note
        .as_ref()
        .filter(|note| !note.trim().is_empty())
        .cloned();
    Ok(match verdict {
        body::Verdict::Clean => (verdict, None, note),
        body::Verdict::Finding => {
            let finding = match note {
                Some(note) => format!("{}\n\n{note}", review.finding),
                None => review.finding.clone(),
            };
            (verdict, Some(finding), None)
        }
    })
}

/// Every correction this review links, in the order the reviewer named it:
/// each new `--then` task, then each existing `--followup` a finding attached
/// to `statement`, the reviewed task's statement after the review.
fn review_followups(
    review: &TaskReview,
    verdict: body::Verdict,
    statement: &TaskHead,
) -> Vec<TaskStamp> {
    let mut followups = review
        .then
        .iter()
        .map(|followup| followup.task.clone())
        .collect::<Vec<_>>();
    if verdict == body::Verdict::Finding {
        for born in review
            .followup
            .iter()
            .filter_map(|handle| handle.born().ok())
        {
            if statement.dependencies.contains(&born) && !followups.contains(&born) {
                followups.push(born);
            }
        }
    }
    followups
}

fn review_followup_creation(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    snapshot: &ValidatedSnapshot,
    review: &TaskReview,
) -> Result<Option<TaskCreationCapture>, Outcome<CommandAnswer>> {
    if review.then.is_empty() {
        Ok(None)
    } else {
        task_creation_capture(bus, command, snapshot).map(Some)
    }
}

struct PreparedReview {
    snapshot: ValidatedSnapshot,
    entry: Entry,
    transition: board::Transition,
    at: Instant,
    lane: wire::CanonicalUuid,
    source: body::SourceBoundary,
    record: ReviewRecord,
    statement: TaskHead,
    origin: CreationCause,
    git: Option<body::GitBoundary>,
    creation: Option<TaskCreationCapture>,
}

/// Refuse a clean verdict while any dependency of the task is unsettled.
///
/// A finding's corrections become dependencies, so no clean verdict counts
/// toward a review that a correction has yet to change.
fn refuse_unsettled_corrections(
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
) -> Result<(), Outcome<CommandAnswer>> {
    let satisfied = board::resolved_tasks(&snapshot.entries);
    if board::dependencies_satisfied(entry, &satisfied) {
        return Ok(());
    }
    Err(refusal(
        "review::corrections-unsettled",
        Some(entry.handle().to_string()),
        format!(
            "a dependency of {} is unsettled; record a clean verdict once every correction it \
             waits on has settled, or record a finding now",
            entry.handle(),
        ),
    ))
}

/// Refuse a clean verdict from a review claim won before a correction settled.
///
/// A correction that settled after the claim was won changed the revision the
/// review judges after the claim took stock of it. No generation moved, so
/// the holder frees the claim and wins the review again to inspect the
/// corrected revision; a finding on the earlier revision stays recordable.
fn refuse_stale_revision(
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    actor: &Actor,
) -> Result<(), Outcome<CommandAnswer>> {
    let claimed = entry
        .claim_of(actor)
        .expect("an admitted review claim has a head")
        .claimed();
    let settled = entry
        .statement()
        .dependencies
        .iter()
        .filter_map(|dependency| task_heads_by_stamp(&snapshot.answer, dependency).ok())
        .filter(|heads| heads.workflow.position > claimed)
        .max_by_key(|heads| heads.workflow.position);
    let Some(correction) = settled else {
        return Ok(());
    };
    let statement = &correction.statement.body;
    Err(refusal(
        "review::revision-stale",
        Some(format!(
            "correction {} settled at position {} after this review claim was won at position {claimed}",
            TaskHandle::for_topic(&statement.topic.path, &statement.born),
            correction.workflow.position,
        )),
        format!(
            "a correction of {} settled after this claim took stock of it; free the claim and win the review again with task next to inspect the corrected revision, or record a finding",
            entry.handle(),
        ),
    ))
}

fn prepare_review(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    review: &TaskReview,
    repository: Option<Host<'_>>,
) -> Result<PreparedReview, Outcome<CommandAnswer>> {
    let roots = task_review_roots(review);
    let snapshot = validated_working_snapshot_for_tasks(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        &roots,
    )?;
    if let Some(entry) = task_entry(&snapshot.entries, &review.task)
        && entry.is_draft()
    {
        return Err(refusal(
            "task::phase-rule",
            Some(standing_detail(entry)),
            format!(
                "a draft never enters review; advance it with spice task file {} first",
                entry.handle(),
            ),
        ));
    }
    let (entry, transition, at, lane) = admit_review(&snapshot, command, review)?;
    let source = bus
        .source_boundary(&lane)
        .map_err(|error| store_fault("review::boundary-unreadable", error.to_string()))?;
    let (verdict, finding, note) = review_verdict(review)?;
    let statement = match verdict {
        body::Verdict::Clean => {
            refuse_taste_hold(entry)?;
            admit_completion(&snapshot, entry, command, None)?;
            refuse_unsettled_corrections(&snapshot, entry)?;
            refuse_stale_revision(&snapshot, entry, command.actor())?;
            entry.statement().clone()
        }
        body::Verdict::Finding => correction_statement(&snapshot, command, entry, review)?,
    };
    let record = ReviewRecord {
        verdict,
        finding,
        note,
        followups: review_followups(review, verdict, &statement),
    };
    let parent = wire::TaskProvenance::Task(entry.born().clone());
    let origin = resolve_creation_origin(None, command.actor(), &snapshot, Some(parent), false)?;
    let git = publish_task_boundary(repository, &snapshot, command, entry)?;
    let creation = review_followup_creation(bus, command, &snapshot, review)?;
    Ok(PreparedReview {
        entry: entry.clone(),
        snapshot,
        transition,
        at,
        lane,
        source,
        record,
        statement,
        origin,
        git,
        creation,
    })
}

pub(super) fn task_review(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    review: &TaskReview,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    task_review_with(bus, command, review, repository, || {})
}

pub(super) fn task_review_with(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    review: &TaskReview,
    repository: Option<Host<'_>>,
    after_validation: impl FnOnce(),
) -> Outcome<CommandAnswer> {
    let prepared = match prepare_review(bus, command, review, repository) {
        Ok(prepared) => prepared,
        Err(outcome) => return outcome,
    };
    after_validation();
    commit_review(bus, command, review, repository, prepared)
}

fn commit_review(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    review: &TaskReview,
    repository: Option<Host<'_>>,
    prepared: PreparedReview,
) -> Outcome<CommandAnswer> {
    let PreparedReview {
        snapshot,
        entry,
        transition,
        at,
        lane,
        source,
        record,
        statement,
        origin,
        git,
        creation,
    } = prepared;
    let mut write = JournalWrite::new(
        command.actor().clone(),
        origin.origin.clone(),
        at,
        Producer::CommandDoor,
    );
    guard(&mut write, command.expected().to_vec());
    snapshot.guard_ownership(&mut write);
    if let Some(creation) = &creation {
        creation.guard(&mut write);
    }
    let verdict = record.verdict;
    // A clean verdict in a shared round is one arrival; only the arrival that
    // completes the round moves the workflow.
    let arrival = match (verdict == body::Verdict::Clean && shared_round(&entry))
        .then(|| {
            let evidence = shared::ReviewEvidence {
                lane,
                validation: record.note.clone().unwrap_or_else(|| "clean".to_owned()),
                git: git.clone(),
                source: source.clone(),
            };
            shared::ReviewArrival::new(&snapshot, &entry, command.actor(), at, evidence)
        })
        .transpose()
    {
        Ok(arrival) => arrival,
        Err(outcome) => return outcome,
    };
    let closes = arrival.as_ref().is_none_or(shared::ReviewArrival::closes);
    let workflow = if closes {
        review_workflow(&entry, &transition, verdict)
    } else {
        entry.workflow().clone()
    };
    if let Err(outcome) = preparation::release::completed_plan(
        bus, command, repository, &snapshot, &entry, &workflow, &mut write,
    ) {
        return outcome;
    }
    let completion = completion_status(&workflow);
    let round = arrival.as_ref().map(|_| entry.completion().round);
    let reviewed = match append_reviewed_fact(
        &mut write, command, &entry, record, completion, round, git, source,
    ) {
        Ok(reviewed) => reviewed,
        Err(outcome) => return outcome,
    };
    let post = freed_projection(&entry, command.actor(), statement.clone(), workflow.clone());
    let next = next_action(&snapshot, &post);
    let reviewed_task = match task_heads(&snapshot.answer, &review.task) {
        Ok(reviewed_task) => reviewed_task,
        Err(outcome) => return outcome,
    };
    let created = match record_followups(
        &mut write,
        &review.then,
        &FollowupContext {
            bus,
            command,
            snapshot: &snapshot,
            reviewed_task,
            reviewed,
            origin: &origin,
            creation: creation.as_ref(),
            repository,
        },
    ) {
        Ok(created) => created,
        Err(outcome) => return outcome,
    };
    let state_cause = match append_review_statement(&mut write, &entry, statement, reviewed) {
        Ok(cause) => cause,
        Err(outcome) => return outcome,
    };
    let status = if closes {
        workflow.status.as_str().to_owned()
    } else {
        "arrived".to_owned()
    };
    if let Some(arrival) = arrival {
        if let Err(outcome) =
            arrival.append(&mut write, &entry, command.actor(), reviewed, next.clone())
        {
            return outcome;
        }
    } else {
        write_review_workflow(&mut write, &entry, verdict, reviewed, state_cause, workflow);
    }
    if let Err(outcome) = free_claim(&mut write, &entry, command.actor(), state_cause, None) {
        return outcome;
    }
    let mut answer = review_answer(&snapshot, &post, status, next);
    answer.created = created;
    finish_review(bus, command, review, write, answer)
}

fn finish_review(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    review: &TaskReview,
    mut write: JournalWrite,
    answer: wire::intent::TaskAnswer,
) -> Outcome<CommandAnswer> {
    if let Err(error) = write.command_result(
        request(command, review.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("review::receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::Task(Box::new(answer)))
}

/// Write the verdict's workflow head; a finding also opens a fresh round, so
/// clean arrivals recorded before it never count after it.
fn write_review_workflow(
    write: &mut JournalWrite,
    entry: &Entry,
    verdict: body::Verdict,
    reviewed: PendingFact,
    state_cause: PendingFact,
    workflow: wire::WorkflowHead,
) {
    if verdict == body::Verdict::Finding && !entry.completion().arrivals.is_empty() {
        let mut completion = entry.completion().clone();
        completion.arrivals.clear();
        write.completion_head(entry.born().clone(), reviewed, move |fact| {
            completion.round = fact;
            completion
        });
    }
    write.workflow_head(entry.born().clone(), state_cause, move |_| workflow);
}

#[expect(
    clippy::too_many_arguments,
    reason = "one review fact carries each recorded field"
)]
fn append_reviewed_fact(
    write: &mut JournalWrite,
    command: &wire::AdmittedCommand,
    entry: &Entry,
    record: ReviewRecord,
    status: body::TaskCompletionStatus,
    round: Option<wire::Seq>,
    git: Option<body::GitBoundary>,
    source: body::SourceBoundary,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    write
        .fact(
            body::TaskReviewed {
                task: entry.born().clone(),
                reviewer: command.actor().clone(),
                verdict: record.verdict,
                status,
                finding: record.finding,
                note: record.note,
                followups: record.followups,
                round,
                git,
                source,
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("review::build", error.to_string()))
}

fn review_workflow(
    entry: &Entry,
    transition: &board::Transition,
    verdict: body::Verdict,
) -> WorkflowHead {
    match verdict {
        body::Verdict::Clean => transition.clone().reviewed().next().clone(),
        body::Verdict::Finding => {
            let mut workflow = entry.workflow().clone();
            workflow.status = wire::WorkflowStatus::pending();
            workflow
        }
    }
}

fn append_review_statement(
    write: &mut JournalWrite,
    entry: &Entry,
    statement: TaskHead,
    reviewed: PendingFact,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    if &statement == entry.statement() {
        return Ok(reviewed);
    }
    let revision = u32::try_from(entry.statement().revision.get())
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let dependencies = (statement.dependencies != entry.statement().dependencies)
        .then(|| statement.dependencies.clone());
    let edited = write
        .fact(
            body::TaskEdited {
                intent: None,
                task: entry.born().clone(),
                revision,
                dependencies,
                flow: None,
            },
            FactCause::Pending(reviewed),
        )
        .map_err(|error| store_fault("review::dependency-build", error.to_string()))?;
    write.task_head(edited, move |position| TaskHead {
        revision: position,
        ..statement
    });
    Ok(edited)
}

/// The review answer naming `post`, the task as the verdict leaves it.
fn review_answer(
    snapshot: &ValidatedSnapshot,
    post: &Entry,
    status: String,
    next: Option<Box<str>>,
) -> wire::intent::TaskAnswer {
    wire::intent::TaskAnswer {
        diagnostic: None,
        duplicates: Vec::new(),
        duplicates_omitted: 0,
        placement: None,
        created: Default::default(),
        taste: None,
        completion_contract: None,
        task: post.handle().clone(),
        ownership: Some(snapshot.ownership(post)),
        status,
        completion: None,
        claim: None,
        closed: Vec::new(),
        next,
    }
}
