use super::*;
pub(super) fn route_actor_identity(identity: &str) -> Option<String> {
    if let Ok(identity) = identity.parse::<wire::CanonicalUuid>() {
        return Some(identity.to_string());
    }
    if let Ok(actor @ wire::Actor::Principal(_)) = identity.parse::<wire::Actor>() {
        return Some(actor.to_string());
    }
    (!identity.is_empty()
        && identity
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
    .then(|| identity.to_owned())
}

pub(super) fn task_origin_threads<'a>(
    facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    residency: &wire::ResidencyHead,
    selected: Option<&std::collections::BTreeSet<wire::TaskStamp>>,
) -> Result<std::collections::BTreeMap<wire::TaskStamp, String>, Outcome<CommandAnswer>> {
    let mut occupants = std::collections::BTreeMap::new();
    let mut sessions = std::collections::BTreeMap::new();
    let mut origins = std::collections::BTreeMap::new();
    for fact in facts {
        if let Some(declared) = fact.fact.read::<body::LaneSessionDeclared>() {
            sessions.insert((declared.lane, declared.occupant), declared.identity);
            continue;
        }
        if let Some(started) = fact.fact.read::<body::LaneStarted>() {
            occupants.insert(started.lane, started.occupant);
            continue;
        }
        let Some(added) = fact.fact.read::<body::TaskAdded>() else {
            continue;
        };
        if selected.is_some_and(|tasks| !tasks.contains(&added.task)) {
            continue;
        }
        let actor = match fact.actor {
            wire::Actor::Spice | wire::Actor::Principal(_) => fact.actor.to_string(),
            wire::Actor::Lane(lane) => {
                let occupant = occupants.get(&lane).copied().or_else(|| {
                    residency
                        .lanes
                        .iter()
                        .find(|row| row.lane == lane && row.session.is_none())
                        .and_then(|row| row.occupant)
                });
                let occupant = occupant.ok_or_else(|| {
                    protocol_fault(
                        "board::task-origin-occupant-missing",
                        "a lane task has no authoritative creating occupant",
                    )
                })?;
                match sessions.get(&(lane, occupant)) {
                    Some(identity) => route_actor_identity(identity).ok_or_else(|| {
                        protocol_fault(
                            "board::task-origin-session-invalid",
                            "the creating vendor session identity cannot name an actor task route",
                        )
                    })?,
                    None => occupant.to_string(),
                }
            }
        };
        if origins.insert(added.task.clone(), actor).is_some() {
            return Err(protocol_fault(
                "board::task-origin-duplicate",
                format!("task {} has more than one creation fact", added.task),
            ));
        }
    }
    Ok(origins)
}

pub(super) fn task_creator_teams<'a>(
    facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    selected: Option<&std::collections::BTreeSet<wire::TaskStamp>>,
) -> Result<
    std::collections::BTreeMap<wire::TaskStamp, Option<wire::CanonicalUuid>>,
    Outcome<CommandAnswer>,
> {
    let mut creators = std::collections::BTreeMap::new();
    for envelope in facts {
        let Some(added) = envelope.fact.read::<body::TaskAdded>() else {
            continue;
        };
        if selected.is_some_and(|tasks| !tasks.contains(&added.task)) {
            continue;
        }
        let creator = match envelope.actor {
            Actor::Lane(_) => added.creation.creator_team(),
            Actor::Spice | Actor::Principal(_) if added.creation.creator_team().is_none() => None,
            Actor::Spice | Actor::Principal(_) => {
                return Err(protocol_fault(
                    "board::task-creator-team-invalid",
                    "a non-lane task creation names a creator team",
                ));
            }
        };
        if creators.insert(added.task.clone(), creator).is_some() {
            return Err(protocol_fault(
                "board::task-creator-duplicate",
                format!("task {} has more than one creation fact", added.task),
            ));
        }
    }
    Ok(creators)
}

