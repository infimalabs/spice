use base64::Engine as _;

use super::*;

/// Read one task's current evidence from one immutable journal barrier.
pub(in crate::board_command) fn read_task_detail(
    bus: &Bus,
    repository: Option<Host<'_>>,
    detail: &wire::intent::TaskDetail,
    principal: wire::Actor,
) -> Outcome<CommandAnswer> {
    let observed = match bus.task_snapshot() {
        Ok(observed) => observed,
        Err(error) => return store_fault("board::heads-unreadable", error.to_string()),
    };
    let selected = std::collections::BTreeSet::from([detail.task.clone()]);
    let projection = expand_task_selection(&observed, vec![detail.task.clone()]);
    let snapshot = match validated_snapshot_from_with_occupant(
        &observed,
        repository.map(|host| host.repository),
        principal,
        None,
        Some(&projection),
        false,
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(mut task) = snapshot
        .answer
        .tasks
        .into_iter()
        .find(|task| task.statement.body.born == detail.task)
    else {
        return refusal(
            "task::detail-unknown",
            Some(detail.task.to_string()),
            "read the board and request one current durable task identity",
        );
    };
    let at = match current_instant() {
        Ok(at) => at,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) =
        super::invitations::attach_invitations(bus, &mut task, &snapshot.answer.addresses, at)
    {
        return outcome;
    }
    let media = match task_media(repository, &detail.task) {
        Ok(media) => media,
        Err(outcome) => return outcome,
    };
    let description_quotes =
        match description_quotes(bus, repository, task.statement.body.description.as_deref()) {
            Ok(quotes) => quotes,
            Err(outcome) => return outcome,
        };
    let mut record = task_record(observed.facts(), &selected);
    let closure = record.closures.remove(&detail.task);
    let deletion = record.deletions.remove(&detail.task);
    let disposition = record.dispositions.remove(&detail.task);
    let steps = record.steps.remove(&detail.task).unwrap_or_default();
    let pass = pass_final_word(observed.facts(), &task, &steps);
    let review = latest_review(steps);
    let origin = match &task.statement.body.provenance {
        wire::TaskProvenance::Steering(key) => match steering_origin(bus, repository, key) {
            Ok(origin) => origin,
            Err(outcome) => return outcome,
        },
        _ => None,
    };
    snapshot_reply(
        snapshot.cursor,
        CommandAnswer::TaskDetail(Box::new(wire::intent::TaskDetailAnswer {
            task,
            media,
            closure,
            deletion,
            disposition,
            review,
            pass,
            origin,
            description_quotes,
        })),
    )
}

/// Consume one selected task's ordered history, keeping only its newest review.
fn latest_review(
    steps: Vec<wire::intent::TaskDetailStep>,
) -> Option<wire::intent::TaskDetailReview> {
    steps.into_iter().rev().find_map(|step| {
        let wire::intent::TaskStep::Reviewed {
            verdict,
            status,
            text,
            ..
        } = step.step
        else {
            return None;
        };
        Some(wire::intent::TaskDetailReview {
            position: step.position,
            at: step.at,
            reviewer: step.author,
            verdict,
            status,
            text,
        })
    })
}

/// The completed Design pass's final word: its finishing `task done` arrival
/// and that lane's latest authored post in the Design's room since the pass
/// opened.
fn pass_final_word(
    facts: &bus::TaskFacts,
    task: &wire::intent::BoardTask,
    steps: &[wire::intent::TaskDetailStep],
) -> Option<wire::intent::TaskDetailPass> {
    let cycle = &task.design_cycle;
    let pass = cycle.pass?;
    let finish = steps
        .iter()
        .rev()
        .find(|step| {
            step.position <= pass
                && matches!(
                    step.step,
                    wire::intent::TaskStep::Done {
                        phase: wire::Phase::Design,
                        ..
                    }
                )
        })?
        .clone();
    let room = &task.statement.body.born;
    let latest = facts
        .iter()
        .filter(|envelope| {
            envelope.actor == finish.author
                && envelope.seq <= pass
                && cycle.opened.is_none_or(|opened| envelope.seq > opened)
        })
        .filter_map(|envelope| {
            let (posted, text) = recorded_note(envelope)?;
            (posted == *room).then_some(wire::intent::TaskDetailPost {
                position: envelope.seq,
                at: envelope.at,
                text,
            })
        })
        .next_back();
    Some(wire::intent::TaskDetailPass { finish, latest })
}

/// Durable history on the named tasks, each task's rows in ascending journal
/// position.
#[derive(Default)]
pub(super) struct TaskRecord {
    /// Every task or claim fact whose origin is an acknowledged steering key,
    /// and every operator directive a holder acknowledged while holding it.
    pub(super) acknowledgments:
        std::collections::BTreeMap<wire::TaskStamp, Vec<wire::intent::TaskAcknowledgment>>,
    /// The operator's closure of each closed task; a settled task closes once.
    pub(super) closures:
        std::collections::BTreeMap<wire::TaskStamp, wire::intent::TaskDetailClosure>,
    /// The operator's withdrawal of each deleted task; a settled task settles
    /// once.
    pub(super) deletions:
        std::collections::BTreeMap<wire::TaskStamp, wire::intent::TaskDetailDeletion>,
    /// The no-construction disposition each Plan recorded, through settlement.
    pub(super) dispositions:
        std::collections::BTreeMap<wire::TaskStamp, wire::intent::TaskDetailDisposition>,
    /// Every done, phase advance, and review verdict.
    pub(super) steps:
        std::collections::BTreeMap<wire::TaskStamp, Vec<wire::intent::TaskDetailStep>>,
}

/// The authored observation one fact publishes in a task's room, with that
/// room's task.
pub(in crate::board_command) fn recorded_note(
    envelope: &wire::Envelope<wire::AnyFact>,
) -> Option<(wire::TaskStamp, String)> {
    let published = envelope.fact.read::<wire::rooms::RoomPublished>()?;
    let wire::rooms::RoomPublicationContent::Authored { text } = published.content else {
        return None;
    };
    Some((published.room, text))
}

/// Read the named tasks' closures, workflow steps, and acknowledgments from
/// the snapshot's facts.
pub(super) fn task_record(
    facts: &bus::TaskFacts,
    tasks: &std::collections::BTreeSet<wire::TaskStamp>,
) -> TaskRecord {
    let mut record = TaskRecord::default();
    // The phase each named task's workflow holds, so a completion names the
    // phase it closed; only creation and advance move it.
    let mut phases = std::collections::BTreeMap::new();
    for envelope in facts.iter() {
        if let Some((task, step)) = task_step(envelope, &mut phases, tasks) {
            record
                .steps
                .entry(task)
                .or_default()
                .push(wire::intent::TaskDetailStep {
                    position: envelope.seq,
                    at: envelope.at,
                    author: envelope.actor.clone(),
                    step,
                });
        }
        let note = recorded_note(envelope).filter(|(task, _)| tasks.contains(task));
        if let Some(closed) = envelope.fact.read::<body::TaskClosed>()
            && tasks.contains(&closed.task)
        {
            record.closures.insert(
                closed.task,
                wire::intent::TaskDetailClosure {
                    position: envelope.seq,
                    at: envelope.at,
                    author: envelope.actor.clone(),
                    validation: closed.validation,
                    phase: closed.phase,
                    claimed: closed.claimed,
                },
            );
        }
        if let Some(deleted) = envelope.fact.read::<body::TaskDeleted>()
            && tasks.contains(&deleted.task)
        {
            record.deletions.insert(
                deleted.task,
                wire::intent::TaskDetailDeletion {
                    position: envelope.seq,
                    at: envelope.at,
                    author: envelope.actor.clone(),
                    reason: deleted.reason,
                    claimed: deleted.claimed,
                },
            );
        }
        // A Plan's one disposition persists through its later, plain completions.
        if let Some(done) = envelope.fact.read::<body::TaskDone>()
            && tasks.contains(&done.task)
            && let Some(disposition) = done.disposition
        {
            record.dispositions.insert(
                done.task,
                wire::intent::TaskDetailDisposition {
                    position: envelope.seq,
                    at: envelope.at,
                    author: envelope.actor.clone(),
                    disposition,
                    validation: done.validation,
                },
            );
        }
        let Some(key) = envelope
            .origin
            .as_str()
            .parse::<wire::TaskProvenance>()
            .ok()
            .and_then(|origin| origin.steering_key().map(str::to_owned))
        else {
            continue;
        };
        let kind = envelope.fact.kind();
        let task = match kind.as_str().split_once("::") {
            Some(("task" | "claim", _)) => super::fact_task(envelope),
            _ => note.map(|(task, _)| task),
        };
        let Some(task) = task else {
            continue;
        };
        if tasks.contains(&task) {
            record.acknowledgments.entry(task).or_default().push(
                wire::intent::TaskAcknowledgment {
                    position: envelope.seq,
                    kind,
                    key,
                },
            );
        }
    }
    for (task, steered) in super::steering_during_claims(facts, tasks) {
        let rows = record.acknowledgments.entry(task).or_default();
        rows.extend(steered);
        rows.sort_by_key(|row| row.position);
    }
    record
}

/// The workflow step one fact records on a named task, if it records one.
fn task_step(
    envelope: &wire::Envelope<wire::AnyFact>,
    phases: &mut std::collections::BTreeMap<wire::TaskStamp, wire::Phase>,
    tasks: &std::collections::BTreeSet<wire::TaskStamp>,
) -> Option<(wire::TaskStamp, wire::intent::TaskStep)> {
    match envelope.fact.kind() {
        wire::FactKind::TaskAdded => {
            let added = envelope.fact.read::<body::TaskAdded>()?;
            if tasks.contains(&added.task) {
                phases.insert(added.task, added.phase);
            }
            None
        }
        wire::FactKind::TaskDone => {
            let done = envelope.fact.read::<body::TaskDone>()?;
            let phase = *phases.get(&done.task)?;
            Some((
                done.task,
                wire::intent::TaskStep::Done {
                    phase,
                    validation: done.validation,
                    disposition: done.disposition,
                },
            ))
        }
        // Every participant of a shared round, its closer included, arrives
        // through its own fact. Only `task done` arrivals carry an operation;
        // a shared review verdict records its own step.
        wire::FactKind::TaskArrived => {
            let arrived = envelope.fact.read::<body::TaskArrived>()?;
            if arrived.operation.is_none() || !tasks.contains(&arrived.task) {
                return None;
            }
            Some((
                arrived.task,
                wire::intent::TaskStep::Done {
                    phase: arrived.phase,
                    validation: arrived.validation,
                    disposition: arrived.disposition,
                },
            ))
        }
        wire::FactKind::TaskPhased => {
            let phased = envelope.fact.read::<body::TaskPhased>()?;
            let phase = phases.get_mut(&phased.task)?;
            phase.clone_from(&phased.phase);
            Some((
                phased.task,
                wire::intent::TaskStep::Phased {
                    phase: phased.phase,
                },
            ))
        }
        wire::FactKind::TaskReviewed => {
            let reviewed = envelope.fact.read::<body::TaskReviewed>()?;
            if !tasks.contains(&reviewed.task) {
                return None;
            }
            Some((
                reviewed.task,
                wire::intent::TaskStep::Reviewed {
                    verdict: reviewed.verdict,
                    status: reviewed.status,
                    text: reviewed.finding.or(reviewed.note),
                    followups: reviewed.followups,
                },
            ))
        }
        wire::FactKind::TaskConcluded => {
            let concluded = envelope.fact.read::<body::TaskConcluded>()?;
            if !tasks.contains(&concluded.task) {
                return None;
            }
            Some((
                concluded.task,
                wire::intent::TaskStep::Concluded {
                    conclusion: concluded.conclusion,
                },
            ))
        }
        _ => None,
    }
}

/// The resident's presentation of every quote link in `description`, so a
/// task description shows its quoted sources as an agent message does.
fn description_quotes(
    bus: &Bus,
    repository: Option<Host<'_>>,
    description: Option<&str>,
) -> Result<Vec<wire::QuotePresentation>, Outcome<CommandAnswer>> {
    let (Some(repository), Some(description)) = (repository, description) else {
        return Ok(Vec::new());
    };
    if spice::quote::QuoteMessage::parse(description)
        .quotes()
        .is_empty()
    {
        return Ok(Vec::new());
    }
    let state = repository
        .admit_state()
        .map_err(|error| repository_state_failure(&error))?;
    Ok(spice::quote::Resolver::new(&state, &bus.reader()).present(description))
}

/// The directive a steering origin's `key` names, presented as a message
/// quotes it: whole text, recorded author and disposition, and every quote it
/// carries. A key no directive carries has none.
fn steering_origin(
    bus: &Bus,
    repository: Option<Host<'_>>,
    key: &str,
) -> Result<Option<wire::intent::TaskDetailOrigin>, Outcome<CommandAnswer>> {
    let Some(repository) = repository else {
        return Ok(None);
    };
    let owned = key.to_owned();
    let Some(head) = bus
        .read(move |read| read.steering_key(&owned))
        .map_err(|error| store_fault("task::steering-request-unreadable", error.to_string()))?
    else {
        return Ok(None);
    };
    let head = head.body();
    let target = wire::QuoteTarget::Steering {
        key: head.key.clone(),
        digest: wire::QuoteTarget::text_digest(&head.text),
    };
    let state = repository
        .admit_state()
        .map_err(|error| repository_state_failure(&error))?;
    Ok(spice::quote::Resolver::new(&state, &bus.reader())
        .present(&target.markdown())
        .pop()
        .map(|directive| wire::intent::TaskDetailOrigin {
            published: head.published,
            directive,
        }))
}

fn task_media(
    repository: Option<Host<'_>>,
    task: &wire::TaskStamp,
) -> Result<Vec<wire::Media>, Outcome<CommandAnswer>> {
    let Some(repository) = repository else {
        return Ok(Vec::new());
    };
    let state = repository
        .admit_state()
        .map_err(|error| repository_state_failure(&error))?;
    let store = artifact::Store::new(state.artifacts(), state.artifacts_lock());
    let mut encoded_bytes = 0_usize;
    let payloads = store
        .show_matching(task, |artifact| {
            let Ok(bytes) = usize::try_from(artifact.bytes) else {
                return false;
            };
            if !artifact.media_type.is_image() || bytes == 0 || bytes > wire::media::MAX_MEDIA_BYTES
            {
                return false;
            }
            let data_url_bytes =
                format!("data:{};base64,", artifact.media_type).len() + bytes.div_ceil(3) * 4;
            let Some(total) = encoded_bytes.checked_add(data_url_bytes) else {
                return false;
            };
            if total > wire::media::MAX_MEDIA_DATA_URL_BYTES {
                return false;
            }
            encoded_bytes = total;
            true
        })
        .map_err(artifact_operation_failure)?;
    let media = payloads
        .into_iter()
        .filter_map(|(artifact, bytes)| {
            wire::Media::from_base64(
                artifact.name,
                artifact.media_type.to_string(),
                &base64::engine::general_purpose::STANDARD.encode(bytes),
            )
            .ok()
        })
        .take(wire::media::MAX_MEDIA_ITEMS)
        .collect::<Vec<_>>();
    debug_assert!(wire::media::admit_media(&media).is_ok());
    Ok(media)
}
