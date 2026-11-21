use super::*;

pub(super) mod activation;
mod claim;
mod deferral;
mod edit;
mod file;
mod invite;
mod join;
mod media;
mod oops;
mod placement;
mod requirements;
mod review_flow;
mod settlement;
mod sweep;
mod target;
pub(crate) use deferral::task_defer;
pub(super) use deferral::task_wake;
pub(super) use file::append_statement_edit as append_activation_statement;
pub(super) use file::task_file;
pub(super) use media::task_media;
pub(super) use placement::place_creation;
pub(super) use settlement::{
    DecisionWrite, dispatch_settlement, stage_conclusion, stage_rejection,
};

#[cfg(test)]
pub(super) use claim::Possession;
#[cfg(test)]
pub(super) use claim::{ClaimPlan, claim_git_boundary, claim_grant, claim_plan, commit_claim};
use claim::{PreparedClaim, claim_room, prepare_capture_claim};
pub(super) use claim::{
    claim_activation_next_action, claim_lease, claim_source_boundary, claim_win, mint_claim,
    named_claim_ineligible, refuse_held_design, task_parked_commits,
};
pub(super) use edit::task_edit;
pub(crate) use invite::task_invite;
pub(crate) use join::{accept_invitation, task_join};
pub(super) use target::task_target;
use wire::Phase;

/// Dispatch one intake that files new work under a controlling task: an OOPS
/// report under its Design, or a debt sweep's children under their Plan.
pub(super) fn dispatch_intake(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'_>>,
) -> Option<Outcome<CommandAnswer>> {
    Some(match command.body() {
        wire::intent::Intent::TaskOopsAdd(add) => {
            super::routing::automatic(|| oops::task_oops_add(bus, command, add, repository))
        }
        wire::intent::Intent::TaskSweep(sweep) => {
            super::routing::automatic(|| sweep::task_sweep(bus, command, sweep, repository))
        }
        _ => return None,
    })
}

/// One actor-filtered board snapshot whose cursor and entries were validated together.
pub(super) struct ValidatedSnapshot {
    pub(super) cursor: wire::Seq,
    pub(super) facts: bus::TaskFacts,
    pub(super) answer: BoardAnswer,
    pub(super) entries: Vec<Entry>,
    pub(super) owners: std::sync::Arc<board::OwnershipSnapshot>,
    pub(super) policy: std::sync::Arc<drive::PolicyCapture>,
    pub(super) topic_bindings: std::sync::Arc<[(wire::TaskStamp, wire::Topic, Option<i64>)]>,
}

impl ValidatedSnapshot {
    pub(super) fn ownership(&self, entry: &Entry) -> wire::intent::TaskOwnership {
        self.ownership_in_phase(entry, &entry.workflow().phase)
    }

    pub(super) fn ownership_in_phase(
        &self,
        entry: &Entry,
        phase: &wire::Phase,
    ) -> wire::intent::TaskOwnership {
        let owners = self.owners.owners_in_phase(entry, phase);
        wire::intent::TaskOwnership {
            teams: owners.teams.clone(),
            reserved: owners.reserved,
        }
    }

    pub(super) fn guard_ownership(&self, write: &mut JournalWrite) {
        let policy = std::sync::Arc::clone(&self.policy);
        let residency = self.answer.residency.position;
        let topics = std::sync::Arc::clone(&self.topic_bindings);
        write.guard(JournalGuard::new(move |read| {
            if !read
                .residency_head()?
                .is_some_and(|head| head.position() == residency)
            {
                return Err(GuardError::refused("team::membership-moved"));
            }
            for (task, path, node) in topics.iter() {
                if read.topic_node(path)?.map(|current| current.id) != *node
                    && !read
                        .workflow_head(task)?
                        .is_some_and(|workflow| !workflow.body().status.is_pending())
                {
                    return Err(GuardError::refused("team::topic-moved"));
                }
            }
            policy.require_current()
        }));
    }
}

/// How a creation leaves the write: advanced into its first substantive
/// phase, or resting in draft with the standing the receipt reports.
enum Standing {
    /// Author-held: an explicit `--draft`, or a drive denial carried in the
    /// receipt. Never allocated.
    Held,
    /// The author's hand-off whose construction the validator refused, with
    /// that diagnostic. An uncontrolled row carries the submitted mark and is
    /// allocatable triage work; a controlled row rests unmarked under its
    /// controller.
    Submitted(Box<str>),
    /// Advanced into the first substantive phase in the same write.
    Advanced,
}

pub(super) struct PreparedTask {
    /// The handle the task answers to once the write lands.
    pub(super) task: TaskHandle,
    pub(super) born: wire::TaskStamp,
    flow: Vec<wire::Phase>,
    origin: wire::Origin,
    creation: TaskCreationCapture,
    at: Instant,
    /// The statement as the write leaves it.
    pub(super) statement: TaskHead,
    /// The workflow as the write leaves it: draft, or the first substantive
    /// phase when advanced.
    workflow: WorkflowHead,
    /// Final saved placement; absent while the task is created unplaced.
    pub(super) placement: Option<wire::intent::TaskPlacement>,
    standing: Standing,
    /// Observations the advance rests on, guarded at commit.
    admission: Option<drive::Admission>,
    destination: Option<activation::Destination>,
}

