use super::*;

/// One decided claim and the board snapshot it was decided against.
///
/// The allocator's re-read guard exists precisely because deciding a claim and
/// writing it down are separated in time. Naming that separation is what makes
/// the hazard expressible: everything `commit_claim` writes, the guard's
/// observation included, is derived from the one snapshot `claim_plan` read,
/// never from a second read taken later. This is the shape `DocPlan` already
/// runs, where the plan carries the snapshot its decision was made against and
/// the later steps consume it.
pub(in crate::board_command) struct ClaimPlan<'a> {
    snapshot: ValidatedSnapshot,
    pub(in crate::board_command) selected: Entry,
    /// The selected row is the actor's own unsubmitted draft, which this
    /// capture submits in the same write.
    submit: bool,
    grant: ClaimGrant<'a>,
    next: Option<Box<str>>,
    room_context: Option<Box<str>>,
    possession: Possession,
}

pub(in crate::board_command) fn claim_win(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    win: &ClaimWin,
    repository: Option<Host<'_>>,
    allocation: &board::NextConfig,
    profiles: OccupantProfiles<'_>,
    resources: Option<&resources::ResourceClaims>,
) -> Outcome<CommandAnswer> {
    let plan = match claim_plan_with_resources(
        bus, command, win, repository, allocation, profiles, resources,
    ) {
        Ok(plan) => plan,
        Err(outcome) => return outcome,
    };
    let reservation = match resources
        .map(|resources| resources.reserve(&plan.selected, command.actor(), &plan.snapshot.entries))
        .transpose()
    {
        Ok(reservation) => reservation,
        Err(outcome) => return outcome,
    };
    let outcome = commit_claim(bus, command, win, plan);
    if matches!(outcome, Outcome::Reply(_))
        && let Some(reservation) = reservation
        && let Err(error) = reservation.commit()
    {
        return error;
    }
    outcome
}

/// Decide one claim against a single board read.
///
/// Every refusal a caller could have avoided is raised here, in order, and the
/// caller sees the first one only.
#[cfg(test)]
pub(in crate::board_command) fn claim_plan<'a>(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    win: &ClaimWin,
    repository: Option<Host<'a>>,
    allocation: &board::NextConfig,
    profiles: OccupantProfiles<'_>,
) -> Result<ClaimPlan<'a>, Outcome<CommandAnswer>> {
    claim_plan_with_resources(bus, command, win, repository, allocation, profiles, None)
}

fn claim_plan_with_resources<'a>(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    win: &ClaimWin,
    repository: Option<Host<'a>>,
    allocation: &board::NextConfig,
    profiles: OccupantProfiles<'_>,
    resources: Option<&resources::ResourceClaims>,
) -> Result<ClaimPlan<'a>, Outcome<CommandAnswer>> {
    let possession = possession_rule(win)?;
    // A named claim decides against its exact witnessed task even when that
    // task is settled, so its refusal names ineligibility rather than absence.
    let roots = command
        .expected()
        .iter()
        .filter_map(|witness| match witness {
            Witness::Task { task, .. } => Some(task.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let snapshot = validated_working_snapshot_for_tasks(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        &roots,
    )?;
    let mut environment = snapshot.answer.environment();
    if let Some(resources) = resources {
        environment = resources.observe(environment, &snapshot.entries)?;
    }
    let (selected, submit) = if possession.named() {
        named_claim_selection(&snapshot, command, possession, &environment)?
    } else {
        let selected = allocator_claim_selection(
            &snapshot,
            command,
            allocation,
            profiles,
            &environment,
            &snapshot.owners,
        )?;
        (selected.clone(), false)
    };
    // Named capture may possess a planning deliverable, but a recovery-bound
    // capture adopts implementation, so it refuses planning before Git work.
    if matches!(possession, Possession::Recover | Possession::RecoverSteal) {
        planning::admit_capture(&selected)?;
    }
    refuse_if_actor_already_claims(&snapshot.entries, command.actor())?;
    let room_context = claim_room(bus, repository, &selected)?;
    let grant = claim_grant(bus, command, repository, &snapshot, &selected, possession)?;
    let next = Some(claim_activation_next_action(&selected));
    Ok(ClaimPlan {
        snapshot,
        selected,
        submit,
        grant,
        next,
        room_context,
        possession,
    })
}

/// `selected`'s room conversation for its claim answer, read through the
/// repository's tape and artifacts; a repository-less dispatch owns neither.
pub(super) fn claim_room(
    bus: &Bus,
    repository: Option<Host<'_>>,
    selected: &Entry,
) -> Result<Option<Box<str>>, Outcome<CommandAnswer>> {
    repository
        .map(|repository| claim_room_context(bus, repository, selected.handle(), selected.born()))
        .transpose()
        .map(Option::flatten)
}

fn refuse_if_actor_already_claims(
    entries: &[Entry],
    actor: &Actor,
) -> Result<(), Outcome<CommandAnswer>> {
    let mut active = entries
        .iter()
        .filter(|entry| entry.claim_of(actor).is_some());
    let Some(first) = active.next() else {
        return Ok(());
    };
    if active.next().is_some() {
        return Err(protocol_fault(
            "claim::active-ambiguous",
            "one actor holds more than one active claim",
        ));
    }
    Err(refusal(
        "claim::active-exists",
        Some(first.handle().to_string()),
        format!(
            "finish task {} before claiming another task, or run spice task free {}",
            first.handle(),
            first.handle(),
        ),
    ))
}

/// What a claim settles before the journal opens.
///
/// These resolve together because they refuse together, and the order is the
/// contract rather than an accident of writing: each step here can refuse, and
/// the caller sees the first refusal only, so a seat that cannot be read is
/// reported before a boundary that cannot be captured.
pub(in crate::board_command) struct ClaimGrant<'a> {
    lane: wire::CanonicalUuid,
    occupant: wire::CanonicalUuid,
    at: Instant,
    expiry: Instant,
    maximum: Instant,
    git: Option<GitBoundary>,
    source: body::SourceBoundary,
    prepared_git: Option<spice::task_git::ClaimPreparation<'a>>,
}

/// Resolve the seat, the deadlines and the two boundaries one claim records.
pub(in crate::board_command) fn claim_grant<'a>(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'a>>,
    snapshot: &ValidatedSnapshot,
    selected: &Entry,
    possession: Possession,
) -> Result<ClaimGrant<'a>, Outcome<CommandAnswer>> {
    let (lane, occupant) = claim_residency(command)?;
    let (at, expiry, maximum) = claim_deadlines()?;
    let git = claim_git_boundary(repository, snapshot, command, selected, possession)?;
    let source = claim_source_boundary(bus, &lane)?;
    Ok(ClaimGrant {
        lane,
        occupant,
        at,
        expiry,
        maximum,
        git: git.as_ref().map(|git| git.boundary.clone()),
        prepared_git: git.and_then(|git| git.prepared),
        source,
    })
}
#[derive(Clone)]
struct TrustStanding {
    requirements: wire::EnvironmentRequirements,
    blocked: bool,
}