pub(super) fn selected_task_origin_threads(
    observed: &bus::TaskSnapshot,
    residency: &wire::ResidencyHead,
    selected: &std::collections::BTreeSet<wire::TaskStamp>,
) -> Result<std::collections::BTreeMap<wire::TaskStamp, String>, Outcome<CommandAnswer>> {
    let facts = observed.facts();
    let mut origins = std::collections::BTreeMap::new();
    for task in selected {
        let Some(heads) = observed.task_heads(task) else {
            continue;
        };
        if facts.has_duplicate_task_origin(task) {
            return Err(protocol_fault(
                "board::task-origin-duplicate",
                format!("task {task} has more than one creation fact"),
            ));
        }
        let identity = match facts.task_origin(task) {
            Some(bus::TaskOrigin::Identity(identity)) => identity.clone(),
            Some(bus::TaskOrigin::ResidencyIdentity(lane)) => {
                let occupant = residency
                    .lanes
                    .iter()
                    .find(|row| row.lane == *lane && row.session.is_none())
                    .and_then(|row| row.occupant)
                    .ok_or_else(|| {
                        protocol_fault(
                            "board::task-origin-occupant-missing",
                            "a lane task has no authoritative creating occupant",
                        )
                    })?;
                facts
                    .lane_session_identity(*lane, occupant)
                    .map_or_else(|| occupant.to_string(), str::to_owned)
            }
            None => {
                let added = facts
                    .at(heads.statement().body().topic_origin)
                    .and_then(|envelope| {
                        envelope
                            .fact
                            .read::<body::TaskAdded>()
                            .filter(|added| added.task == *task)
                            .map(|added| (envelope, added))
                    })
                    .ok_or_else(|| {
                        protocol_fault(
                            "board::task-origin-missing",
                            format!("task {task} has no authoritative creation fact"),
                        )
                    })?;
                match &added.0.actor {
                    actor @ (wire::Actor::Spice | wire::Actor::Principal(_)) => actor.to_string(),
                    wire::Actor::Lane(_) => match &added.1.creation {
                        body::TaskCreationContext::Transcript { session, .. } => session.clone(),
                        body::TaskCreationContext::Unavailable { .. } => {
                            return Err(protocol_fault(
                                "board::task-origin-occupant-missing",
                                "a retained lane task has no authoritative creating occupant",
                            ));
                        }
                    },
                }
            }
        };
        let identity = route_actor_identity(&identity).ok_or_else(|| {
            protocol_fault(
                "board::task-origin-session-invalid",
                "the creating vendor session identity cannot name an actor task route",
            )
        })?;
        origins.insert(task.clone(), identity);
    }
    Ok(origins)
}

pub(super) fn selected_task_creator_teams(
    observed: &bus::TaskSnapshot,
    selected: &std::collections::BTreeSet<wire::TaskStamp>,
) -> Result<
    std::collections::BTreeMap<wire::TaskStamp, Option<wire::CanonicalUuid>>,
    Outcome<CommandAnswer>,
> {
    let mut creators = std::collections::BTreeMap::new();
    for task in selected {
        let Some(heads) = observed.task_heads(task) else {
            continue;
        };
        let creator = heads.statement().body().creation.creator_team();
        let invalid = observed
            .facts()
            .at(heads.statement().body().topic_origin)
            .is_some_and(|envelope| !matches!(envelope.actor, Actor::Lane(_)) && creator.is_some());
        if invalid {
            return Err(protocol_fault(
                "board::task-creator-team-invalid",
                "a non-lane task creation names a creator team",
            ));
        }
        creators.insert(task.clone(), creator);
    }
    Ok(creators)
}

pub(super) fn selected_task_contexts(
    observed: &bus::TaskSnapshot,
    selected: &std::collections::BTreeSet<wire::TaskStamp>,
) -> (
    ClaimContexts,
    std::collections::BTreeMap<wire::TaskStamp, wire::intent::TaskGitContext>,
) {
    let facts = observed.facts();
    let mut claims = std::collections::BTreeMap::new();
    let mut git = std::collections::BTreeMap::new();
    for task in selected {
        let Some(heads) = observed.task_heads(task) else {
            continue;
        };
        for held in heads.claims() {
            let Some(envelope) = facts.at(held.claim().body().claimed) else {
                continue;
            };
            let Some(won) = envelope.fact.read::<body::ClaimWon>() else {
                continue;
            };
            let mut sessions = std::collections::BTreeMap::new();
            let body::SourceBoundary::Proven { heads } = &won.source;
            for boundary in heads {
                if let Some(identity) = facts.lane_session_identity(won.lane, won.occupant) {
                    sessions.insert(
                        (won.lane, won.occupant, boundary.vendor.clone(), won.epoch),
                        identity.to_owned(),
                    );
                }
            }
            claims.insert(
                envelope.seq,
                (
                    task.clone(),
                    wire::intent::TaskClaimContext {
                        claimed: envelope.seq,
                        lane: won.lane,
                        occupant: won.occupant,
                        epoch: won.epoch,
                        source: won.source.clone(),
                        recovery: claim_transcript_recovery(&won, &sessions),
                    },
                ),
            );
            if let Some(boundary) = won.git.clone() {
                git.insert(
                    task.clone(),
                    wire::intent::TaskGitContext {
                        fact: envelope.seq,
                        kind: wire::intent::TaskGitContextKind::Claim,
                        boundary,
                    },
                );
            }
        }
        let mut workflow = facts.at(heads.workflow().position());
        if workflow.is_some_and(|envelope| envelope.fact.read::<body::TaskPhased>().is_some()) {
            workflow = workflow
                .and_then(|envelope| envelope.cause)
                .and_then(|cause| facts.at(cause));
        }
        let context = workflow.and_then(|envelope| {
            if let Some(done) = envelope.fact.read::<body::TaskDone>() {
                done.git.map(|boundary| {
                    (
                        envelope.seq,
                        wire::intent::TaskGitContextKind::Completion,
                        boundary,
                    )
                })
            } else {
                envelope
                    .fact
                    .read::<body::TaskReviewed>()
                    .and_then(|reviewed| reviewed.git)
                    .map(|boundary| {
                        (
                            envelope.seq,
                            wire::intent::TaskGitContextKind::Review,
                            boundary,
                        )
                    })
            }
        });
        if let Some((fact, kind, boundary)) = context
            && git.get(task).is_none_or(|prior| prior.fact < fact)
        {
            git.insert(
                task.clone(),
                wire::intent::TaskGitContext {
                    fact,
                    kind,
                    boundary,
                },
            );
        }
    }
    (claims, git)
}