impl PreparedTask {
    /// Guard the creation capture and every observation an advance rests on.
    pub(super) fn guard(&self, write: &mut JournalWrite) {
        self.creation.guard(write);
        if let Some(admission) = &self.admission {
            admission.guard(write);
        }
        if let Some(destination) = &self.destination {
            destination.guard(write);
        }
    }

    /// Whether the creation leaves draft in its write.
    pub(super) const fn advances(&self) -> bool {
        matches!(self.standing, Standing::Advanced)
    }

    /// The construction or drive diagnostic a receipt carries for a row
    /// resting in draft.
    pub(super) fn diagnostic(&self) -> Option<Box<str>> {
        match &self.standing {
            Standing::Submitted(diagnostic) => Some(diagnostic.clone()),
            Standing::Held => self
                .placement
                .as_ref()
                .and_then(|placement| placement.drive_refusal.clone())
                .map(String::into_boxed_str),
            Standing::Advanced => None,
        }
    }

    /// Decide how this creation leaves its write. Without `--draft` the
    /// phase-zero validator runs over the prepared row against the destination
    /// `placement` chose: a pass advances it; incomplete construction lands it
    /// submitted with the diagnostic; a drive denial lands it author-held with
    /// the denial in its receipt; any other authority refusal refuses the
    /// creation. Returns the dependency witnesses an advance rests on.
    pub(super) fn activate(
        &mut self,
        snapshot: &ValidatedSnapshot,
        command: &wire::AdmittedCommand,
        repository: Option<Host<'_>>,
        draft: bool,
        placement: placement::CreationPlacement,
        siblings: &[activation::Sibling],
    ) -> Result<Vec<Witness>, Outcome<CommandAnswer>> {
        let destination = activation::Destination::decided(
            placement.topic,
            placement.route.clone(),
            placement.capture,
        );
        self.placement = destination.placement();
        let destination = self.destination.insert(destination);
        if draft {
            return Ok(Vec::new());
        }
        match activation::validate(
            snapshot,
            command,
            repository,
            self.statement.clone(),
            &self.workflow,
            destination,
            siblings,
        ) {
            Ok(activation) => {
                self.statement = activation.statement;
                self.workflow = activation.workflow;
                self.task = TaskHandle::for_phase(
                    self.statement.topic.path.as_ref(),
                    &self.born,
                    self.workflow.phase,
                );
                self.standing = Standing::Advanced;
                self.admission = Some(activation.admission);
                Ok(activation.dependency_witnesses)
            }
            Err(activation::AdvanceRefusal::Construction(outcome)) => {
                // The hand-off mark belongs to uncontrolled drafts alone: a
                // controlled row leaves draft through its controller.
                self.standing = Standing::Submitted(activation::diagnostic(&outcome));
                self.workflow.submitted = self.statement.preparation.is_none();
                Ok(Vec::new())
            }
            Err(activation::AdvanceRefusal::Drive(outcome)) => {
                let diagnostic = activation::diagnostic(&outcome);
                if let Some(placement) = &mut self.placement {
                    placement.drive_refusal = Some(diagnostic.into_string());
                    placement.next = Some(format!("spice task place {}", self.task));
                }
                self.standing = Standing::Held;
                Ok(Vec::new())
            }
            Err(refused) => Err(refused.into_outcome()),
        }
    }
}

struct AtomicClaim<'a> {
    prepared: PreparedClaim<'a>,
    answer: Box<wire::intent::ClaimAnswer>,
}

fn prepare_mutation_claim<'a>(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'a>>,
    snapshot: &ValidatedSnapshot,
    selected: &Entry,
    requested: bool,
) -> Result<Option<AtomicClaim<'a>>, Outcome<CommandAnswer>> {
    if !requested {
        return Ok(None);
    }
    if !selected.statement().resources.is_empty() {
        return Err(refusal(
            "task::resource-claim-must-allocate",
            Some(selected.handle().to_string()),
            "perform the mutation without --claim, then run task next so allocation reserves its resource",
        ));
    }
    let room_context = claim_room(bus, repository, selected)?;
    let prepared = prepare_capture_claim(bus, command, repository, snapshot, selected, false)?;
    let origin_context = task_steering_origin_output(bus, &snapshot.facts, selected)?;
    let answer =
        Box::new(prepared.answer(command, snapshot, selected, origin_context, room_context));
    Ok(Some(AtomicClaim { prepared, answer }))
}

fn prepare_mutation_write(
    command: &wire::AdmittedCommand,
    entry: &Entry,
    snapshot: &ValidatedSnapshot,
    atomic_claim: Option<&AtomicClaim<'_>>,
) -> Result<JournalWrite, Outcome<CommandAnswer>> {
    let at = match atomic_claim {
        Some(claim) => claim.prepared.at(),
        None => current_instant()?,
    };
    let origin = task_origin(entry.born())?;
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    if let Some(claim) = atomic_claim {
        claim.prepared.guard(&mut write, snapshot, command);
    } else {
        snapshot.guard_ownership(&mut write);
    }
    Ok(write)
}

pub(super) fn task_add(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    add: &TaskAdd,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    task_add_with(bus, command, add, repository, || {})
}