impl TrustStanding {
    fn observe(
        requirements: &wire::EnvironmentRequirements,
        environment: &wire::Environment,
    ) -> Option<Self> {
        requirements
            .as_slice()
            .iter()
            .any(|requirement| {
                matches!(requirement, wire::EnvironmentRequirement::TrustGrant { .. })
            })
            .then(|| Self {
                requirements: requirements.clone(),
                blocked: requirements.has_unsatisfied_trust_grant(environment),
            })
    }

    fn guard(&self, write: &mut JournalWrite) {
        let requirements = self.requirements.clone();
        let blocked = self.blocked;
        write.guard(JournalGuard::new(move |read| {
            let environment = read.environment(&requirements)?;
            if requirements.has_unsatisfied_trust_grant(&environment) == blocked {
                Ok(())
            } else {
                Err(GuardError::refused("claim::trust-standing-moved"))
            }
        }));
    }
}

/// Claim state prepared for a task made ready in the same transaction.
pub(super) struct PreparedClaim<'a> {
    grant: ClaimGrant<'a>,
    prepared_git: Option<spice::task_git::ClaimPreparation<'a>>,
    recovery: Option<String>,
    trust_standing: Option<TrustStanding>,
}

/// Validate and prepare an atomic mutation-and-claim before the journal opens.
///
/// `creating` says the same write creates `selected`, so an ineligibility
/// refusal leaves no task behind and must not name its handle as a repair.
pub(super) fn prepare_capture_claim<'a>(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'a>>,
    snapshot: &ValidatedSnapshot,
    selected: &Entry,
    creating: bool,
    environment: Option<wire::Environment>,
) -> Result<PreparedClaim<'a>, Outcome<CommandAnswer>> {
    let claim_rule = command.expected().iter().find_map(|witness| match witness {
        Witness::Eligibility { rule }
            if matches!(rule.as_str(), "capture" | "recover" | CAPTURE_WORK_RULE) =>
        {
            Some(rule.as_str())
        }
        _ => None,
    });
    let Some(claim_rule) = claim_rule else {
        return Err(protocol_fault(
            "claim::capture-eligibility-witness",
            "atomic capture has no capture or recovery eligibility witness",
        ));
    };
    let captures_work = claim_rule == CAPTURE_WORK_RULE;
    // Every caller (add, wake, filing) shares this door, so adopting loose
    // commits or retained recovery into a planning deliverable refuses here
    // before any claim, binding, or fact.
    if captures_work || claim_rule == "recover" {
        planning::admit_capture(selected)?;
    }
    refuse_if_actor_already_claims(&snapshot.entries, command.actor())?;
    let satisfied = board::resolved_tasks(&snapshot.entries);
    refuse_held_design(selected, &satisfied)?;
    let trust_standing = environment.as_ref().and_then(|environment| {
        TrustStanding::observe(&selected.statement().requirements, environment)
    });
    let environment = environment.unwrap_or_else(|| snapshot.answer.environment());
    if !board::capturable_entry(&satisfied, selected, command.actor(), &environment) {
        return Err(if creating {
            new_claim_ineligible()
        } else {
            named_claim_ineligible(selected, false)
        });
    }
    // Ordinary add-and-claim records the clean allocator boundary. Compound
    // capture and dirty adoption both retain loose commits; dirty adoption also
    // binds the synchronized quarantine. Every mode commits with task creation
    // under one journal guard.
    let possession = if captures_work || claim_rule == "recover" {
        Possession::Capture
    } else {
        Possession::Next
    };
    let mut grant = claim_grant(bus, command, repository, snapshot, selected, possession)?;
    let recovery = if claim_rule == "recover" {
        let resident = repository.ok_or_else(|| {
            protocol_fault(
                "git::recovery-resident-missing",
                "atomic dirty-handoff recovery requires the resident repository",
            )
        })?;
        let lane = task_repository(snapshot, command, &resident)?;
        let recovery = spice::task_git::pending_recovery(&lane)
            .map_err(task_git_outcome)?
            .ok_or_else(|| {
                refusal(
                    "git::recovery-transaction-missing",
                    None,
                    "start atomic adoption through `spice task capture --title TITLE --topic TOPIC`",
                )
            })?;
        let boundary = grant.git.clone().ok_or_else(|| {
            protocol_fault(
                "git::recovery-boundary-missing",
                "atomic dirty-handoff recovery prepared no Git boundary",
            )
        })?;
        spice::task_git::bind_recovery_claim(
            &lane,
            &recovery,
            spice::task_git::RecoveryClaim {
                actor: command.actor().clone(),
                task: selected.born().clone(),
                handle: Some(selected.handle().clone()),
                fresh: true,
                statement: wire::Seq::new(1).expect("fresh recovery placeholder is non-zero"),
                workflow: wire::Seq::new(1).expect("fresh recovery placeholder is non-zero"),
                phase: selected.workflow().phase,
                claim: None,
                lane: grant.lane,
                occupant: grant.occupant,
                epoch: command.epoch(),
                boundary,
            },
        )
        .map_err(task_git_outcome)?;
        Some(recovery.transaction().to_owned())
    } else {
        None
    };
    let prepared_git = grant.prepared_git.take();
    Ok(PreparedClaim {
        grant,
        prepared_git,
        recovery,
        trust_standing,
    })
}