pub(super) fn task_contexts(
    facts: &bus::TaskFacts,
    selected: Option<&std::collections::BTreeSet<wire::TaskStamp>>,
) -> (
    ClaimContexts,
    std::collections::BTreeMap<wire::TaskStamp, wire::intent::TaskGitContext>,
) {
    let mut sessions = std::collections::BTreeMap::new();
    let mut claims = std::collections::BTreeMap::new();
    let mut git = std::collections::BTreeMap::new();
    for envelope in facts.iter() {
        if let Some(declaration) = envelope.fact.read::<body::LaneSessionDeclared>() {
            sessions.insert(
                (
                    declaration.lane,
                    declaration.occupant,
                    declaration.vendor,
                    declaration.epoch,
                ),
                declaration.identity,
            );
            continue;
        }
        if let Some(won) = envelope.fact.read::<body::ClaimWon>() {
            if selected.is_some_and(|tasks| !tasks.contains(&won.task)) {
                continue;
            }
            let task = won.task.clone();
            if let Some(boundary) = won.git.clone() {
                git.insert(
                    task.clone(),
                    wire::intent::TaskGitContext {
                        fact: envelope.seq,
                        kind: wire::intent::TaskGitContextKind::Claim,
                        boundary,
                    },
                );
            }
            let recovery = claim_transcript_recovery(&won, &sessions);
            claims.insert(
                envelope.seq,
                (
                    task,
                    wire::intent::TaskClaimContext {
                        claimed: envelope.seq,
                        lane: won.lane,
                        occupant: won.occupant,
                        epoch: won.epoch,
                        source: won.source,
                        recovery,
                    },
                ),
            );
            continue;
        }
        if let Some(done) = envelope.fact.read::<body::TaskDone>()
            && selected.is_none_or(|tasks| tasks.contains(&done.task))
            && let Some(boundary) = done.git
        {
            git.insert(
                done.task,
                wire::intent::TaskGitContext {
                    fact: envelope.seq,
                    kind: wire::intent::TaskGitContextKind::Completion,
                    boundary,
                },
            );
            continue;
        }
        if let Some(reviewed) = envelope.fact.read::<body::TaskReviewed>()
            && selected.is_none_or(|tasks| tasks.contains(&reviewed.task))
            && let Some(boundary) = reviewed.git
        {
            git.insert(
                reviewed.task,
                wire::intent::TaskGitContext {
                    fact: envelope.seq,
                    kind: wire::intent::TaskGitContextKind::Review,
                    boundary,
                },
            );
        }
    }
    (claims, git)
}

#[derive(Clone)]
pub(in crate::board_command) struct TaskCreationCapture {
    context: body::TaskCreationContext,
    residency: Option<Seq>,
    source: Option<(wire::CanonicalUuid, body::SourceBoundary)>,
    drive: std::sync::Arc<drive::Admission>,
}

impl TaskCreationCapture {
    pub(in crate::board_command) fn context(&self) -> body::TaskCreationContext {
        self.context.clone()
    }

    pub(in crate::board_command) const fn creator_team(&self) -> Option<wire::CanonicalUuid> {
        self.context.creator_team()
    }

    pub(in crate::board_command) fn guard(&self, write: &mut JournalWrite) {
        let Some(expected_residency) = self.residency else {
            return;
        };
        let (lane, expected_source) = self
            .source
            .clone()
            .expect("a lane creation capture carries a source boundary");
        write.guard(JournalGuard::new(move |read| {
            let residency = read.residency_head()?.ok_or_else(|| {
                journal::GuardError::Invalid(
                    "task creation context has no current residency head".to_owned(),
                )
            })?;
            if residency.position() != expected_residency {
                return Err(journal::GuardError::refused(
                    "task::creation-context-residency-moved",
                ));
            }
            if read.source_boundary(&lane)? != expected_source {
                return Err(journal::GuardError::refused(
                    "task::creation-context-source-moved",
                ));
            }
            Ok(())
        }));
        self.drive.guard(write);
    }
}

