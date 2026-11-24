use super::*;

/// Replace an unsettled task statement in place, at a fresh revision.
///
/// A correction is the same statement-head publication `task depends` performs,
/// so the durable record is a new revision rather than an overwrite: the prior
/// text stays readable in the journal and the edit carries the actor that made
/// it. Only fields the caller named move; the rest ride through untouched.
pub(in crate::board_command) fn task_edit(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    edit: &TaskEdit,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let snapshot = match validated_working_snapshot(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        std::slice::from_ref(&edit.task),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(entry) = task_entry(&snapshot.entries, &edit.task) else {
        return refusal(
            "task::unknown",
            Some(edit.task.to_string()),
            "read board and name one current task handle",
        );
    };
    // Correcting a row that has already left the board would publish a
    // revision nobody reaches through the working board.
    if !entry.workflow().status.is_pending() {
        return refusal(
            "task::settled",
            Some(entry.handle().to_string()),
            "correct a statement only while the task is pending",
        );
    }
    // The OOPS generation names one open opportunistic Design in its fixed
    // topic; its statement text amends like any Design's.
    if (edit.intent.is_some() || edit.topic.is_some())
        && entry
            .statement()
            .intent
            .is_some_and(wire::TaskIntent::is_opportunistic_design)
    {
        return refusal(
            "task::oops-design-internal",
            Some(entry.handle().to_string()),
            "the OOPS Design keeps its intent and topic; amend its text instead",
        );
    }
    let (statement, publication) =
        match prepare_correction(bus, &snapshot, entry, command, edit, repository) {
            Ok(prepared) => prepared,
            Err(outcome) => return outcome,
        };
    publish_correction(bus, command, edit, statement, publication, &snapshot)
}

fn prepare_correction(
    bus: &Bus,
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    command: &wire::AdmittedCommand,
    edit: &TaskEdit,
    repository: Option<Host<'_>>,
) -> Result<(TaskHead, CorrectionPublication), Outcome<CommandAnswer>> {
    if let Some(term) = &edit.term {
        validate_term(term)?;
    }
    if let Some(pitch) = &edit.pitch {
        validate_pitch(pitch)?;
    }
    let (mut statement, flow) = corrected_statement(entry, edit)?;
    // A Draft asserts nothing yet; taste policy binds when it is filed.
    if !entry.is_draft() {
        review_taste(repository, snapshot, command, &mut statement)?;
    }
    let at = current_instant()?;
    if flow.is_some() {
        admit_edit_authority(snapshot, entry, command, at)?;
    }
    let environment =
        observe_requirement_environment(bus, edit, &statement, snapshot.answer.environment())?;
    let requirement_change = requirements::prepare(
        entry,
        command,
        edit,
        &statement.requirements,
        &statement.resources,
        environment,
        at,
    )?;
    let route = flow.map(|flow| {
        let workflow = wire::WorkflowHead {
            flow: flow.clone(),
            ..entry.workflow().clone()
        };
        let mut completion = entry.completion().clone();
        completion.targets.retain(|phase, _| flow.contains(phase));
        completion.asked.retain(|phase, _| flow.contains(phase));
        RoutePublication {
            change: body::TaskFlowEdit {
                previous: entry.workflow().flow.clone(),
                current: flow,
            },
            completion: (completion != *entry.completion()).then_some(completion),
            workflow,
        }
    });
    let workflow = route
        .as_ref()
        .map_or_else(|| entry.workflow().clone(), |route| route.workflow.clone());
    let completion = route
        .as_ref()
        .and_then(|route| route.completion.clone())
        .unwrap_or_else(|| entry.completion().clone());
    let mut claims = entry.claim_bodies();
    if let Some(transitioned) = requirement_change.lease() {
        for (claim, lease) in &mut claims {
            if claim.claimed == transitioned.claimed {
                *lease = Some(transitioned.clone());
            }
        }
    }
    let post = Entry::new(statement.clone(), workflow.clone(), completion, claims)
        .map_err(|error| {
            refusal(
                "task::edit-refused",
                Some(error.to_string()),
                "submit a statement the board admits",
            )
        })?
        .with_design(entry.design())
        .with_creator_team(entry.creator_team());
    planning::refuse_disposed_construction(snapshot, &statement, &[])?;
    // The command carries exact statement, workflow, and completion witnesses;
    // their one publication guard refuses every concurrent route, advancement,
    // arrival, invitation, or staffing move.
    let mut status = edit_status(entry.statement().taste.as_ref(), statement.taste.as_ref());
    if edit.term.is_some() {
        let items = wire::StatementItem::each(&statement.term).collect::<Vec<_>>();
        let handles = items
            .iter()
            .map(|item| item.handle(&items))
            .collect::<Vec<_>>()
            .join(",");
        status.push_str("; item handles ");
        status.push_str(if handles.is_empty() { "none" } else { &handles });
    }
    let publication = CorrectionPublication {
        ownership: snapshot.ownership(&post),
        next: next_action(snapshot, &post),
        status,
        at,
        requirement_change,
        route,
    };
    Ok((statement, publication))
}

/// Add only a proposed trust digest absent from the live-task snapshot.
fn observe_requirement_environment(
    bus: &Bus,
    edit: &TaskEdit,
    statement: &TaskHead,
    mut environment: wire::Environment,
) -> Result<wire::Environment, Outcome<CommandAnswer>> {
    if edit.requirements.is_none() {
        return Ok(environment);
    }
    let required_trust = statement
        .requirements
        .as_slice()
        .iter()
        .find_map(|requirement| match requirement {
            wire::EnvironmentRequirement::TrustGrant { value } => Some(value),
            _ => None,
        });
    if let Some(digest) = required_trust
        && environment.trust_head(digest).is_none()
    {
        let observed = bus
            .trust_head(digest)
            .map_err(|error| store_fault("task::require-trust-unreadable", error.to_string()))?;
        if let Some(head) = observed {
            environment = environment.with_trust_heads([head.body().clone()]);
        }
    }
    Ok(environment)
}

/// Read a corrected statement against the taste policy and move its hold.
///
/// A match records the hold, or counts one more recheck of a standing one; a
/// clear read clears it. Only the statement moves: the task's phase, flow,
/// and claim stay exactly as they were.
fn review_taste(
    repository: Option<Host<'_>>,
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    statement: &mut TaskHead,
) -> Result<(), Outcome<CommandAnswer>> {
    let review = task_taste_review(
        repository,
        snapshot,
        command.actor(),
        &statement.title,
        &statement.pitch,
        statement.description.as_deref(),
        &statement.term,
    )?;
    resync_taste_hold(&mut statement.taste, review.as_ref())
}

/// The edit answer's status, naming where the edit left the taste hold.
fn edit_status(before: Option<&wire::TasteHold>, after: Option<&wire::TasteHold>) -> String {
    match (before, after) {
        (_, Some(_)) => "edited; taste_review required".to_owned(),
        (Some(_), None) => "edited; taste_review cleared".to_owned(),
        (None, None) => "edited".to_owned(),
    }
}

/// Build the statement a correction publishes, or the refusal it earns.
///
/// Only fields the caller named move; the rest ride through from the filed
/// statement untouched. A field this refuses is refused before anything is
/// written, so a correction that names two fields and gets one wrong publishes
/// neither. A Draft's replacement route returns beside its statement.
fn corrected_statement(
    entry: &Entry,
    edit: &TaskEdit,
) -> Result<(TaskHead, Option<Vec<wire::Phase>>), Outcome<CommandAnswer>> {
    if edit.title.is_none()
        && edit.pitch.is_none()
        && edit.description.is_none()
        && edit.term.is_none()
        && edit.requirements.is_none()
        && edit.intent.is_none()
        && edit.priority.is_none()
        && edit.topic.is_none()
        && edit.flow.is_none()
    {
        return Err(refusal(
            "task::edit-empty",
            Some(edit.task.to_string()),
            "name at least one statement field, environmental requirement edit, intent edit, or complete flow",
        ));
    }
    let flow = edit
        .flow
        .as_deref()
        .map(|flow| corrected_flow(entry, flow))
        .transpose()?;
    let mut statement = entry.statement().clone();
    if let Some(title) = &edit.title {
        statement.title = title.clone();
    }
    if let Some(pitch) = &edit.pitch {
        statement.pitch = pitch.trim().to_owned();
    }
    if let Some(description) = &edit.description {
        statement.description = (!description.is_empty()).then(|| description.clone());
    }
    if let Some(term) = &edit.term {
        statement.term = term.clone();
    }
    if let Some(requirements) = &edit.requirements {
        let authored = requirements.requirements();
        statement.resources = authored.compiled_resources();
        statement.requirements = authored.without_resources();
    }
    if let Some(intent) = edit.intent {
        // A Draft has not declared any purpose yet; its author may revise it.
        if statement.intent != intent.intent() && statement.intent.is_some() && !entry.is_draft() {
            return Err(refusal(
                "task::planning-intent-stable",
                Some(entry.handle().to_string()),
                "retain the declared planning purpose through construction and review",
            ));
        }
        statement.intent = intent.intent();
    }
    if let Some(priority) = &edit.priority {
        statement.priority = Some(declared_priority(priority)?);
    }
    if let Some(topic) = &edit.topic {
        // An unplaced task keeps a relative tail for its placement at the
        // close of its first phase; a complete topic places it now.
        statement.topic = match topic {
            wire::TopicInput::Tail(tail) if statement.topic.path.is_none() => {
                wire::TaskTopic::unplaced(Some(tail.clone()))
            }
            _ => exact_topic(topic)?,
        };
    }
    // A Design walks one phase: judge the pair this edit leaves, so an intent
    // and a Draft route edited together are checked as they will stand.
    if edit.intent.is_some() || flow.is_some() {
        planning::admit_design_flow(
            statement.intent,
            flow.as_deref().unwrap_or(&entry.workflow().flow),
        )?;
    }
    Ok((statement, flow))
}

/// Admit a complete replacement route while freezing every started phase.
fn corrected_flow(entry: &Entry, flow: &str) -> Result<Vec<wire::Phase>, Outcome<CommandAnswer>> {
    let flow = phases(Some(flow))?;
    if entry.is_draft() {
        return Ok(flow);
    }
    let current = entry
        .workflow()
        .flow
        .iter()
        .position(|phase| phase == &entry.workflow().phase)
        .expect("validated workflow contains its current phase");
    if flow.get(..=current) != entry.workflow().flow.get(..=current) {
        return Err(refusal(
            "task::flow-prefix",
            Some(entry.handle().to_string()),
            "preserve every completed phase and the current phase; edit only the unstarted suffix",
        ));
    }
    let old_suffix = &entry.workflow().flow[current + 1..];
    let new_suffix = &flow[current + 1..];
    if old_suffix.contains(&Phase::Review) && !new_suffix.contains(&Phase::Review) {
        return Err(refusal(
            "task::flow-review-required",
            Some(entry.handle().to_string()),
            "retain the activated route's future review gate",
        ));
    }
    Ok(flow)
}

fn admit_edit_authority(
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    command: &wire::AdmittedCommand,
    at: Instant,
) -> Result<(), Outcome<CommandAnswer>> {
    if command.holds(wire::principal::PrincipalRole::Operator) {
        return Ok(());
    }
    let authority = if entry.is_draft() {
        let Some(preparation) = &entry.statement().preparation else {
            return Ok(());
        };
        snapshot
            .entries
            .iter()
            .find(|candidate| candidate.born() == &preparation.controller)
            .ok_or_else(|| {
                refusal(
                    "task::controller-absent",
                    Some(preparation.controller.to_string()),
                    "read the controlled draft with its current controller",
                )
            })?
    } else {
        entry
    };
    let active = board::Active::admit(authority, command.actor(), at).map_err(|error| {
        refusal(
            "task::flow-authority",
            Some(error.to_string()),
            "edit an active route only as its exact current holder, or edit a controlled draft while holding its controller",
        )
    })?;
    if active.epoch() != command.epoch() {
        return Err(refusal(
            "claim::epoch-moved",
            Some(active.epoch().to_string()),
            "run spice task next to rebind the claim under the current resident generation",
        ));
    }
    let held = authority
        .claim_of(command.actor())
        .expect("active admission proves the actor's claim");
    let witnessed = command.expected().iter().any(|witness| {
        matches!(
            witness,
            Witness::Claim { task, claimed, .. }
                if task == authority.born() && *claimed == Some(held.claimed())
        )
    });
    if !witnessed {
        return Err(refusal(
            "task::flow-authority-witness",
            Some(authority.handle().to_string()),
            "read the current holder claim and retry the route edit",
        ));
    }
    Ok(())
}

fn declared_priority(priority: &str) -> Result<wire::TaskPriority, Outcome<CommandAnswer>> {
    board::priority_from_authored(priority).ok_or_else(|| {
        refusal(
            "task::edit-priority-unknown",
            Some(priority.to_owned()),
            "name one of critical, high, medium, low, or none, or the letter the board prints",
        )
    })
}

/// Write one corrected statement as a fresh revision attributed to its editor.
struct CorrectionPublication {
    ownership: wire::intent::TaskOwnership,
    next: Option<Box<str>>,
    /// The answer's status: the edit, plus how it moved the taste hold.
    status: String,
    at: Instant,
    requirement_change: requirements::Change,
    /// The replacement route and any staffing head it changes.
    route: Option<RoutePublication>,
}

struct RoutePublication {
    workflow: wire::WorkflowHead,
    completion: Option<wire::CompletionHead>,
    change: body::TaskFlowEdit,
}

fn publish_correction(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    edit: &TaskEdit,
    statement: TaskHead,
    publication: CorrectionPublication,
    snapshot: &ValidatedSnapshot,
) -> Outcome<CommandAnswer> {
    let CorrectionPublication {
        ownership,
        next,
        status,
        at,
        requirement_change,
        route,
    } = publication;
    let revision = u32::try_from(statement.revision.get())
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let origin = match task_origin(&statement.born) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    snapshot.guard_ownership(&mut write);
    requirement_change.guard(&mut write);
    let flow = route.as_ref().map(|route| route.change.clone());
    let edited = match write.fact(
        body::TaskEdited {
            intent: edit.intent,
            task: statement.born.clone(),
            revision,
            dependencies: None,
            flow,
        },
        FactCause::Root,
    ) {
        Ok(fact) => fact,
        Err(error) => return store_fault("task::edit-build", error.to_string()),
    };
    if let Some(route) = route {
        let RoutePublication {
            workflow,
            completion,
            ..
        } = route;
        write.workflow_head(statement.born.clone(), edited, move |_| workflow);
        if let Some(completion) = completion {
            write.completion_head(statement.born.clone(), edited, move |_| completion);
        }
    }
    let phase = snapshot
        .entries
        .iter()
        .find(|entry| entry.born() == &statement.born)
        .map_or(Phase::Draft, |entry| entry.workflow().phase);
    let rendered = statement.handle(phase);
    let taste = answered_taste(command.actor(), statement.taste.as_ref());
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
    write.task_head(edited, move |position| {
        published_statement(
            TaskHead {
                revision: position,
                ..statement
            },
            position,
        )
    });
    requirement_change.write_lease(&mut write, edited);
    let answer = wire::intent::TaskAnswer {
        diagnostic: None,
        ownership: Some(ownership),
        placement: None,
        created: Default::default(),
        taste,
        completion_contract,
        task: rendered,
        status,
        completion: None,
        claim: None,
        closed: Vec::new(),
        next,
    };
    if let Err(error) = write.command_result(
        request(command, edit.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("task::edit-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::Task(Box::new(answer)))
}