/// Prepare one ordinary claim for a task another participant already holds.
///
/// `require_capacity` distinguishes a direct join from an accepted invitation:
/// the latter grows a full target in the same write that appends this claim.
pub(super) fn prepare_join_claim<'a>(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'a>>,
    snapshot: &ValidatedSnapshot,
    selected: &Entry,
    require_capacity: bool,
    resources: Option<&'a resources::ResourceClaims>,
) -> Result<
    (
        PreparedClaim<'a>,
        Option<resources::ResourceReservation<'a>>,
    ),
    Outcome<CommandAnswer>,
> {
    if !command
        .expected()
        .iter()
        .any(|witness| matches!(witness, Witness::Eligibility { rule } if rule.as_str() == "join"))
    {
        return Err(protocol_fault(
            "claim::join-eligibility-witness",
            "task join has no join eligibility witness",
        ));
    }
    refuse_if_actor_already_claims(&snapshot.entries, command.actor())?;
    let satisfied = board::resolved_tasks(&snapshot.entries);
    refuse_held_design(selected, &satisfied)?;
    if selected.active_claims().next().is_none() {
        return Err(refusal(
            "task::join-unheld",
            Some(selected.handle().to_string()),
            "claim unheld work with spice task capture HANDLE",
        ));
    }
    if require_capacity && !selected.has_capacity() {
        return Err(refusal(
            "task::join-full",
            Some(selected.handle().to_string()),
            "join with a live invitation when the holder wants another participant",
        ));
    }
    let reservation = resources
        .map(|resources| resources.reserve(selected, command.actor(), &snapshot.entries))
        .transpose()?;
    let mut environment = snapshot.answer.environment();
    if reservation.is_some() {
        // The reservation holds these shards for this task, including shards
        // already retained for its other participants. Free pool capacity is
        // not a second requirement for joining their task.
        for (name, count) in selected.statement().resources.iter() {
            environment = environment.with_resource_pool(name.clone(), count);
        }
    }
    if !board::joinable_entry(&satisfied, selected, command.actor(), &environment) {
        return Err(refusal(
            "task::join-ineligible",
            Some(selected.handle().to_string()),
            "join a ready task whose prerequisites, environment, and participation state admit this lane",
        ));
    }
    let mut grant = claim_grant(
        bus,
        command,
        repository,
        snapshot,
        selected,
        Possession::Join,
    )?;
    let prepared_git = grant.prepared_git.take();
    Ok((
        PreparedClaim {
            grant,
            prepared_git,
            recovery: None,
            trust_standing: None,
        },
        reservation,
    ))
}

impl<'a> PreparedClaim<'a> {
    pub(super) fn at(&self) -> Instant {
        self.grant.at
    }

    pub(super) fn guard(
        &self,
        write: &mut JournalWrite,
        snapshot: &ValidatedSnapshot,
        command: &wire::AdmittedCommand,
    ) {
        snapshot.guard_ownership(write);
        if let Some(trust_standing) = &self.trust_standing {
            trust_standing.guard(write);
        }
        guard_capture(
            write,
            command.expected().to_vec(),
            snapshot.cursor,
            command.actor().clone(),
        );
    }

    pub(super) fn answer(
        &self,
        command: &wire::AdmittedCommand,
        snapshot: &ValidatedSnapshot,
        selected: &Entry,
        origin_context: Option<Box<str>>,
        room_context: Option<Box<str>>,
    ) -> wire::intent::ClaimAnswer {
        claim_answer(
            command,
            selected,
            snapshot,
            ClaimReading {
                next: Some(claim_activation_next_action(selected)),
                origin_context,
                room_context,
            },
            self.prepared_git.as_ref(),
            false,
        )
    }

    pub(super) fn append(
        self,
        write: &mut JournalWrite,
        command: &wire::AdmittedCommand,
        selected: &Entry,
    ) -> Result<Option<spice::task_git::ClaimPreparation<'a>>, Outcome<CommandAnswer>> {
        self.append_with_cause(write, command, selected, None)
    }

    pub(super) fn append_with_cause(
        self,
        write: &mut JournalWrite,
        command: &wire::AdmittedCommand,
        selected: &Entry,
        cause: Option<PendingFact>,
    ) -> Result<Option<spice::task_git::ClaimPreparation<'a>>, Outcome<CommandAnswer>> {
        let Self {
            grant,
            prepared_git,
            recovery,
            trust_standing: _,
        } = self;
        if let Err(outcome) = mint_claim(
            write,
            board::allocate::Mint {
                task: selected.born(),
                actor: command.actor(),
                workflow: selected.workflow(),
                topic: selected.topic(),
                lane: grant.lane,
                occupant: grant.occupant,
                epoch: command.epoch(),
                at: grant.at,
                expiry: grant.expiry,
                maximum: grant.maximum,
                git: grant.git,
                recovery,
                source: grant.source,
                cause,
            },
        ) {
            if let Some(prepared) = prepared_git
                && let Err(error) = prepared.rollback()
            {
                return Err(task_git_outcome(error));
            }
            return Err(outcome);
        }
        Ok(prepared_git)
    }

    pub(super) fn rollback(self, outcome: Outcome<CommandAnswer>) -> Outcome<CommandAnswer> {
        match self.prepared_git {
            Some(prepared) => prepared
                .rollback()
                .map_or_else(task_git_outcome, |()| outcome),
            None => outcome,
        }
    }
}