pub(in crate::board_command) fn task_creation_context_output(
    context: &body::TaskCreationContext,
) -> Box<str> {
    match context {
        body::TaskCreationContext::Unavailable { reason } => {
            format!("unavailable {}", reason.as_str()).into_boxed_str()
        }
        body::TaskCreationContext::Transcript {
            session, boundary, ..
        } => shell_words::join([
            "spice".to_owned(),
            "session".to_owned(),
            "context".to_owned(),
            session.clone(),
            "--lane".to_owned(),
            boundary.lane.to_string(),
            "--vendor".to_owned(),
            boundary.vendor.to_string(),
            "--stream".to_owned(),
            boundary.stream.clone(),
            "--source".to_owned(),
            boundary.source.to_string(),
            "--member".to_owned(),
            boundary.next.member.to_string(),
            "--offset".to_owned(),
            boundary.next.offset.to_string(),
        ])
        .into_boxed_str(),
    }
}

pub(in crate::board_command) fn task_creation_capture(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    snapshot: &ValidatedSnapshot,
) -> Result<TaskCreationCapture, Outcome<CommandAnswer>> {
    let drive = std::sync::Arc::new(drive::Admission::capture(
        command.actor(),
        &snapshot.answer.residency,
        std::sync::Arc::clone(&snapshot.policy),
    )?);
    let lane = match command.actor() {
        Actor::Principal(_) => {
            return Ok(TaskCreationCapture {
                context: body::TaskCreationContext::unavailable(
                    body::TaskCreationUnavailable::PrincipalActor,
                ),
                residency: None,
                source: None,
                drive,
            });
        }
        Actor::Spice => {
            return Ok(TaskCreationCapture {
                context: body::TaskCreationContext::unavailable(
                    body::TaskCreationUnavailable::SpiceActor,
                ),
                residency: None,
                source: None,
                drive,
            });
        }
        Actor::Lane(lane) => *lane,
    };
    let row = snapshot
        .answer
        .residency
        .body
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .ok_or_else(|| {
            protocol_fault(
                "task::creation-context-lane-missing",
                "the authenticated task actor has no current residency row",
            )
        })?;
    let session = row.session.as_ref().ok_or_else(|| {
        refusal(
            "task::creation-context-session-missing",
            Some(lane.to_string()),
            "re-seat the lane under current session authority before creating tasks",
        )
    })?;
    let current_occupant = row.occupant.ok_or_else(|| {
        protocol_fault(
            "task::creation-context-occupant-missing",
            "the authenticated task actor has no current occupant",
        )
    })?;
    if session.lane != lane
        || session.occupant != current_occupant
        || session.vendor != row.vendor
        || session.identity.is_empty()
    {
        return Err(protocol_fault(
            "task::creation-context-session-mismatch",
            "the authenticated lane, occupant, vendor, and causal session disagree",
        ));
    }
    let source = bus.source_boundary(&lane).map_err(|error| {
        store_fault(
            "task::creation-context-source-unreadable",
            error.to_string(),
        )
    })?;
    let body::SourceBoundary::Proven { heads } = &source;
    let matching = heads
        .iter()
        .filter(|head| head.lane == lane && head.vendor == session.vendor)
        .collect::<Vec<_>>();
    let registered = matching
        .iter()
        .filter(|head| head.stream == body::TASK_CREATION_CONTEXT_STREAM)
        .copied()
        .collect::<Vec<_>>();
    if registered.is_empty() && !matching.is_empty() {
        return Err(refusal(
            "task::creation-context-stream-unregistered",
            Some(format!(
                "lane={lane} vendor={} matching_heads={}",
                session.vendor,
                matching.len()
            )),
            "account the selected session stream before creating tasks",
        ));
    }
    let [boundary] = registered.as_slice() else {
        return Err(refusal(
            "task::creation-context-source-ambiguous",
            Some(format!(
                "lane={lane} vendor={} registered_heads={}",
                session.vendor,
                registered.len()
            )),
            "account exactly one selected session stream before creating tasks",
        ));
    };
    Ok(TaskCreationCapture {
        context: body::TaskCreationContext::Transcript {
            session: session.identity.clone(),
            boundary: (**boundary).clone(),
            creator_team: Some(row.team),
        },
        residency: Some(snapshot.answer.residency.position),
        source: Some((lane, source)),
        drive,
    })
}
