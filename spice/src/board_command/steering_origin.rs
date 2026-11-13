use super::*;

/// Every distinct steering key on one task record: the task's own origin
/// first, then each other key in the order its first carrying fact landed.
pub(super) fn task_acknowledgment_keys<'a>(
    origin: &'a wire::TaskProvenance,
    acknowledgments: &'a [wire::intent::TaskAcknowledgment],
) -> Vec<&'a str> {
    let mut keys = Vec::new();
    for key in origin
        .steering_key()
        .into_iter()
        .chain(acknowledgments.iter().map(|row| row.key.as_str()))
    {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// Every steering key that caused non-deleted work, with that work's handles.
///
/// Open reads select only unsettled rows, yet a direction's Work outlives its
/// completion, so the index spans the whole task snapshot.
pub(super) fn steering_origins(
    observed: &bus::TaskSnapshot,
) -> std::collections::BTreeMap<String, Vec<wire::TaskHandle>> {
    let mut origins = std::collections::BTreeMap::<String, Vec<wire::TaskHandle>>::new();
    for heads in observed.tasks() {
        if heads.workflow().body().status.is_deleted() {
            continue;
        }
        let statement = heads.statement().body();
        if let Some(key) = statement.provenance.steering_key() {
            origins
                .entry(key.to_owned())
                .or_default()
                .push(statement.handle(heads.workflow().body().phase));
        }
    }
    for handles in origins.values_mut() {
        handles.sort();
    }
    origins
}

/// Operator steering each named task's holder acknowledged while it held the
/// task, in journal order, whatever the phase.
///
/// A lane holds one task from a win, carry, or rebound until a release, lapse,
/// or settling fact names that task. Peer messages and resident notices are
/// not operator steering. A key another task cites as its origin was captured
/// as that task's work, so it stays off the task its acknowledger then held.
pub(super) fn steering_during_claims(
    facts: &bus::TaskFacts,
    tasks: &std::collections::BTreeSet<wire::TaskStamp>,
) -> std::collections::BTreeMap<wire::TaskStamp, Vec<wire::intent::TaskAcknowledgment>> {
    let mut held = std::collections::HashMap::<wire::CanonicalUuid, wire::TaskStamp>::new();
    let mut operator = std::collections::HashSet::<String>::new();
    let mut captured = std::collections::HashMap::<String, wire::TaskStamp>::new();
    let mut steered =
        std::collections::BTreeMap::<wire::TaskStamp, Vec<wire::intent::TaskAcknowledgment>>::new();
    for envelope in facts.iter() {
        let kind = envelope.fact.kind();
        match kind {
            wire::FactKind::SteerSent if matches!(envelope.actor, wire::Actor::Principal(_)) => {
                if let Some(sent) = envelope.fact.read::<body::SteerSent>() {
                    operator.insert(sent.key);
                }
            }
            wire::FactKind::SteerAcked => {
                if let Some(acked) = envelope.fact.read::<body::SteerAcked>()
                    && operator.contains(&acked.key)
                    && let Some(task) = acked
                        .recipient
                        .lane()
                        .and_then(|lane| held.get(&lane))
                        .filter(|task| tasks.contains(*task))
                {
                    steered.entry(task.clone()).or_default().push(
                        wire::intent::TaskAcknowledgment {
                            position: envelope.seq,
                            kind,
                            key: acked.key,
                        },
                    );
                }
            }
            wire::FactKind::TaskAdded => {
                if let Ok(origin) = envelope.origin.as_str().parse::<wire::TaskProvenance>()
                    && let Some(key) = origin.steering_key()
                    && let Some(task) = fact_task(envelope)
                {
                    captured.insert(key.to_owned(), task);
                }
            }
            wire::FactKind::ClaimWon
            | wire::FactKind::ClaimCarried
            | wire::FactKind::ClaimRebound => {
                if let Some((task, lane)) = claim_holder(envelope) {
                    held.retain(|_, holding| *holding != task);
                    held.insert(lane, task);
                }
            }
            wire::FactKind::ClaimFreed
            | wire::FactKind::ClaimLapsed
            | wire::FactKind::TaskDone
            | wire::FactKind::TaskReviewed
            | wire::FactKind::TaskDeferred
            | wire::FactKind::TaskDeleted
            | wire::FactKind::TaskClosed
            | wire::FactKind::TaskConcluded => {
                if let Some(task) = fact_task(envelope) {
                    held.retain(|_, holding| *holding != task);
                }
            }
            _ => {}
        }
    }
    steered.retain(|task, rows| {
        rows.retain(|row| captured.get(&row.key).is_none_or(|owner| owner == task));
        !rows.is_empty()
    });
    steered
}

/// The lane a win, carry, or rebound binds its task to.
fn claim_holder(
    envelope: &wire::Envelope<wire::AnyFact>,
) -> Option<(wire::TaskStamp, wire::CanonicalUuid)> {
    let fact = &envelope.fact;
    fact.read::<body::ClaimWon>()
        .map(|won| (won.task, won.lane))
        .or_else(|| {
            fact.read::<body::ClaimCarried>()
                .map(|carried| (carried.task, carried.lane))
        })
        .or_else(|| {
            fact.read::<body::ClaimRebound>()
                .map(|rebound| (rebound.task, rebound.lane))
        })
}

/// The task a task or claim fact names.
pub(super) fn fact_task(envelope: &wire::Envelope<wire::AnyFact>) -> Option<wire::TaskStamp> {
    envelope
        .fact
        .body()
        .get("task")
        .and_then(serde_json::Value::as_str)
        .and_then(|task| task.parse().ok())
}

/// Every steering key that shaped one task, resolved to its directive and the
/// answer that retired it: the task's origin, then what its holders
/// acknowledged while holding it.
pub(super) fn task_steering_origin_output(
    bus: &Bus,
    facts: &bus::TaskFacts,
    entry: &Entry,
) -> Result<Option<Box<str>>, Outcome<CommandAnswer>> {
    let steered = steering_during_claims(
        facts,
        &std::collections::BTreeSet::from([entry.born().clone()]),
    )
    .remove(entry.born())
    .unwrap_or_default();
    let keys = task_acknowledgment_keys(&entry.statement().provenance, &steered)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return Ok(None);
    }
    let named = keys.join(" ");
    let blocks = bus
        .read(move |read| {
            keys.iter()
                .map(|key| {
                    read_steering_provenance(read, key)
                        .map(|provenance| render_task_steering_origin(key, provenance.as_ref()))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|error| {
            store_fault(
                "task::steering-index-unreadable",
                format!("cannot inspect steering origins {named}: {error}"),
            )
        })?;
    Ok(Some(blocks.join("\n").into_boxed_str()))
}

/// One steering key's source directive and the transcript locus that answered it.
pub(super) struct SteeringProvenance {
    presentation: bus::SteeringPresentation,
    /// The directive's author as the principal roles resolve it.
    author: wire::Actor,
    answer: SteeringAnswer,
}

enum SteeringAnswer {
    /// A lane acknowledged or declined the directive at a transcript locus.
    Answered {
        disposition: &'static str,
        lane: wire::CanonicalUuid,
        at: wire::Instant,
        /// Executable session context at the answering record, or why none exists.
        context: Result<String, &'static str>,
    },
    /// No transcript answer retired the directive.
    Unanswered(&'static str),
}

/// Read one key's directive, its author, and the session context where a lane
/// answered it, all inside one immutable journal snapshot.
pub(super) fn read_steering_provenance(
    read: &journal::JournalRead<'_>,
    key: &str,
) -> Result<Option<SteeringProvenance>, journal::GuardError> {
    let Some(presentation) = bus::SteeringPresentation::read(read, key)? else {
        return Ok(None);
    };
    let current = presentation.current();
    let answered = current
        .fact
        .read::<wire::body::SteerAcked>()
        .and_then(|body| {
            Some((
                "acked",
                body.recipient.lane()?,
                body.evidence.transcript()?.1,
            ))
        })
        .or_else(|| {
            current
                .fact
                .read::<wire::body::SteerDeclined>()
                .and_then(|body| {
                    Some((
                        "declined",
                        body.recipient.lane()?,
                        body.evidence.transcript()?.1,
                    ))
                })
        });
    let answer = match answered {
        Some((disposition, lane, locus)) => SteeringAnswer::Answered {
            disposition,
            lane,
            at: current.at,
            context: answer_context(read, current.seq, lane, &locus)?,
        },
        None => SteeringAnswer::Unanswered(presentation.head().body().disposition.as_str()),
    };
    let author = presentation.created().actor.clone();
    Ok(Some(SteeringProvenance {
        presentation,
        author,
        answer,
    }))
}

/// `spice session context` at the answering record, naming the resumable
/// session the lane declared on that exact tape at or before the record.
fn answer_context(
    read: &journal::JournalRead<'_>,
    answered: wire::Seq,
    lane: wire::CanonicalUuid,
    locus: &wire::body::TranscriptLocus,
) -> Result<Result<String, &'static str>, journal::GuardError> {
    let source = locus
        .source
        .expect("admitted steering transcript facts carry a tape generation");
    let mut before = answered;
    loop {
        let page = read.recent_fact_page(
            wire::FactKind::LaneSessionDeclared,
            Some(before),
            journal::FactReadBudget::RETAINED,
        )?;
        let declared = page.rows.iter().find_map(|row| {
            row.fact
                .read::<wire::body::LaneSessionDeclared>()
                .filter(|declared| {
                    declared.lane == lane
                        && declared.source == source
                        && (declared.member, declared.offset) <= (locus.member, locus.offset)
                })
        });
        if let Some(declared) = declared {
            return Ok(Ok(shell_words::join([
                "spice".to_owned(),
                "session".to_owned(),
                "context".to_owned(),
                declared.identity,
                "--lane".to_owned(),
                lane.to_string(),
                "--vendor".to_owned(),
                declared.vendor.to_string(),
                "--stream".to_owned(),
                body::TASK_CREATION_CONTEXT_STREAM.to_owned(),
                "--source".to_owned(),
                source.to_string(),
                "--member".to_owned(),
                locus.member.to_string(),
                "--offset".to_owned(),
                locus.offset.to_string(),
            ])));
        }
        let Some(next) = page.continuation else {
            return Ok(Err("session-undeclared"));
        };
        before = next;
    }
}

pub(super) fn render_task_steering_origin(
    key: &str,
    provenance: Option<&SteeringProvenance>,
) -> Box<str> {
    let Some(provenance) = provenance else {
        return format!("origin_steering key {key}\norigin_steering unavailable unresolved-key")
            .into_boxed_str();
    };
    let head = provenance.presentation.head().body();
    let created = provenance.presentation.created();
    let attachments = if head.attachments.is_empty() {
        "-".to_owned()
    } else {
        head.attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| task_steering_attachment_name(key, index, attachment))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let answer = match &provenance.answer {
        SteeringAnswer::Answered {
            disposition,
            lane,
            at,
            context,
        } => format!(
            "origin_steering_answer {disposition} lane={lane} at={at}\n{}",
            context.as_ref().map_or_else(
                |reason| format!("origin_steering_context unavailable {reason}"),
                Clone::clone,
            )
        ),
        SteeringAnswer::Unanswered(disposition) => {
            format!("origin_steering_answer unavailable {disposition}")
        }
    };
    format!(
        "origin_steering key {key}\n\
         origin_steering_directive {} lane={} author={} published={}\n\
         origin_steering_text:\n{}\n\
         origin_steering_attachments {attachments}\n\
         {answer}",
        head.directive, head.recipient, provenance.author, created.at, head.text
    )
    .into_boxed_str()
}

pub(super) fn task_steering_attachment_name(
    key: &str,
    index: usize,
    attachment: &wire::Media,
) -> String {
    if attachment.name.is_empty() {
        format!(
            "steering-{key}-attachment-{}.{}",
            index + 1,
            match attachment.content_type.as_str() {
                artifact::MediaType::IMAGE_JPEG => "jpg",
                artifact::MediaType::IMAGE_GIF => "gif",
                artifact::MediaType::IMAGE_WEBP => "webp",
                _ => "png",
            }
        )
    } else {
        attachment.name.clone()
    }
}