/// The texts a claimant reads with its answer: the command it runs next and
/// the steering and room context each derived on its own.
struct ClaimReading {
    next: Option<Box<str>>,
    origin_context: Option<Box<str>>,
    room_context: Option<Box<str>>,
}

fn claim_answer(
    command: &wire::AdmittedCommand,
    selected: &Entry,
    snapshot: &ValidatedSnapshot,
    reading: ClaimReading,
    prepared: Option<&spice::task_git::ClaimPreparation<'_>>,
    nothing_stolen: bool,
) -> wire::intent::ClaimAnswer {
    let ClaimReading {
        next,
        origin_context,
        room_context,
    } = reading;
    wire::intent::ClaimAnswer {
        ownership: Some(snapshot.ownership(selected)),
        task: selected.handle().clone(),
        statement: Some(Box::new(wire::intent::ClaimStatement {
            title: selected.statement().title.clone(),
            pitch: selected.statement().pitch.clone(),
            description: selected.statement().description.clone(),
            term: statement_items_with_handles(&selected.statement().term),
            design_research: snapshot
                .answer
                .tasks
                .iter()
                .find(|task| task.statement.body.born == *selected.born())
                .and_then(BoardTask::design_research)
                .map(String::into_boxed_str),
            completion_contract: Some(
                completion_contract(
                    &selected.statement().term,
                    selected.born(),
                    &selected.statement().dependencies,
                    selected.statement().intent,
                    snapshot.entries.iter(),
                )
                .into_boxed_str(),
            ),
        })),
        holder: Some(command.actor().clone()),
        acknowledged: None,
        held: false,
        warnings: board::claim_overlaps(selected, &snapshot.entries)
            .into_iter()
            .map(|overlap| wire::intent::ClaimWarning {
                task: overlap.task,
                lane: overlap.lane,
                surface: overlap.surface,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        nothing_stolen,
        creation_context: Some(task_creation_context_output(&selected.statement().creation)),
        origin_context,
        room_context,
        next,
        phase: Some(claim_phase(
            selected,
            command.actor(),
            &snapshot.answer.addresses,
        )),
        transplant: prepared
            .and_then(spice::task_git::ClaimPreparation::transplant)
            .cloned(),
        parked: None,
        uncarried: prepared
            .and_then(|prepared| prepared.uncarried.as_deref())
            .map(Box::from),
    }
}

/// Write the decided claim: one origin, the guards it owes, its two heads, its
/// receipt.
///
/// Nothing here refuses on what the caller asked for; by this point the command
/// is already admitted and the plan already decided, so every failure below is
/// a fault rather than a refusal the caller could have avoided: a validated
/// birth stamp that will not form an origin is a protocol fault, and the
/// mint and the receipt are store faults. The public handle is answer text only.
///
/// The plan arrives whole rather than as loose arguments so the guard's
/// observation can only come from the snapshot the decision was made against.
pub(in crate::board_command) fn commit_claim(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    win: &ClaimWin,
    plan: ClaimPlan,
) -> Outcome<CommandAnswer> {
    let ClaimPlan {
        snapshot,
        selected,
        submit,
        grant,
        next,
        room_context,
        possession,
    } = plan;
    let ClaimGrant {
        lane,
        occupant,
        at,
        expiry,
        maximum,
        git,
        source,
        prepared_git,
    } = grant;
    let task = selected.handle().clone();
    let born = selected.born().clone();
    let origin = match task_origin(&born) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard_claim(&mut write, &snapshot, command, possession, &task);
    snapshot.guard_ownership(&mut write);
    let stealing = matches!(possession, Possession::Steal | Possession::RecoverSteal);
    let displaced = stealing
        .then(|| selected.displaced_by_steal())
        .flatten()
        .map(|held| held.claim().clone());
    let nothing_stolen = stealing && displaced.is_none();
    let won = match submit
        .then(|| activation::append_submitted(&mut write, &selected))
        .transpose()
        .and_then(|submitted| {
            mint_claim(
                &mut write,
                board::allocate::Mint {
                    task: &born,
                    actor: command.actor(),
                    workflow: selected.workflow(),
                    topic: selected.topic(),
                    lane,
                    occupant,
                    epoch: command.epoch(),
                    at,
                    expiry,
                    maximum,
                    git,
                    recovery: win.recovery.clone(),
                    source,
                    cause: submitted,
                },
            )
        }) {
        Ok(won) => won,
        Err(outcome) => {
            if let Some(prepared) = prepared_git
                && let Err(error) = prepared.rollback()
            {
                return task_git_outcome(error);
            }
            return outcome;
        }
    };
    end_displaced(&mut write, &born, won, displaced);
    let origin_context = match task_steering_origin_output(bus, &snapshot.facts, &selected) {
        Ok(context) => context,
        Err(outcome) => {
            if let Some(prepared) = prepared_git
                && let Err(error) = prepared.rollback()
            {
                return task_git_outcome(error);
            }
            return outcome;
        }
    };
    let answer = claim_answer(
        command,
        &selected,
        &snapshot,
        ClaimReading {
            next,
            origin_context,
            room_context,
        },
        prepared_git.as_ref(),
        nothing_stolen,
    );
    if let Err(error) = write.command_result(
        request(command, win.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        if let Some(prepared) = prepared_git
            && let Err(rollback) = prepared.rollback()
        {
            return task_git_outcome(rollback);
        }
        return store_fault("claim::win-receipt", error.to_string());
    }
    let body = CommandAnswer::Claim(Box::new(answer));
    if let Some(prepared) = prepared_git {
        committed_with_task_git(bus, write, body, PreparedTaskGit::Claim(prepared))
    } else {
        committed(bus, write, body)
    }
}

/// The exact command a lane runs next on the task it just won.
///
/// A task held for taste review is held on its statement, so the admitted
/// next action is the rewording rather than another read.
pub(in crate::board_command) fn claim_activation_next_action(selected: &Entry) -> Box<str> {
    if selected.statement().taste.is_some() {
        return format!("spice task modify {} --title \"...\"", selected.handle()).into_boxed_str();
    }
    format!("spice task show {} --full", selected.handle()).into_boxed_str()
}

/// The phase a lane just won, said where the lane reads the assignment.
///
/// The handle and the next command say nothing about what kind of work this
/// is, and the phase decides whether the deliverable is board state or
/// repository files. A lane that has to read the task again to learn that has
/// already started the wrong work. `selected` is the row as it stood before
/// this win, so the lane's own review and a row it freed still read as such,
/// and a freed row carries the reminder of how to say what it waits on.
fn claim_phase(
    selected: &Entry,
    actor: &wire::Actor,
    addresses: &std::collections::BTreeMap<wire::CanonicalUuid, wire::SeatAddress>,
) -> wire::intent::ClaimPhase {
    let workflow = selected.workflow();
    let target = selected.completion().target(&workflow.phase);
    let deliverable = if board::is_independent_handoff(selected, actor) {
        board::phase::SELF_HANDOFF_DELIVERABLE
    } else {
        board::phase::deliverable(&workflow.phase)
    };
    let deliverable = if board::is_released_by(selected, actor) {
        Some(
            format!(
                "{deliverable}; {}",
                board::phase::released_reminder(selected)
            )
            .into_boxed_str(),
        )
    } else {
        Some(Box::from(deliverable))
    };
    let participant = |actor: &wire::Actor, lane| wire::intent::ClaimParticipant {
        actor: actor.clone(),
        lane,
        address: addresses.get(&lane).and_then(wire::SeatAddress::address),
    };
    wire::intent::ClaimPhase {
        name: workflow.phase,
        deliverable,
        target: target.get(),
        partners: selected
            .active_claims()
            .map(|held| participant(&held.claim().actor, held.claim().lane))
            .collect(),
        arrivals: selected
            .completion()
            .arrivals
            .iter()
            .map(|arrival| participant(&arrival.actor, arrival.lane))
            .collect(),
    }
}

/// Which possession law one claim win asks the board to apply.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::board_command) enum Possession {
    /// The allocator chooses among tasks nobody holds.
    Next,
    /// The caller names a task nobody holds.
    Capture,
    /// The caller names a task and takes it from its current holder.
    Steal,
    /// The caller joins a task another participant already holds.
    Join,
    /// The caller names one unclaimed task for a sealed dirty adoption.
    Recover,
    /// The caller replaces a current holder with a sealed dirty adoption.
    RecoverSteal,
}
impl Possession {
    /// Whether the caller names the exact task rather than running allocation.
    const fn named(self) -> bool {
        !matches!(self, Self::Next)
    }
}

fn possession_rule(win: &ClaimWin) -> Result<Possession, Outcome<CommandAnswer>> {
    match (win.rule.as_str(), win.recovery.as_deref()) {
        ("next", None) => Ok(Possession::Next),
        ("capture", None) => Ok(Possession::Capture),
        ("steal", None) => Ok(Possession::Steal),
        (rule @ ("recover" | RECOVER_STEAL_RULE), Some(transaction))
            if spice::task_git::Recovery::is_transaction(transaction) =>
        {
            Ok(if rule == RECOVER_STEAL_RULE {
                Possession::RecoverSteal
            } else {
                Possession::Recover
            })
        }
        ("recover" | RECOVER_STEAL_RULE, None) => Err(refusal(
            "git::recovery-transaction-missing",
            None,
            "start sealed adoption through `spice task capture HANDLE`",
        )),
        ("recover" | RECOVER_STEAL_RULE, Some(transaction)) => Err(refusal(
            "git::recovery-transaction-invalid",
            Some(transaction.to_owned()),
            "use the content-addressed transaction sealed by task capture",
        )),
        ("next" | "capture" | "steal", Some(transaction)) => Err(refusal(
            "git::recovery-rule-mismatch",
            Some(transaction.to_owned()),
            "use recovery identity only with a sealed capture possession rule",
        )),
        _ => Err(refusal(
            "board::allocator-rule",
            Some(win.rule.to_string()),
            "use the next, capture, steal, or sealed-capture possession rule",
        )),
    }
}

/// The allocator's choice at claim time, ranked exactly as the `task next`
/// read ranked it: same policy and the same occupant match, so a witness the
/// read handed out is refused only when the board itself moved.
fn allocator_claim_selection<'a>(
    snapshot: &'a ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    allocation: &board::NextConfig,
    profiles: OccupantProfiles<'_>,
    environment: &wire::Environment,
    owners: &std::sync::Arc<board::OwnershipSnapshot>,
) -> Result<&'a Entry, Outcome<CommandAnswer>> {
    let residency = &snapshot.answer.residency.body;
    let team = actor_subscription(residency, command.actor())?;
    let subscription = owners.for_team(team.team);
    let now = current_instant()?;
    let matcher = OccupantMatch::open(profiles, residency, command.actor(), &snapshot.entries)?;
    let match_score = |entry: &Entry| matcher.as_ref().map_or(0.0, |matcher| matcher.score(entry));
    let satisfied = board::resolved_tasks(&snapshot.entries);
    let selected = board::select_with_satisfied_at(
        &snapshot.entries,
        board::Claimant {
            actor: command.actor(),
            subscription: &subscription,
            environment,
            match_score: matcher
                .as_ref()
                .map(|_| &match_score as &dyn Fn(&Entry) -> f64),
        },
        *allocation,
        &satisfied,
        now,
    )
    .map(|selected| selected.entry())
    .ok_or_else(|| {
        refusal(
            "board::allocator-empty",
            None,
            concat!("add or wake eligible work before ", "requesting task next",),
        )
    })?;
    if let Some(until) = independent_handoff_awaits_peer(
        &snapshot.entries,
        &satisfied,
        selected,
        command.actor(),
        (residency, owners),
        environment,
        now,
    )
    .map_err(|fault| protocol_fault("board::peer-subscription-ambiguous", fault.detail()))?
    {
        return Err(refusal(
            "board::allocator-awaits-peer",
            Some(format!("task={} until={until}", selected.handle())),
            "let an eligible peer take this independent handoff, or retry task next after the peer-first deadline",
        ));
    }
    let witnessed = command
        .expected()
        .iter()
        .find_map(|witness| match witness {
            Witness::Task { task, .. } => Some(task),
            _ => None,
        })
        .ok_or_else(|| {
            protocol_fault(
                "board::allocator-task-witness",
                "task next has no selected task witness",
            )
        })?;
    if witnessed != selected.born() {
        return Err(refusal(
            "board::witness-moved",
            Some(format!(
                "witnessed {witnessed}; selected {}",
                selected.handle()
            )),
            "read the current board subscription and retry task next",
        ));
    }
    Ok(selected)
}