pub(super) fn task_add_with(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    add: &TaskAdd,
    repository: Option<Host<'_>>,
    after_validation: impl FnOnce(),
) -> Outcome<CommandAnswer> {
    if let Err(outcome) = draft_creation_admitted(add) {
        return outcome;
    }
    let mut after_validation = Some(after_validation);
    super::routing::automatic(|| {
        create_task(bus, command, add, repository, || {
            if let Some(after_validation) = after_validation.take() {
                after_validation();
            }
        })
    })
}

/// An author-held draft is never claimed: `--draft` rests the row, and a
/// claim takes only work that has left draft.
fn draft_creation_admitted(add: &TaskAdd) -> Result<(), Outcome<CommandAnswer>> {
    if add.draft && add.claim {
        return Err(refusal(
            "task::draft-claim-refused",
            None,
            "create the draft without --claim, then advance and claim it with spice task file DRAFT --claim",
        ));
    }
    Ok(())
}

/// Advances and statement edits name a complete destination, never a router tail.
pub(super) fn exact_topic(
    input: &wire::TopicInput,
) -> Result<wire::TaskTopic, Outcome<CommandAnswer>> {
    let topic = match input {
        wire::TopicInput::Absolute(topic) | wire::TopicInput::Private(topic) => topic.clone(),
        wire::TopicInput::Tail(_) => {
            return Err(refusal(
                "task::topic-must-be-explicit",
                Some(input.to_string()),
                "name a complete /topic or UUID/owned/path; use spice task place to preview routing",
            ));
        }
    };
    refuse_reserved_oops_topic(&topic)?;
    Ok(topic.into())
}

pub(super) fn refuse_reserved_oops_topic(
    topic: &wire::Topic,
) -> Result<(), Outcome<CommandAnswer>> {
    if wire::is_oops_topic(topic) {
        return Err(refusal(
            "task::oops-topic-reserved",
            Some(topic.to_string()),
            wire::OOPS_TOPIC_REMEDIATION,
        ));
    }
    Ok(())
}

/// Bind the canonical authored statement used to route a task addition.
pub(super) fn task_add_statement<'a>(
    add: &'a TaskAdd,
    phase: &'a Phase,
) -> super::routing::Statement<'a> {
    super::routing::Statement::new(
        &add.title,
        &add.pitch,
        add.description.as_deref(),
        &add.term,
        phase,
    )
}

struct AdmittedAdd {
    placement: placement::CreationPlacement,
    origin: CreationCause,
    snapshot: ValidatedSnapshot,
    flow: Vec<Phase>,
    dependency_witnesses: Vec<Witness>,
}

/// Forecast every unnamed creation. An author-held or controlled draft reports
/// the proposal without binding it until filing.
fn admit_add_topic(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    add: &TaskAdd,
    repository: Option<&Repository>,
) -> Result<AdmittedAdd, Outcome<CommandAnswer>> {
    let roots = task_add_roots(add);
    let snapshot =
        validated_working_snapshot_for_tasks(bus, repository, command.actor().clone(), &roots)?;
    let origin = resolve_creation_origin(
        add.origin.as_ref(),
        command.actor(),
        &snapshot,
        operator_draft_origin(command, add.draft),
        command.holds(wire::principal::PrincipalRole::Operator),
    )?;
    let (flow, dependency_witnesses) =
        task_flow_for_dependencies(command, add, &snapshot, &origin)?;
    let controlled = planning::preparation_parent(&snapshot.answer, add)?.is_some();
    let mut placement = place_creation(
        bus,
        &snapshot,
        command.actor(),
        add.topic.as_ref(),
        task_add_statement(add, &flow[1]),
        super::routing::OriginView::of(bus, &origin.provenance)?,
        None,
    )?;
    if (add.draft || controlled) && add.topic.is_none() {
        placement.topic = wire::TaskTopic::unplaced(None);
    }
    Ok(AdmittedAdd {
        placement,
        origin,
        snapshot,
        flow,
        dependency_witnesses,
    })
}

