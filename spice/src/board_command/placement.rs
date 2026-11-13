//! Topic-only placement previews bind the task and the one scored routing capture.

use std::sync::Arc;

use wire::intent::{TaskPlace, TaskPlaceAnswer, TaskRoute};

use super::*;

struct Plan {
    routing: routing::Capture,
    admission: drive::Admission,
    topic: Option<wire::TaskTopic>,
    answer: TaskPlaceAnswer,
}

pub(super) fn run(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    request: &TaskPlace,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let snapshot = match validated_working_snapshot(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        std::slice::from_ref(&request.task),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(entry) = task_entry(&snapshot.entries, &request.task) else {
        return refusal(
            "task::unknown",
            Some(request.task.to_string()),
            "read board and name one current task handle",
        );
    };
    if !entry.workflow().status.is_pending() {
        return refusal(
            "task::settled",
            Some(entry.handle().to_string()),
            "place a task only while it is pending",
        );
    }
    if entry
        .statement()
        .intent
        .is_some_and(wire::TaskIntent::is_opportunistic_design)
    {
        return refusal(
            "task::oops-design-internal",
            Some(entry.handle().to_string()),
            "the OOPS Design keeps its fixed topic; task file can activate its saved Draft",
        );
    }
    let plan = match prepare(bus, &snapshot, entry, command.actor()) {
        Ok(plan) => plan,
        Err(outcome) => return outcome,
    };
    let Some(applied) = request.apply.as_ref() else {
        return Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(snapshot.cursor),
            body: CommandAnswer::TaskPlace(Box::new(plan.answer)),
        });
    };
    if plan.answer.preview.as_ref() != Some(applied) {
        return refusal(
            "task::place-preview-moved",
            plan.answer.drive_refusal.clone(),
            "preview task place again and apply its exact available digest",
        );
    }
    publish(bus, command, request, entry, plan)
}

fn prepare(
    bus: &Bus,
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    actor: &Actor,
) -> Result<Plan, Outcome<CommandAnswer>> {
    let routing = routing::Capture::new_for_team(
        bus,
        snapshot,
        entry.creator_team(),
        routing::Statement::task(entry.statement(), &entry.workflow().phase),
        routing::OriginView::of(bus, &entry.statement().provenance)?,
    )?;
    let restricted = routing.restricted();
    let route = route_answer(&restricted);
    let unrestricted = route_answer(&routing.unrestricted());
    let admission = drive::Admission::capture(
        actor,
        &snapshot.answer.residency,
        Arc::clone(&snapshot.policy),
    )?;
    let mut answer = TaskPlaceAnswer {
        task: entry.handle().clone(),
        from: entry.topic().cloned(),
        unrestricted: (unrestricted != route).then_some(unrestricted),
        route,
        ownership: None,
        phase: entry.workflow().phase,
        drive_refusal: None,
        preview: None,
        applied: false,
        next: None,
    };
    let topic = restricted
        .ok()
        .map(|routed| routed.topic)
        .filter(|topic| topic.path.is_some());
    if let Some(topic) = &topic {
        // A draft is judged at the phase it enters when it leaves draft.
        let phase = entry
            .workflow()
            .flow
            .get(1)
            .copied()
            .filter(|_| entry.is_draft())
            .unwrap_or(entry.workflow().phase);
        let decision = admission.decide(topic.path.as_ref(), &phase, entry.creator_team());
        answer.drive_refusal = decision.refusal;
        let owners = snapshot
            .owners
            .owners_for(topic, &phase, entry.creator_team());
        answer.ownership = Some(wire::intent::TaskOwnership {
            teams: owners.teams.clone(),
            reserved: owners.reserved,
        });
        if entry.is_draft() || answer.drive_refusal.is_none() {
            let digest = preview_digest(entry, actor, &routing, topic, &answer);
            answer.next = Some(format!(
                "spice task place {} --apply={digest}",
                entry.handle()
            ));
            answer.preview = Some(digest);
        }
    }
    Ok(Plan {
        routing,
        admission,
        topic,
        answer,
    })
}

fn route_answer(route: &Result<routing::Routed, board::routing::RouteError>) -> TaskRoute {
    match route {
        Ok(routed) => routed.route.clone(),
        Err(error) => TaskRoute::Unavailable {
            reason: error.reason().to_owned(),
            detail: error.to_string(),
        },
    }
}

fn preview_digest(
    entry: &Entry,
    actor: &Actor,
    routing: &routing::Capture,
    topic: &wire::TaskTopic,
    answer: &TaskPlaceAnswer,
) -> Digest {
    Digest::compute(
        Domain::TaskPlacementPlan,
        &wire::encode(&(
            entry.born(),
            entry.statement().revision,
            entry.workflow(),
            actor,
            routing.fingerprint(),
            topic,
            &answer.from,
            &answer.ownership,
            answer.phase,
            &answer.drive_refusal,
        ))
        .expect("typed placement preview encodes"),
    )
}

fn publish(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    request: &TaskPlace,
    entry: &Entry,
    mut plan: Plan,
) -> Outcome<CommandAnswer> {
    let Some(topic) = plan.topic else {
        return protocol_fault(
            "task::place-pick-missing",
            "an admitted preview requires a pick",
        );
    };
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    let origin = match task_origin(entry.born()) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let mut statement = entry.statement().clone();
    statement.topic = topic;
    let revision = u32::try_from(statement.revision.get())
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let mut write = JournalWrite::new(command.actor().clone(), origin, at, Producer::CommandDoor);
    guard(&mut write, command.expected().to_vec());
    plan.routing.guard(&mut write);
    plan.admission.guard(&mut write);
    let edited = match write.fact(
        body::TaskEdited {
            intent: None,
            task: statement.born.clone(),
            revision,
            dependencies: None,
            flow: None,
        },
        FactCause::Root,
    ) {
        Ok(edited) => edited,
        Err(error) => return store_fault("task::place-build", error.to_string()),
    };
    plan.answer.task = statement.handle(entry.workflow().phase);
    write.task_head(edited, move |position| TaskHead {
        revision: position,
        ..statement
    });
    plan.answer.applied = true;
    plan.answer.preview = None;
    plan.answer.next = entry.is_draft().then(|| {
        if plan.answer.drive_refusal.is_some() {
            format!("spice task place {}", plan.answer.task)
        } else {
            format!("spice task file {}", plan.answer.task)
        }
    });
    if let Err(error) = write.command_result(
        super::request(command, request.clone()),
        command.epoch(),
        plan.answer.clone(),
    ) {
        return store_fault("task::place-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::TaskPlace(Box::new(plan.answer)))
}