/// The named task and whether this capture submits it: an explicit capture
/// of the actor's own pending, uncontrolled, unheld draft marks it submitted
/// in the claim's write, so it stands as the triage work it then claims.
fn named_claim_selection(
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    possession: Possession,
    environment: &wire::Environment,
) -> Result<(Entry, bool), Outcome<CommandAnswer>> {
    let task = command
        .expected()
        .iter()
        .find_map(|witness| match witness {
            Witness::Task { task, .. } => Some(task),
            _ => None,
        })
        .ok_or_else(|| {
            protocol_fault(
                "claim::capture-task-witness",
                "capture possession has no exact task witness",
            )
        })?;
    let entry = snapshot
        .entries
        .iter()
        .find(|entry| entry.born() == task)
        .ok_or_else(|| {
            refusal(
                "task::unknown",
                Some(task.to_string()),
                "capture into one current pending task",
            )
        })?;
    let submitted = if entry.is_draft() && !entry.is_submitted_draft() {
        if !own_draft_capture(snapshot, entry, possession)? {
            return Err(refusal(
                "task::draft-capture-refused",
                Some(standing_detail(entry)),
                format!(
                    "only a submitted draft or your own draft is claimable; capture your own \
                     with spice task capture {handle}, or advance and claim it with spice task \
                     file {handle} --claim",
                    handle = entry.handle(),
                ),
            ));
        }
        Some(activation::submitted_entry(entry)?)
    } else {
        None
    };
    let submit = submitted.is_some();
    let entry = submitted.as_ref().unwrap_or(entry);
    let satisfied = board::resolved_tasks(&snapshot.entries);
    refuse_held_design(entry, &satisfied)?;
    let (admitted, stealing) = match possession {
        Possession::Steal | Possession::RecoverSteal => (
            board::stealable_entry(&satisfied, entry, command.actor(), environment),
            true,
        ),
        Possession::Recover | Possession::Capture => (
            board::capturable_entry(&satisfied, entry, command.actor(), environment),
            false,
        ),
        Possession::Next => unreachable!("allocator possession returned above"),
        Possession::Join => unreachable!("task join admits through its own shared-task law"),
    };
    if admitted {
        Ok((entry.clone(), submit))
    } else {
        Err(named_claim_ineligible(entry, stealing))
    }
}