/// Create one manually added task: place it, validate the request against
/// the actor's board, prepare its heads in draft, decide how it leaves the
/// write, and commit them with any claim the request takes.
fn create_task(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    add: &TaskAdd,
    repository: Option<Host<'_>>,
    after_validation: impl FnOnce(),
) -> Outcome<CommandAnswer> {
    let AdmittedAdd {
        placement,
        origin,
        snapshot,
        flow,
        mut dependency_witnesses,
    } = match admit_add_topic(bus, command, add, repository.map(|host| host.repository)) {
        Ok(admitted) => admitted,
        Err(outcome) => return outcome,
    };
    let creation = match task_creation_capture(bus, command, &snapshot) {
        Ok(creation) => creation,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = validate_term(&add.term).and_then(|()| validate_pitch(&add.pitch)) {
        return outcome;
    }
    let mut prepared = match prepare_task(
        command,
        add,
        placement.topic.clone(),
        TaskSeed {
            cause: origin,
            creation,
            flow,
        },
    ) {
        Ok(prepared) => prepared,
        Err(outcome) => return outcome,
    };
    if captures_implementation(command) {
        let review = match task_taste_review(
            repository,
            &snapshot,
            command.actor(),
            &prepared.statement.title,
            &prepared.statement.pitch,
            prepared.statement.description.as_deref(),
            &prepared.statement.term,
        ) {
            Ok(review) => review.filter(TasteReview::required),
            Err(outcome) => return outcome,
        };
        if let Some(review) = review {
            return review.refusal(
                "captured implementation requires taste-reviewed task text".to_owned(),
                "correct the capture title and retry; no task or claim was created and the local commits remain on this lane",
            );
        }
    }
    if let Err(outcome) =
        planning::admit_preparation(&snapshot, command, add, &mut prepared.statement)
    {
        return outcome;
    }
    let draft = add.draft || prepared.statement.preparation.is_some();
    if draft && add.claim {
        return refusal(
            "task::draft-claim-refused",
            None,
            "construct controlled drafts without claiming them",
        );
    }
    match prepared.activate(&snapshot, command, repository, draft, placement, &[]) {
        Ok(witnesses) => dependency_witnesses.extend(witnesses),
        Err(outcome) => return outcome,
    }
    // Only an advanced row can be claimed, so a claiming creation that would
    // rest in draft refuses whole rather than committing unclaimed work its
    // author believes it holds.
    if add.claim && !prepared.advances() {
        return refusal(
            "task::claim-unadvanced",
            prepared.diagnostic().map(String::from),
            "complete the contract the diagnostic names and retry, or add without --claim to keep a draft; no task was created",
        );
    }
    if add.claim && add.requirements.resource_pools().next().is_some() {
        return refusal(
            "task::resource-claim-must-allocate",
            Some("atomic creation cannot reserve a configured resource pool".to_owned()),
            "create the task without --claim, then run task next so allocation reserves its resource",
        );
    }
    let post = prepared_entry(&prepared);
    let added_claim = if add.claim {
        match prepare_capture_claim(bus, command, repository, &snapshot, &post, true) {
            Ok(claim) => Some(claim),
            Err(outcome) => return outcome,
        }
    } else {
        None
    };
    after_validation();
    commit_prepared_task(
        bus,
        command,
        add.clone(),
        &snapshot,
        AdmittedCreation {
            prepared,
            post,
            dependency_witnesses,
            claim: added_claim,
        },
    )
}

/// The operator is the root authority, so a draft it authors needs no further
/// cause. A lane's draft still names its directive or held parent, so drafting
/// never mints causeless work for an agent.
fn operator_draft_origin(
    command: &wire::AdmittedCommand,
    draft: bool,
) -> Option<wire::TaskProvenance> {
    (draft && command.holds(wire::principal::PrincipalRole::Operator))
        .then_some(wire::TaskProvenance::OperatorDraft)
}

pub(super) fn task_add_roots(add: &TaskAdd) -> Vec<wire::TaskStamp> {
    let mut roots = add
        .depends
        .iter()
        .map(|handle| {
            handle
                .born()
                .expect("an admitted task handle carries a canonical task stamp")
        })
        .collect::<Vec<_>>();
    if let Some(parent) = add
        .origin
        .as_ref()
        .and_then(|origin| wire::TaskProvenance::authored(origin.as_str()).ok())
        .and_then(|provenance| provenance.parent().cloned())
    {
        roots.push(parent);
    }
    roots.extend(add.preparation.as_ref().map(|parent| {
        parent
            .born()
            .expect("an admitted preparation handle carries a canonical task stamp")
    }));
    roots
}

pub(super) fn task_review_roots(review: &TaskReview) -> Vec<wire::TaskStamp> {
    let mut roots = std::iter::once(&review.task)
        .chain(review.followup.iter())
        .map(|handle| {
            handle
                .born()
                .expect("an admitted task handle carries a canonical task stamp")
        })
        .collect::<Vec<_>>();
    roots.extend(
        review
            .then
            .iter()
            .flat_map(|followup| task_add_roots(&followup.add)),
    );
    roots
}

/// One creation admitted against its snapshot: the prepared heads, the entry
/// the task stands as once born, the dependency witnesses that guard the
/// write, and any claim taken in the same transaction.
struct AdmittedCreation<'a> {
    prepared: PreparedTask,
    post: Entry,
    dependency_witnesses: Vec<Witness>,
    claim: Option<PreparedClaim<'a>>,
}