/// Whether a plain capture names a pending, uncontrolled, unheld draft created
/// on the actor's own origin thread, the same authorship default reads use.
fn own_draft_capture(
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    possession: Possession,
) -> Result<bool, Outcome<CommandAnswer>> {
    if possession != Possession::Capture
        || !entry.workflow().status.is_pending()
        || entry.is_controlled()
        || entry.is_held()
    {
        return Ok(false);
    }
    let row = visible_task(snapshot, entry.born())?;
    Ok(row.origin_thread == snapshot.answer.actor_route.origin_thread)
}

/// An open Design that awaits the operator's decision or waits on work it
/// released is held even from a named claim: capture, steal, and recovery name
/// the cycle and its next action. A Design still gathering input is claimable
/// by name, so the operator can force its pass before it steeps.
pub(in crate::board_command) fn refuse_held_design(
    entry: &Entry,
    satisfied: &board::ResolvedTasks,
) -> Result<(), Outcome<CommandAnswer>> {
    if let Some((cycle, next_action)) = board::design_hold(entry, satisfied) {
        return Err(refusal(
            "claim::design-held",
            Some(entry.handle().to_string()),
            format!("{} is {}; {next_action}", entry.handle(), cycle.label()),
        ));
    }
    Ok(())
}

/// The refusal of a named claim the board does not admit, naming the boundary
/// and the exact command for the route that does.
///
/// Capture takes an unclaimed task that is ready, or an undeferred open Design
/// still gathering input; steal takes the same from any holder. Deferred,
/// dependency-blocked, environment-held, hidden, and settled work refuses
/// both, as a held Design does through [`refuse_held_design`].
pub(in crate::board_command) fn named_claim_ineligible(
    entry: &Entry,
    stealing: bool,
) -> Outcome<CommandAnswer> {
    let handle = entry.handle();
    let (reason, remedy) = if stealing {
        (
            "claim::steal-ineligible",
            format!(
                "spice task capture {handle} --steal takes a ready task or an undeferred open \
                 Design gathering input, held or not; wake deferred work and settle its \
                 prerequisites first"
            ),
        )
    } else {
        (
            "claim::capture-ineligible",
            format!(
                "spice task capture {handle} claims an unclaimed ready task or an undeferred open \
                 Design gathering input; take a held one with spice task capture {handle} --steal, \
                 and wake deferred work and settle its prerequisites first"
            ),
        )
    };
    refusal(reason, Some(handle.to_string()), &remedy)
}

/// The refusal of a task add whose new task the board would not let it claim.
/// The whole write is refused, so no handle exists to name or capture yet.
fn new_claim_ineligible() -> Outcome<CommandAnswer> {
    refusal(
        "claim::capture-ineligible",
        None,
        "nothing was created: the new task's prerequisites or environment requirements are not \
         satisfied, so it cannot be claimed yet; add it without --claim, then claim it with \
         spice task capture HANDLE once it is ready",
    )
}

fn claim_residency(
    command: &wire::AdmittedCommand,
) -> Result<(wire::CanonicalUuid, wire::CanonicalUuid), Outcome<CommandAnswer>> {
    let seat = command
        .expected()
        .iter()
        .find_map(|witness| match witness {
            Witness::Residency {
                lane,
                occupant: Some(occupant),
                epoch,
            } if *epoch == command.epoch() => Some((*lane, *occupant)),
            _ => None,
        })
        .ok_or_else(|| {
            protocol_fault(
                "claim::residency-witness",
                "claim win has no current residency witness",
            )
        })?;
    if let Actor::Lane(actor) = command.actor()
        && *actor != seat.0
    {
        return Err(refusal(
            "claim::lane-mismatch",
            Some(seat.0.to_string()),
            concat!(
                "claim through the lane bound to the ",
                "authenticated capability",
            ),
        ));
    }
    Ok(seat)
}

fn claim_deadlines() -> Result<(Instant, Instant, Instant), Outcome<CommandAnswer>> {
    let at = current_instant()?;
    let (expiry, maximum) = claim_lease(at)?;
    Ok((at, expiry, maximum))
}

/// The ordinary claim lease granted at `at`. The resident advances both
/// horizons while the holder remains seated; no manual extension budget is
/// minted with the claim.
pub(in crate::board_command) fn claim_lease(
    at: Instant,
) -> Result<(Instant, Instant), Outcome<CommandAnswer>> {
    let expiry = instant_after(at, AUTOMATIC_MAXIMUM_SECONDS)?;
    Ok((expiry, expiry))
}

pub(in crate::board_command) struct ClaimGit<'a> {
    boundary: GitBoundary,
    prepared: Option<spice::task_git::ClaimPreparation<'a>>,
}

/// Commits and dirty work the task's latest release parked for its next claim.
pub(in crate::board_command) fn task_parked_commits<'a>(
    snapshot: &'a ValidatedSnapshot,
    task: &TaskStamp,
) -> Option<&'a wire::body::TaskParkedCommits> {
    snapshot
        .answer
        .tasks
        .iter()
        .find(|row| row.statement.body.born == *task)
        .and_then(|row| row.parked_commits.as_ref())
}

pub(in crate::board_command) fn claim_git_boundary<'a>(
    repository: Option<Host<'a>>,
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    selected: &Entry,
    possession: Possession,
) -> Result<Option<ClaimGit<'a>>, Outcome<CommandAnswer>> {
    let Some(resident) = repository else {
        return Ok(None);
    };
    let lane = task_repository(snapshot, command, &resident)?;
    let ambient = resident.ambient;
    let parked = task_parked_commits(snapshot, selected.born());
    let result = match possession {
        // Capture adopts loose commits only into implementation; a planning
        // deliverable takes the allocator's clean boundary instead.
        Possession::Capture if parked.is_none() && !planning::planning_deliverable(selected) => {
            spice::task_git::prepare_explicit_claim(&lane, ambient).map(|prepared| ClaimGit {
                boundary: prepared.boundary.clone(),
                prepared: Some(prepared),
            })
        }
        Possession::Capture if parked.is_none() => {
            match spice::task_git::prepare_task_claim(&lane, ambient, None) {
                Err(error) if error.reason() == "git::claim-loose-commits" => {
                    planning::admit_capture(selected)?;
                    return Err(task_git_outcome(error));
                }
                result => result.map(|prepared| ClaimGit {
                    boundary: prepared.boundary.clone(),
                    prepared: Some(prepared),
                }),
            }
        }
        Possession::Steal => match stolen_work(resident, snapshot, command, selected) {
            Ok(Some(work)) => spice::task_git::prepare_task_claim(&lane, ambient, Some(&work)),
            Ok(None) => spice::task_git::prepare_explicit_claim(&lane, ambient),
            Err(reason) => {
                spice::task_git::prepare_explicit_claim(&lane, ambient).map(|mut prepared| {
                    prepared.uncarried = Some(reason);
                    prepared
                })
            }
        }
        .map(|prepared| ClaimGit {
            boundary: prepared.boundary.clone(),
            prepared: Some(prepared),
        }),
        Possession::Next | Possession::Capture | Possession::Join => {
            spice::task_git::prepare_task_claim(&lane, ambient, parked).map(|prepared| ClaimGit {
                boundary: prepared.boundary.clone(),
                prepared: Some(prepared),
            })
        }
        Possession::Recover | Possession::RecoverSteal => {
            let lane = Host {
                repository: &lane,
                ambient,
            };
            return recovery_git_boundary(lane, command, selected).map(|boundary| {
                Some(ClaimGit {
                    boundary,
                    prepared: None,
                })
            });
        }
    };
    result.map(Some).map_err(task_git_outcome)
}

/// The displaced holder's unpublished commits and dirty work, copied from its
/// lane for a steal to replay; `Err` names why none could be carried.
fn stolen_work(
    resident: Host<'_>,
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    selected: &Entry,
) -> Result<Option<wire::body::TaskParkedCommits>, String> {
    let Some(victim) = selected
        .active_claims()
        .find(|held| held.claim().actor != *command.actor())
    else {
        return Ok(None);
    };
    let Some(won) = snapshot
        .facts
        .at(victim.claimed())
        .and_then(|envelope| envelope.fact.read::<body::ClaimWon>())
    else {
        return Ok(None);
    };
    let Some(row) = snapshot
        .answer
        .residency
        .body
        .lanes
        .iter()
        .find(|row| row.lane == won.lane)
    else {
        return Ok(None);
    };
    crate::board_command::copy_holder_work(resident, &row.worktree, won.git.as_ref())
}