fn commit_prepared_task<B>(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    request_body: B,
    snapshot: &ValidatedSnapshot,
    creation: AdmittedCreation,
) -> Outcome<CommandAnswer>
where
    B: serde::Serialize,
{
    let AdmittedCreation {
        prepared,
        post,
        dependency_witnesses,
        claim: added_claim,
    } = creation;
    let origin_context = if added_claim.is_some() {
        match task_steering_origin_output(bus, &snapshot.facts, &post) {
            Ok(context) => context,
            Err(outcome) => return outcome,
        }
    } else {
        None
    };
    // A task born in this transaction has published nothing in its room.
    let claim_answer = added_claim
        .as_ref()
        .map(|claim| Box::new(claim.answer(command, snapshot, &post, origin_context, None)));
    let next = match &prepared.standing {
        Standing::Held if prepared.diagnostic().is_some() => {
            Some(format!("spice task place {}", prepared.task).into_boxed_str())
        }
        Standing::Submitted(_) => prepared
            .placement
            .as_ref()
            .and_then(|placement| placement.next.clone())
            .map(String::into_boxed_str),
        Standing::Held | Standing::Advanced => claim_answer
            .as_ref()
            .and_then(|claim| claim.next.clone())
            .or_else(|| next_action(snapshot, &post)),
    };
    let at = added_claim.as_ref().map_or(prepared.at, PreparedClaim::at);
    let mut write = JournalWrite::new(
        command.actor().clone(),
        prepared.origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let mut expected = command.expected().to_vec();
    expected.extend(dependency_witnesses);
    guard(&mut write, expected);
    prepared.guard(&mut write);
    if let Some(claim) = &added_claim {
        claim.guard(&mut write, snapshot, command);
    } else {
        snapshot.guard_ownership(&mut write);
    }
    let task = prepared.task.clone();
    let diagnostic = prepared.diagnostic();
    let status = prepared.workflow.phase.to_string();
    let placement = match append_prepared_task(&mut write, prepared, FactCause::Root) {
        Ok((_, placement)) => placement,
        Err(outcome) => return outcome,
    };
    if let Some(claim) = added_claim
        && let Err(outcome) = claim.append(&mut write, command, &post)
    {
        return outcome;
    }
    let answer = wire::intent::TaskAnswer {
        diagnostic,
        ownership: Some(snapshot.ownership(&post)),
        placement,
        created: Default::default(),
        taste: answered_taste(command.actor(), post.statement().taste.as_ref()),
        completion_contract: Some(
            completion_contract(
                &post.statement().term,
                post.born(),
                &post.statement().dependencies,
                post.statement().intent,
                snapshot.entries.iter(),
            )
            .into_boxed_str(),
        ),
        task,
        status,
        completion: None,
        claim: claim_answer,
        closed: Vec::new(),
        next,
    };
    if let Err(error) = write.command_result(
        request(command, request_body),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("task::add-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::Task(Box::new(answer)))
}

/// Append one prepared task's birth fact and both heads, then the advance
/// out of draft when the creation makes one. The birth fact repeats the
/// prepared statement's authored fields, so a statement adjusted after
/// preparation is born as adjusted; the birth is always in draft, and the
/// returned fact is the birth the write's later facts follow.
pub(super) fn append_prepared_task(
    write: &mut JournalWrite,
    prepared: PreparedTask,
    cause: FactCause,
) -> Result<(journal::PendingFact, Option<wire::intent::TaskPlacement>), Outcome<CommandAnswer>> {
    let advances = prepared.advances();
    let added = write
        .fact(
            body::TaskAdded {
                preparation: prepared.statement.preparation.clone(),
                intent: prepared.statement.intent,
                task: prepared.born.clone(),
                title: prepared.statement.title.clone(),
                pitch: prepared.statement.pitch.clone(),
                description: prepared.statement.description.clone(),
                term: prepared.statement.term.clone(),
                requirements: prepared.statement.requirements.clone(),
                topic: prepared.statement.topic.clone(),
                submitted: prepared.workflow.submitted,
                priority: prepared.statement.priority,
                taste: prepared.statement.taste.clone(),
                depends: prepared.statement.dependencies.clone(),
                flow: prepared.flow,
                phase: Phase::Draft,
                revision: 1,
                creation: prepared.creation.context(),
            },
            cause,
        )
        .map_err(|error| store_fault("task::add-build", error.to_string()))?;
    let statement = prepared.statement;
    write.task_head(added, move |position| {
        published_statement(
            TaskHead {
                topic_origin: position,
                revision: position,
                ..statement
            },
            position,
        )
    });
    if advances {
        activation::append_advance(
            write,
            &prepared.born,
            prepared.workflow,
            FactCause::Pending(added),
        )?;
    } else {
        write.workflow_head(prepared.born, added, move |_| prepared.workflow);
    }
    Ok((added, prepared.placement))
}

pub(super) fn prepared_entry(prepared: &PreparedTask) -> Entry {
    Entry::new(
        prepared.statement.clone(),
        prepared.workflow.clone(),
        placeholder_completion(),
        Vec::new(),
    )
    .expect("prepared task already passed entry validation")
    .with_creator_team(prepared.creation.creator_team())
}

/// A not-yet-committed task's completion head: every target one, with the
/// round the journal assigns at commit.
pub(super) fn placeholder_completion() -> wire::CompletionHead {
    wire::CompletionHead::opened(Seq::new(1).expect("placeholder round is non-zero"))
}

/// What one new task is born from: the cause it answers, the creation capture
/// it records, and the flow it opens in.
#[derive(Clone)]
pub(super) struct TaskSeed {
    pub(super) cause: CreationCause,
    pub(super) creation: TaskCreationCapture,
    pub(super) flow: Vec<wire::Phase>,
}

fn prepare_task(
    command: &wire::AdmittedCommand,
    add: &TaskAdd,
    topic: wire::TaskTopic,
    seed: TaskSeed,
) -> Result<PreparedTask, Outcome<CommandAnswer>> {
    let handles = command
        .expected()
        .iter()
        .filter_map(|witness| match witness {
            Witness::Absence { task } => Some(task.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [born] = handles.as_slice() else {
        return Err(protocol_fault(
            "task::add-witness-invalid",
            "task add requires exactly one intended absent birth stamp",
        ));
    };
    prepare_task_for(add, topic, seed, born.clone())
}

/// Prepare one task's heads as the author-held draft it is born as; the
/// caller decides through [`PreparedTask::activate`] how it leaves the write.
pub(super) fn prepare_task_for(
    add: &TaskAdd,
    topic: wire::TaskTopic,
    seed: TaskSeed,
    born: wire::TaskStamp,
) -> Result<PreparedTask, Outcome<CommandAnswer>> {
    let TaskSeed {
        cause,
        creation,
        flow,
    } = seed;
    if cause.provenance.parent() == Some(&born) {
        return Err(refusal(
            "task::origin-self",
            Some(cause.provenance.to_string()),
            "name the steering directive or existing parent task that caused this task",
        ));
    }
    let statement = TaskHead {
        preparation: None,
        intent: add.intent,
        born: born.clone(),
        topic_origin: Seq::new(1).expect("placeholder topic origin is non-zero"),
        title: add.title.clone(),
        pitch: add.pitch.trim().to_owned(),
        description: add.description.clone(),
        term: add.term.clone(),
        requirements: add.requirements.without_resources(),
        resources: add.requirements.compiled_resources(),
        topic,
        priority: None,
        taste: None,
        dependencies: add
            .depends
            .iter()
            .map(TaskHandle::born)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                refusal(
                    "task::dependency-invalid",
                    Some(error.to_string()),
                    "name dependencies carrying canonical birth stamps",
                )
            })?,
        provenance: cause.provenance,
        creation: creation.context(),
        revision: Seq::new(1).expect("placeholder revision is non-zero"),
    };
    prepare_statement(statement, flow, cause.origin, creation)
}

/// Prepare an authored statement as the author-held draft it is born as.
pub(super) fn prepare_statement(
    statement: TaskHead,
    flow: Vec<wire::Phase>,
    origin: wire::Origin,
    creation: TaskCreationCapture,
) -> Result<PreparedTask, Outcome<CommandAnswer>> {
    let born = statement.born.clone();
    let task = TaskHandle::for_phase(statement.topic.path.as_ref(), &born, Phase::Draft);
    let at = current_instant()?;
    let workflow = WorkflowHead {
        flow: flow.clone(),
        phase: Phase::Draft,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    };
    if let Err(error) = Entry::new(
        statement.clone(),
        workflow.clone(),
        placeholder_completion(),
        Vec::new(),
    ) {
        return Err(refusal(
            "task::add-refused",
            Some(error.to_string()),
            "correct the task topic, dependencies, or phase flow and retry",
        ));
    }
    Ok(PreparedTask {
        task,
        born,
        flow,
        origin,
        creation,
        at,
        statement,
        workflow,
        placement: None,
        standing: Standing::Held,
        admission: None,
        destination: None,
    })
}

fn captures_implementation(command: &wire::AdmittedCommand) -> bool {
    command.expected().iter().any(|witness| {
        matches!(
            witness,
            Witness::Eligibility { rule } if rule.as_str() == CAPTURE_WORK_RULE
        )
    })
}

fn recovers_implementation(command: &wire::AdmittedCommand) -> bool {
    command.expected().iter().any(|witness| {
        matches!(
            witness,
            Witness::Eligibility { rule } if rule.as_str() == "recover"
        )
    })
}

fn task_flow_for_dependencies(
    command: &wire::AdmittedCommand,
    add: &TaskAdd,
    snapshot: &ValidatedSnapshot,
    origin: &CreationCause,
) -> Result<(Vec<wire::Phase>, Vec<Witness>), Outcome<CommandAnswer>> {
    let requested = planning::default_flow(add.intent, add.flow.as_deref());
    let adopts_implementation =
        captures_implementation(command) || recovers_implementation(command);
    let requested = if adopts_implementation && requested.is_none() {
        Some("todo,review")
    } else {
        requested
    };
    let mut flow = phases(requested)?;
    planning::admit_design_flow(add.intent, &flow)?;
    if adopts_implementation
        && (add.intent.is_some() || gate::lacks_commit_authority(flow[1].as_str()))
    {
        return Err(refusal(
            "task::capture-planning",
            None,
            "captured implementation cannot create or replace a planning contract",
        ));
    }
    let dependencies = add
        .depends
        .iter()
        .map(TaskHandle::born)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            refusal(
                "task::dependency-invalid",
                Some(error.to_string()),
                "name dependencies carrying canonical birth stamps",
            )
        })?;
    let mut witnesses = derive_dependency_flow(
        &mut flow,
        add.flow.is_some() || add.intent.is_some(),
        &dependencies,
        snapshot,
    );
    review_flow::omit_origin_review(&mut flow, &mut witnesses, command, add, snapshot, origin);
    Ok((flow, witnesses))
}

/// Witness each dependency's workflow and, for a defaulted `flow`, omit the
/// review phase when every dependency is awaiting review. A named flow is
/// honored as written.
pub(super) fn derive_dependency_flow(
    flow: &mut Vec<wire::Phase>,
    explicit: bool,
    dependencies: &[TaskStamp],
    snapshot: &ValidatedSnapshot,
) -> Vec<Witness> {
    let mut witnesses = Vec::with_capacity(dependencies.len());
    let mut all_review = !dependencies.is_empty();
    for born in dependencies {
        let Some(heads) = snapshot
            .answer
            .tasks
            .iter()
            .find(|heads| &heads.statement.body.born == born)
        else {
            all_review = false;
            witnesses.push(Witness::Absence { task: born.clone() });
            continue;
        };
        all_review &= heads.workflow.body.phase.as_str() == "review"
            && heads.workflow.body.status.is_pending();
        witnesses.push(workflow_witness(heads));
    }
    if !explicit {
        omit_review_phase(flow, all_review);
    }
    witnesses
}

/// Drop the review phase from a defaulted flow. Every default names a
/// non-review phase, so the flow stays nonempty.
pub(super) fn omit_review_phase(flow: &mut Vec<wire::Phase>, all_review: bool) {
    if all_review {
        flow.retain(|phase| phase.as_str() != "review");
    }
}

pub(super) fn task_note(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    note: &TaskNote,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let content = match room::authored_observation(&note.note, &note.task) {
        Ok(content) => content,
        Err(outcome) => return outcome,
    };
    let snapshot = match validated_working_snapshot(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        std::slice::from_ref(&note.task),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(entry) = task_entry(&snapshot.entries, &note.task) else {
        return refusal(
            "task::unknown",
            Some(note.task.to_string()),
            "read board and name one current task handle",
        );
    };
    if let Err(outcome) = room::publishable_room(entry) {
        return outcome;
    }
    let next = next_action(&snapshot, entry);
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let publication = match room::mint_publication(bus, entry.born(), at.get().as_microsecond()) {
        Ok(publication) => publication,
        Err(outcome) => return outcome,
    };
    let origin = match task_origin(entry.born()) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    snapshot.guard_ownership(&mut write);
    if let Err(error) = write.fact(
        wire::rooms::RoomPublished {
            room: entry.born().clone(),
            publication,
            content,
        },
        FactCause::Root,
    ) {
        return store_fault("task::note-build", error.to_string());
    }
    let answer = wire::intent::TaskAnswer {
        diagnostic: None,
        ownership: Some(snapshot.ownership(entry)),
        placement: None,
        created: Default::default(),
        taste: None,
        completion_contract: None,
        task: entry.handle().clone(),
        status: "noted".to_owned(),
        completion: None,
        claim: None,
        closed: Vec::new(),
        next,
    };
    if let Err(error) = write.command_result(
        request(command, note.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("task::note-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::Task(Box::new(answer)))
}

pub(super) fn task_depends(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    depends: &TaskDepends,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    task_depends_with(bus, command, depends, repository, || {})
}

pub(super) fn task_depends_with(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    depends: &TaskDepends,
    repository: Option<Host<'_>>,
    after_validation: impl FnOnce(),
) -> Outcome<CommandAnswer> {
    let roots = std::iter::once(depends.task.clone())
        .chain(depends.after.iter().cloned())
        .collect::<Vec<_>>();
    let snapshot = match validated_working_snapshot(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        &roots,
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let entry = match dependency_entry(&snapshot, depends) {
        Ok(entry) => entry,
        Err(outcome) => return outcome,
    };
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let statement = match dependency_statement(&snapshot.entries, entry, depends) {
        Ok(statement) => statement,
        Err(outcome) => return outcome,
    };
    after_validation();
    if let Err(outcome) =
        require_dependency_reachability_witnesses(&snapshot, entry, &statement, command)
    {
        return outcome;
    }
    let post = match Entry::new(
        statement.clone(),
        entry.workflow().clone(),
        entry.completion().clone(),
        entry.claim_bodies(),
    ) {
        Ok(post) => post.with_design(entry.design()),
        Err(error) => {
            return refusal(
                "task::dependency-refused",
                Some(error.to_string()),
                "submit a valid acyclic dependency edit",
            );
        }
    };
    if !depends.remove
        && let Err(outcome) =
            planning::refuse_disposed_construction(&snapshot, post.statement(), &[])
    {
        return outcome;
    }
    let next = next_action(&snapshot, &post);
    commit_dependency_edit(bus, command, depends, &snapshot, statement, next, at)
}

fn require_dependency_witness(
    command: &wire::AdmittedCommand,
    witness: &Witness,
    remedy: &str,
) -> Result<(), Outcome<CommandAnswer>> {
    if command.expected().contains(witness) {
        return Ok(());
    }
    Err(refusal(
        "task::dependency-witness-missing",
        Some(format!("{witness:?}")),
        remedy,
    ))
}

fn require_dependency_reachability_witnesses(
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    statement: &TaskHead,
    command: &wire::AdmittedCommand,
) -> Result<(), Outcome<CommandAnswer>> {
    let added = statement
        .dependencies
        .iter()
        .filter(|dependency| !entry.statement().dependencies.contains(dependency))
        .cloned()
        .collect::<Vec<_>>();
    let mut required = Vec::new();
    append_dependency_reachability_witnesses(&snapshot.answer, &added, &mut required)?;
    let missing = required
        .iter()
        .find(|witness| !command.expected().contains(witness));
    if let Some(missing) = missing {
        return require_dependency_witness(
            command,
            missing,
            "read the complete current prerequisite graph before editing dependencies",
        );
    }
    Ok(())
}

/// The task whose dependencies change. A draft may name another draft, which
/// the phase-zero validator judges when it advances; a row that has left
/// draft waits only on activated work, since a draft satisfies no prerequisite.
fn dependency_entry<'a>(
    snapshot: &'a ValidatedSnapshot,
    depends: &TaskDepends,
) -> Result<&'a Entry, Outcome<CommandAnswer>> {
    let Some(entry) = task_entry(&snapshot.entries, &depends.task) else {
        return Err(refusal(
            "task::unknown",
            Some(depends.task.to_string()),
            "read board and name one current task handle",
        ));
    };
    if entry.workflow().status.is_completed() {
        return Err(refusal(
            "task::settled",
            Some(entry.handle().to_string()),
            "edit dependencies only while the task is pending",
        ));
    }
    Ok(entry)
}

fn commit_dependency_edit(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    depends: &TaskDepends,
    snapshot: &ValidatedSnapshot,
    statement: TaskHead,
    next: Option<Box<str>>,
    at: Instant,
) -> Outcome<CommandAnswer> {
    let entry = task_entry(&snapshot.entries, &depends.task).expect("dependency edit admitted");
    let completion_contract = Some(
        completion_contract(
            &statement.term,
            &statement.born,
            &statement.dependencies,
            statement.intent,
            snapshot.entries.iter(),
        )
        .into_boxed_str(),
    );
    let origin = match task_origin(entry.born()) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    snapshot.guard_ownership(&mut write);
    let revision = u32::try_from(entry.statement().revision.get())
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let dependencies = statement.dependencies.clone();
    let edited = match write.fact(
        body::TaskEdited {
            intent: None,
            task: entry.born().clone(),
            revision,
            dependencies: Some(dependencies),
            flow: None,
        },
        FactCause::Root,
    ) {
        Ok(fact) => fact,
        Err(error) => return store_fault("task::depends-build", error.to_string()),
    };
    write.task_head(edited, move |position| TaskHead {
        revision: position,
        ..statement
    });
    let answer = wire::intent::TaskAnswer {
        diagnostic: None,
        ownership: Some(snapshot.ownership(entry)),
        placement: None,
        created: Default::default(),
        taste: None,
        completion_contract,
        task: entry.handle().clone(),
        status: "edited".to_owned(),
        completion: None,
        claim: None,
        closed: Vec::new(),
        next,
    };
    if let Err(error) = write.command_result(
        request(command, depends.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("task::depends-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::Task(Box::new(answer)))
}

fn dependency_statement(
    entries: &[Entry],
    entry: &Entry,
    depends: &TaskDepends,
) -> Result<TaskHead, Outcome<CommandAnswer>> {
    let prerequisites = depends
        .after
        .iter()
        .map(|after| task_entry(entries, after))
        .collect::<Vec<_>>();
    let mut resolved = Vec::with_capacity(depends.after.len());
    for (after, prerequisite) in depends.after.iter().zip(prerequisites) {
        let Some(prerequisite) = prerequisite else {
            return Err(refusal(
                "task::dependency-unknown",
                Some(after.to_string()),
                "read board and name an existing prerequisite",
            ));
        };
        resolved.push(prerequisite);
    }
    let mut statement = entry.statement().clone();
    if depends.remove {
        for prerequisite in resolved {
            if !entry.statement().dependencies.contains(prerequisite.born()) {
                return Err(refusal(
                    "task::dependency-absent",
                    Some(format!(
                        "{} after {}",
                        entry.handle(),
                        prerequisite.handle()
                    )),
                    "name a dependency edge currently present on the task",
                ));
            }
            statement
                .dependencies
                .retain(|dependency| dependency != prerequisite.born());
        }
        return Ok(statement);
    }
    let mut added = Vec::new();
    for prerequisite in resolved {
        if prerequisite.born() == entry.born() {
            return Err(refusal(
                "task::dependency-refused",
                Some(format!(
                    "{} after {}",
                    entry.handle(),
                    prerequisite.handle()
                )),
                "a task cannot depend on itself",
            ));
        }
        if statement.dependencies.contains(prerequisite.born())
            || added.contains(prerequisite.born())
        {
            continue;
        }
        if dependency_reaches(entries, prerequisite.born(), entry.born()) {
            return Err(refusal(
                "task::dependency-refused",
                Some(format!(
                    "{} after {}",
                    entry.handle(),
                    prerequisite.handle()
                )),
                "name dependencies that keep the complete task graph acyclic",
            ));
        }
        statement.dependencies.push(prerequisite.born().clone());
        added.push(prerequisite.born().clone());
    }
    Ok(statement)
}

fn dependency_reaches(entries: &[Entry], start: &TaskStamp, target: &TaskStamp) -> bool {
    let mut pending = vec![start.clone()];
    let mut visited = Vec::new();
    while let Some(current) = pending.pop() {
        if &current == target {
            return true;
        }
        if visited.contains(&current) {
            continue;
        }
        visited.push(current.clone());
        if let Some(entry) = entries.iter().find(|entry| entry.born() == &current) {
            pending.extend(entry.statement().dependencies.iter().cloned());
        }
    }
    false
}

/// One row's phase, draft standing and possession, for refusal detail.
pub(super) fn standing_detail(entry: &Entry) -> String {
    let claim = if entry.is_held() {
        "active"
    } else {
        match entry.latest_claim().map(|held| held.claim().disposition) {
            Some(ClaimDisposition::Active) => "active",
            Some(ClaimDisposition::Freed) => "freed",
            Some(ClaimDisposition::Lapsed) => "lapsed",
            None => "none",
        }
    };
    let standing = if !entry.is_draft() {
        "active"
    } else if entry.is_controlled() {
        "controlled"
    } else if entry.workflow().submitted {
        "submitted"
    } else {
        "held"
    };
    format!(
        "{} topic={} phase={} standing={standing} status={} claim={claim}",
        entry.handle(),
        wire::topic_label(entry.topic()),
        entry.workflow().phase,
        entry.workflow().status,
    )
}