fn recovery_git_boundary(
    repository: Host<'_>,
    command: &wire::AdmittedCommand,
    selected: &Entry,
) -> Result<GitBoundary, Outcome<CommandAnswer>> {
    let recovery = spice::task_git::pending_recovery(&repository)
        .map_err(task_git_outcome)?
        .ok_or_else(|| {
            refusal(
                "git::recovery-transaction-missing",
                None,
                "start sealed adoption through `spice task capture HANDLE`",
            )
        })?;
    let transaction = match command.body() {
        wire::intent::Intent::ClaimWin(win) => win.recovery.as_deref(),
        _ => None,
    }
    .expect("recover possession admission requires a transaction");
    if recovery.transaction() != transaction {
        return Err(refusal(
            "git::recovery-transaction-mismatch",
            Some(format!(
                "retained recovery is {}, but the claim names {transaction}",
                recovery.transaction()
            )),
            "resume only the retained recovery transaction",
        ));
    }
    let sealed = spice::task_git::recovery_claim(&repository, &recovery)
        .map_err(task_git_outcome)?
        .ok_or_else(|| {
            refusal(
                "git::recovery-claim-unbound",
                Some(format!("recovery {transaction} has no sealed board claim")),
                "restore with `spice task capture --restore`, then restart capture",
            )
        })?;
    let workflow = command.expected().iter().find_map(|witness| match witness {
        Witness::Workflow { task, head, phase } if task == selected.born() => Some((*head, *phase)),
        _ => None,
    });
    let statement = command.expected().iter().find_map(|witness| match witness {
        Witness::Task { task, head } if task == selected.born() => Some(*head),
        _ => None,
    });
    let claim = witnessed_plane(command.expected(), selected.born());
    let residency = command.expected().iter().find_map(|witness| match witness {
        Witness::Residency {
            lane,
            occupant: Some(occupant),
            epoch,
        } => Some((*lane, *occupant, *epoch)),
        _ => None,
    });
    let Some((workflow, phase)) = workflow else {
        return Err(refusal(
            "git::recovery-workflow-witness",
            None,
            "read the current task and restart capture",
        ));
    };
    let Some(statement) = statement else {
        return Err(refusal(
            "git::recovery-task-witness",
            None,
            "read the current task and restart capture",
        ));
    };
    let Some((lane, occupant, epoch)) = residency else {
        return Err(refusal(
            "git::recovery-residency-witness",
            None,
            "retry through the currently seated lane",
        ));
    };
    if sealed.actor != *command.actor()
        || sealed.task != *selected.born()
        || sealed.statement != statement
        || sealed.workflow != workflow
        || sealed.phase != phase
        || sealed.claim != claim
        || sealed.lane != lane
        || sealed.occupant != occupant
        || sealed.epoch != epoch
    {
        return Err(refusal(
            "git::recovery-claim-moved",
            Some(format!(
                "sealed claim intent no longer matches task {}",
                selected.handle()
            )),
            "retain the quarantine and inspect the moved actor/task/workflow/claim/residency state",
        ));
    }
    let boundary =
        spice::task_git::synchronize_recovery(&repository, repository.ambient, &recovery)
            .map_err(task_git_outcome)?;
    if boundary != sealed.boundary {
        return Err(refusal(
            "git::recovery-claim-boundary-mismatch",
            Some(format!(
                "sealed boundary {:?}; synchronized boundary {boundary:?}",
                sealed.boundary
            )),
            "retain the quarantine and inspect the moved Git boundary",
        ));
    }
    Ok(boundary)
}

/// The claim-plane head `expected` witnesses for `task`: absent when no plane
/// witness was sent or the task had no claim.
fn witnessed_plane(expected: &[Witness], task: &wire::TaskStamp) -> Option<wire::Seq> {
    expected.iter().find_map(|witness| match witness {
        Witness::Claim {
            task: witnessed,
            claimed: None,
            head,
        } if witnessed == task => *head,
        _ => None,
    })
}

/// End the claim a steal displaced under the steal's own win, written after
/// the win so the task retains it as ended since that win.
fn end_displaced(
    write: &mut JournalWrite,
    task: &wire::TaskStamp,
    won: PendingFact,
    displaced: Option<ClaimHead>,
) {
    if let Some(claim) = displaced {
        write.claim_head(task.clone(), won, move |fact| ClaimHead {
            fact,
            disposition: ClaimDisposition::Lapsed,
            ..claim
        });
    }
}

/// Read the lane's accounted transcript boundary for the claim heads.
///
/// The read goes through the writer queue, so it can fail on a closed or
/// faulted bus rather than merely returning nothing; that failure is a store
/// fault instead of a refusal because no command the caller could have sent
/// would have avoided it.
pub(in crate::board_command) fn claim_source_boundary(
    bus: &Bus,
    lane: &wire::CanonicalUuid,
) -> Result<body::SourceBoundary, Outcome<CommandAnswer>> {
    bus.source_boundary(lane)
        .map_err(|error| store_fault("claim::boundary-unreadable", error.to_string()))
}

fn guard_claim(
    write: &mut JournalWrite,
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    possession: Possession,
    _task: &TaskHandle,
) {
    let expected = command.expected().to_vec();
    if possession.named() {
        guard_capture(write, expected, snapshot.cursor, command.actor().clone());
    } else {
        guard_allocation(
            write,
            expected,
            AllocationObservation {
                cursor: snapshot.cursor,
                actor: command.actor().clone(),
                residency_position: snapshot.answer.residency.position,
            },
        );
    }
}

/// Mint one claim and its two heads, naming the head whose build refused.
pub(in crate::board_command) fn mint_claim(
    write: &mut JournalWrite,
    mint: board::allocate::Mint<'_>,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    board::allocate::mint(write, mint).map_err(|error| match error {
        board::allocate::MintError::Assignment(error) => {
            store_fault("review::assignment-build", error.to_string())
        }
        board::allocate::MintError::Claim(error) => {
            store_fault("claim::win-build", error.to_string())
        }
    })
}
