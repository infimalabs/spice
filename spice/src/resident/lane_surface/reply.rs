//! Lane answers, recall provenance, and typed reply construction.

use super::*;

pub(in crate::resident) fn command_result<B: serde::Serialize, A: serde::Serialize>(
    write: &mut JournalWrite,
    admitted: &wire::AdmittedCommand,
    body: B,
    epoch: CanonicalUuid,
    answer: A,
) -> CommandResult<()> {
    write
        .command_result(typed_command(admitted, body), epoch, answer)
        .map_err(|error| store_fault("lane::receipt-invalid", &error.to_string()))
}

const QUIET_HORIZON: Duration = Duration::from_millis(wire::intent::LANE_QUIET_HORIZON_MILLIS);
const TOOL_BLOCK_HORIZON: Duration = crate::cli::APP_ANSWER_BOUND;
enum ToolState {
    Clear,
    Pending(Duration),
    Blocked(String),
}

/// One lane answer over `head`; each row shows the code `codes` reports for
/// its lane.
pub(super) fn answer(
    head: &wire::ResidencyHead,
    selected: Option<CanonicalUuid>,
    seats: &Seats,
    paths: &RuntimePaths,
    codes: &bus::TaskFacts,
) -> CommandResult<LaneAnswer> {
    let now = Monotonic::now();
    let mut answer = answer_at(
        head,
        selected,
        now,
        |lane| seats.activity(lane),
        |lane| match seats.tool_call_status(lane, now, TOOL_BLOCK_HORIZON) {
            lane::occupant::ToolCallStatus::Clear => ToolState::Clear,
            lane::occupant::ToolCallStatus::Pending(remaining) => ToolState::Pending(remaining),
            lane::occupant::ToolCallStatus::Blocked(call) => {
                ToolState::Blocked(call.command().to_owned())
            }
        },
    )?;
    for row in &mut answer.lanes {
        row.speech = worktree_speech(paths, &row.worktree);
        row.jobs = terminal::unheard(&terminal::Jobs::for_lane(paths, row.lane));
        row.code = codes.lane_code(row.lane);
    }
    Ok(answer)
}

fn answer_at(
    head: &wire::ResidencyHead,
    selected: Option<CanonicalUuid>,
    now: Monotonic,
    activity: impl Fn(CanonicalUuid) -> Option<lane::occupant::Activity>,
    tool: impl Fn(CanonicalUuid) -> ToolState,
) -> CommandResult<LaneAnswer> {
    let mut refresh_after_millis: Option<u32> = None;
    let lanes = head
        .lanes
        .iter()
        .filter(|row| selected.is_none_or(|lane| row.lane == lane))
        .map(|row| {
            let observed = activity(row.lane);
            let mut liveness = liveness(row, observed, now)?;
            let blocked_on = match tool(row.lane) {
                ToolState::Clear => None,
                ToolState::Pending(remaining) => {
                    schedule_refresh(&mut refresh_after_millis, remaining);
                    None
                }
                ToolState::Blocked(command) => {
                    liveness = wire::intent::LaneLiveness::Blocked;
                    Some(command)
                }
            };
            if liveness == wire::intent::LaneLiveness::Working
                && let Some(observed) = observed
            {
                schedule_refresh(
                    &mut refresh_after_millis,
                    QUIET_HORIZON - now.duration_since(observed.at()),
                );
            }
            Ok(wire::intent::LaneAnswerRow {
                lane: row.lane,
                team: row.team,
                name: row.name.clone(),
                worktree: row.worktree.clone(),
                speech: wire::SpeechPlayback::Disabled {},
                life: row.life.clone(),
                vendor: row.vendor.clone(),
                model: match row.session.as_ref() {
                    Some(session) => Some(session.launch.model.clone()),
                    None if row.occupant.is_none() => row.model.clone(),
                    None => None,
                },
                seat_model: row.model.clone(),
                effort: match row.session.as_ref() {
                    Some(session) => Some(session.launch.effort.clone()),
                    None if row.occupant.is_none() => row.effort.clone(),
                    None => None,
                },
                seat_effort: row.effort.clone(),
                fast_mode: row
                    .session
                    .as_ref()
                    .is_some_and(|session| session.launch.fast_mode),
                fast_refusal: row
                    .session
                    .as_ref()
                    .and_then(|session| session.launch.fast_refusal.clone()),
                seat_fast_mode: row.fast_mode,
                git: row
                    .session
                    .as_ref()
                    .and_then(|session| session.launch.git.clone()),
                rtk: row
                    .session
                    .as_ref()
                    .and_then(|session| session.launch.rtk.clone()),
                occupant: row.occupant,
                code: None,
                lineage: row.lineage.clone(),
                session: row.session.as_ref().map(|session| session.identity.clone()),
                bound: row.session.is_some(),
                disposition: row.disposition.clone(),
                liveness,
                blocked_on,
                hold: None,
                parked: None,
                jobs: Vec::new(),
            })
        })
        .collect::<CommandResult<Vec<_>>>()?;
    let team_ids = lanes
        .iter()
        .map(|row| row.team)
        .collect::<std::collections::BTreeSet<_>>();
    let subscriptions = team_ids
        .into_iter()
        .map(|team| {
            subscription_row(head, team)?.cloned().ok_or_else(|| {
                store_fault(
                    "team::subscription-missing",
                    "a returned team has no runtime subscription",
                )
            })
        })
        .collect::<CommandResult<Vec<_>>>()?;
    Ok(LaneAnswer {
        epoch: head.epoch,
        state: head.state,
        topology_revision: head.topology_revision,
        lanes,
        subscriptions,
        renewals: head.renewals.clone(),
        refresh_after_millis,
    })
}
fn schedule_refresh(refresh_after_millis: &mut Option<u32>, remaining: Duration) {
    // Round past the boundary so a sub-millisecond remainder cannot return
    // another pre-transition snapshot.
    let millis = u32::try_from(remaining.as_millis() + 1)
        .expect("lane-state horizons fit in browser milliseconds");
    *refresh_after_millis =
        Some(refresh_after_millis.map_or(millis, |current| current.min(millis)));
}

fn worktree_speech(paths: &RuntimePaths, worktree: &str) -> wire::SpeechPlayback {
    paths
        .speech_for_worktree(worktree)
        .map(|speech| speech.playback())
        .unwrap_or(wire::SpeechPlayback::Disabled {})
}

/// Admit one client-observed topology authority and derive its successor.
///
/// The caller still guards the exact residency head before commit. Together,
/// this comparison and that guard make observing and advancing the logical
/// revision one optimistic transaction rather than a per-command race check.
pub(super) fn next_topology_revision(
    head: &wire::ResidencyHead,
    observed: u64,
) -> Result<u64, Outcome<CommandAnswer>> {
    wire::grouping::advance_revision(head.topology_revision, observed).map_err(
        |error| match error {
            wire::grouping::RevisionError::Moved { current } => Outcome::Refusal(Refusal::bare(
                wire::RefusalDiagnostic::new(
                    "lane::topology-moved",
                    Some("topology_revision".to_owned()),
                    Some(observed.to_string()),
                    Some(current.to_string()),
                )
                .with_detail(
                    "read the authoritative lane table and retry against its topology revision",
                ),
            )),
            wire::grouping::RevisionError::Exhausted => store_fault(
                "lane::topology-revision-exhausted",
                "the durable topology revision cannot advance",
            ),
        },
    )
}

fn liveness(
    row: &wire::LaneResidency,
    activity: Option<lane::occupant::Activity>,
    now: Monotonic,
) -> CommandResult<wire::intent::LaneLiveness> {
    let Some(occupant) = row.occupant else {
        if activity.is_some() {
            return Err(store_fault(
                "lane::liveness-occupied",
                "a durable vacant lane still has supervised activity",
            ));
        }
        return Ok(wire::intent::LaneLiveness::Vacant);
    };
    if crate::resident::is_simulated_lane(row) {
        if activity.is_some() {
            return Err(store_fault(
                "lane::simulated-supervised",
                "a simulated lane has real supervised activity",
            ));
        }
        if row.session.is_some() {
            return Err(store_fault(
                "lane::simulated-session-present",
                "a simulated lane carries real vendor session authority",
            ));
        }
        return Ok(wire::intent::LaneLiveness::Working);
    }
    let observed = activity.ok_or_else(|| {
        store_fault(
            "lane::liveness-unobserved",
            "a durable occupied lane has no supervised activity observation",
        )
    })?;
    if observed.occupant() != occupant {
        return Err(store_fault(
            "lane::liveness-occupant-moved",
            "the supervised activity occupant differs from durable residency",
        ));
    }
    let quiet = now.checked_duration_since(observed.at()).ok_or_else(|| {
        store_fault(
            "lane::liveness-clock-regressed",
            "the activity observation lies after the read clock",
        )
    })?;
    Ok(if quiet < QUIET_HORIZON {
        wire::intent::LaneLiveness::Working
    } else {
        wire::intent::LaneLiveness::Running
    })
}

pub(super) fn relevant(fact: &wire::Envelope<wire::AnyFact>, lane: CanonicalUuid) -> bool {
    fact.actor == Actor::Lane(lane) || fact_lane(fact) == Some(lane)
}

pub(super) fn fact_lane(fact: &wire::Envelope<wire::AnyFact>) -> Option<CanonicalUuid> {
    fact.fact
        .body()
        .get("lane")
        .and_then(serde_json::Value::as_str)
        .and_then(|lane| lane.parse().ok())
        .or(match fact.actor {
            Actor::Lane(lane) => Some(lane),
            Actor::Spice | Actor::Principal(_) => None,
        })
}

/// The stable root a terminal steering fact answers for: the current indexed
/// standing of the directive that key names.
pub(super) struct StableRoot {
    directive: CanonicalUuid,
    key: String,
    stage: wire::body::SteeringStage,
    escalations: Vec<wire::body::SteeringTransition>,
}

impl From<&wire::SteeringHead> for StableRoot {
    fn from(head: &wire::SteeringHead) -> Self {
        Self {
            directive: head.directive,
            key: head.key.clone(),
            stage: head.stage,
            escalations: head.escalations.clone(),
        }
    }
}

#[cfg(test)]
impl From<&steer::Directive> for StableRoot {
    fn from(directive: &steer::Directive) -> Self {
        Self {
            directive: directive.directive(),
            key: directive.key().to_string(),
            stage: directive.stage(),
            escalations: directive.escalations().to_vec(),
        }
    }
}

/// Answers one steering key with its stable root, when it has one.
pub(super) type StableRoots<'a> = dyn FnMut(&steer::Key) -> CommandResult<Option<StableRoot>> + 'a;

pub(super) fn recall_provenance(
    fact: &wire::Envelope<wire::AnyFact>,
    roots: &mut StableRoots<'_>,
) -> CommandResult<Option<RecallProvenance>> {
    if let Some(provenance) = recall_live_steering_provenance(fact)? {
        return Ok(Some(provenance));
    }
    if let Some(provenance) = recall_terminal_steering_provenance(fact, roots)? {
        return Ok(Some(provenance));
    }
    let provenance = match fact.fact.kind() {
        wire::FactKind::LaneSessionDeclared => {
            let body: wire::body::LaneSessionDeclared = recall_body(fact)?;
            Some(RecallProvenance::Session {
                occupant: body.occupant,
                worktree: body.worktree,
                vendor: body.vendor,
                epoch: body.epoch,
                stream: body.stream,
                identity: body.identity,
                locus: wire::TapeLocus {
                    member: body.member,
                    offset: body.offset,
                    length: body.length,
                },
            })
        }
        wire::FactKind::LaneCompacted => {
            let body: wire::body::LaneCompacted = recall_body(fact)?;
            Some(RecallProvenance::Compaction {
                occupant: body.occupant,
                stream: body.stream,
                locus: wire::TapeLocus {
                    member: body.member,
                    offset: body.offset,
                    length: body.length,
                },
            })
        }
        wire::FactKind::LaneRenewed => {
            let body: wire::body::LaneRenewed = recall_body(fact)?;
            Some(RecallProvenance::Renewal {
                occupant: body.occupant,
                ancestor: body.ancestor,
                epoch: body.epoch,
            })
        }
        _ => None,
    };
    Ok(provenance)
}

fn recall_live_steering_provenance(
    fact: &wire::Envelope<wire::AnyFact>,
) -> CommandResult<Option<RecallProvenance>> {
    let provenance = match fact.fact.kind() {
        wire::FactKind::SteerSent => {
            let body: wire::body::SteerSent = recall_body(fact)?;
            Some(steering_provenance(
                body.directive,
                body.key,
                wire::Disposition::Live,
                Some(wire::body::SteeringStage::Published),
                Vec::new(),
                None,
                None,
            ))
        }
        wire::FactKind::SteerReplied => {
            let body: wire::body::SteerReplied = recall_body(fact)?;
            Some(steering_provenance(
                body.directive,
                body.key,
                wire::Disposition::Live,
                Some(wire::body::SteeringStage::Published),
                Vec::new(),
                None,
                None,
            ))
        }
        // The packet locus is where the sender authored the message, not
        // where a lane answered, so it stays out of the answer locus.
        wire::FactKind::SteerMessaged => {
            let body: wire::body::SteerMessaged = recall_body(fact)?;
            Some(steering_provenance(
                body.directive,
                body.key,
                wire::Disposition::Live,
                Some(wire::body::SteeringStage::Published),
                Vec::new(),
                None,
                None,
            ))
        }
        wire::FactKind::SteerQueued => {
            let body: wire::body::SteerQueued = recall_body(fact)?;
            Some(steering_provenance(
                body.directive,
                body.key,
                wire::Disposition::Live,
                Some(wire::body::SteeringStage::Published),
                Vec::new(),
                None,
                None,
            ))
        }
        wire::FactKind::SteerEscalated => {
            let body: wire::body::SteerEscalated = recall_body(fact)?;
            Some(steering_provenance(
                body.directive,
                body.key,
                wire::Disposition::Live,
                Some(body.stage),
                Vec::new(),
                None,
                None,
            ))
        }
        // A reminder opens a team, so history reads it as the send it is.
        //
        // Its transcript locus is deliberately dropped here: this row's locus
        // means where a lane answered, and a reminder's locus means where the
        // prose that fired the taste sits. Carrying it across would read as an
        // acknowledgment nobody made.
        wire::FactKind::TasteReminded => {
            let body: wire::body::TasteReminded = recall_body(fact)?;
            Some(steering_provenance(
                body.directive,
                body.key,
                wire::Disposition::Live,
                Some(wire::body::SteeringStage::Published),
                Vec::new(),
                None,
                None,
            ))
        }
        _ => None,
    };
    Ok(provenance)
}

fn recall_terminal_steering_provenance(
    fact: &wire::Envelope<wire::AnyFact>,
    roots: &mut StableRoots<'_>,
) -> CommandResult<Option<RecallProvenance>> {
    let provenance = match fact.fact.kind() {
        wire::FactKind::SteerAcked => {
            let body: wire::body::SteerAcked = recall_body(fact)?;
            let evidence = body.evidence.transcript();
            Some(stable_terminal_provenance(
                roots,
                &body.key,
                wire::Disposition::Acked,
                evidence.map(|(epoch, _)| epoch),
                evidence.map(|(_, locus)| locus),
            )?)
        }
        wire::FactKind::SteerDeclined => {
            let body: wire::body::SteerDeclined = recall_body(fact)?;
            let evidence = body.evidence.transcript();
            Some(stable_terminal_provenance(
                roots,
                &body.key,
                wire::Disposition::Declined,
                evidence.map(|(epoch, _)| epoch),
                evidence.map(|(_, locus)| locus),
            )?)
        }
        wire::FactKind::SteerLapsed => {
            let body: wire::body::SteerLapsed = recall_body(fact)?;
            Some(stable_terminal_provenance(
                roots,
                &body.key,
                wire::Disposition::Lapsed,
                None,
                None,
            )?)
        }
        wire::FactKind::SteerRecovered => {
            let body: wire::body::SteerRecovered = recall_body(fact)?;
            Some(stable_terminal_provenance(
                roots,
                &body.key,
                wire::Disposition::Live,
                None,
                None,
            )?)
        }
        wire::FactKind::SteerCanceled => {
            let body: wire::body::SteerCanceled = recall_body(fact)?;
            Some(stable_terminal_provenance(
                roots,
                &body.key,
                wire::Disposition::Canceled,
                None,
                None,
            )?)
        }
        _ => None,
    };
    Ok(provenance)
}

/// Name one steering fact as the provenance a recall answer carries.
fn steering_provenance(
    directive: CanonicalUuid,
    key: String,
    disposition: wire::Disposition,
    stage: Option<wire::body::SteeringStage>,
    escalations: Vec<wire::body::SteeringTransition>,
    epoch: Option<CanonicalUuid>,
    locus: Option<wire::body::TranscriptLocus>,
) -> RecallProvenance {
    RecallProvenance::Steering {
        directive,
        key,
        disposition,
        stage,
        escalations,
        epoch,
        locus,
    }
}

fn stable_terminal_provenance(
    roots: &mut StableRoots<'_>,
    key: &str,
    disposition: wire::Disposition,
    epoch: Option<CanonicalUuid>,
    locus: Option<wire::body::TranscriptLocus>,
) -> CommandResult<RecallProvenance> {
    let key = key.parse::<steer::Key>().map_err(|error| {
        store_fault(
            "recall::steering-key-invalid",
            &format!("terminal steering key is invalid: {error}"),
        )
    })?;
    let root = roots(&key)?.ok_or_else(|| {
        store_fault(
            "recall::steering-root-missing",
            &format!("terminal steering key {key} has no stable directive"),
        )
    })?;
    Ok(steering_provenance(
        root.directive,
        root.key,
        disposition,
        Some(root.stage),
        root.escalations,
        epoch,
        locus,
    ))
}

/// Read one recalled fact body back through its registered DTO.
fn recall_body<T: wire::Body>(fact: &wire::Envelope<wire::AnyFact>) -> CommandResult<T> {
    fact.fact.read::<T>().ok_or_else(|| {
        store_fault(
            "recall::fact-invalid",
            &format!("a {} fact was read as {}", fact.fact.kind(), T::KIND),
        )
    })
}

#[cfg(test)]
#[path = "reply_tests.rs"]
mod terminal_tests;

pub(in crate::resident) fn committed_lane_reply(
    committed: bus::JournalCommit,
    answer: LaneChangeAnswer,
) -> Outcome<CommandAnswer> {
    Outcome::Reply(Reply {
        position: ReplyPosition::Committed(SeqRange {
            first: seq(committed.first()),
            last: seq(committed.last()),
        }),
        body: CommandAnswer::LaneChange(Box::new(answer)),
    })
}

pub(super) fn authority_refusal(actor: &Actor, expected: &str) -> Outcome<CommandAnswer> {
    lane_refusal(
        "command::authority",
        Some(expected.to_owned()),
        Some(actor.to_string()),
        "reconnect with a capability admitted for this command family",
    )
}

pub(super) fn missing_lane(lane: CanonicalUuid) -> Outcome<CommandAnswer> {
    lane_refusal(
        "lane::missing",
        Some("one current lane".to_owned()),
        Some(lane.to_string()),
        "read lane and retry with a current lane identity",
    )
}

pub(in crate::resident) fn lane_refusal<B>(
    reason: &str,
    expected: Option<String>,
    observed: Option<String>,
    detail: &str,
) -> Outcome<B> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("lane".to_owned()),
            expected,
            observed,
        )
        .with_detail(detail),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
            .parse()
            .expect("fixture uuid")
    }

    fn occupied() -> wire::LaneResidency {
        wire::LaneResidency {
            lane: uuid(1),
            team: uuid(1),
            name: Some("Ava".to_owned()),
            worktree: "/worktrees/main-a".to_owned(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: Some(uuid(2)),
            lineage: vec![uuid(2)],
            capabilities: vec![uuid(3)],
            session: Some(wire::LaneSession {
                lane: uuid(1),
                occupant: uuid(2),
                worktree: "/worktrees/main-a".to_owned(),
                vendor: "omp".parse().expect("fixture vendor"),
                launch: wire::body::LaneLaunchIdentity::new("sonnet", "high")
                    .with_fast_mode(true)
                    .with_git("current")
                    .with_rtk(wire::body::RtkHealth::new(
                        wire::body::RtkHealthState::Active,
                        Some("0.45.0".to_owned()),
                        "fixture active",
                    )),
                epoch: uuid(4),
                stream: "stdout".to_owned(),
                identity: "private-session".to_owned(),
                locus: wire::TapeLocus {
                    member: 1,
                    offset: 2,
                    length: 3,
                },
            }),
            disposition: "occupied".to_owned(),
        }
    }

    fn simulated() -> wire::LaneResidency {
        let mut row = occupied();
        row.vendor = crate::resident::SIMULATED_VENDOR
            .parse()
            .expect("simulated fixture vendor");
        row.session = None;
        row.disposition = crate::resident::SIMULATED_DISPOSITION.to_owned();
        row
    }

    fn subscriptions() -> Vec<wire::TeamSubscription> {
        vec![wire::TeamSubscription {
            team: uuid(1),
            formed_at: None,
            drive: true,
            drain: false,
            name: None,
            fast_mode: false,
        }]
    }

    fn renewal() -> wire::LaneRenewalRecord {
        wire::LaneRenewalRecord {
            team: uuid(1),
            lane: uuid(1),
            revision: 7,
            state: wire::LaneRenewalState::HandoffPending {
                predecessor: wire::RenewalIdentity {
                    occupant: uuid(2),
                    session: "private-session".to_owned(),
                    thread: "thread-predecessor".to_owned(),
                },
                team_slot: 0,
            },
        }
    }

    #[test]
    fn the_same_occupied_lane_turns_quiet_at_the_exact_sixty_second_boundary() {
        let row = occupied();
        let head = wire::ResidencyHead {
            epoch: uuid(4),
            state: ResidentState::Seated,
            topology_revision: 7,
            lanes: vec![row.clone()],
            subscriptions: subscriptions(),
            renewals: Vec::new(),
        };
        let unchanged = head.clone();
        let observed = Monotonic::now();
        let activity = lane::occupant::Activity::new(uuid(2), observed);

        let recent = answer_at(
            &head,
            None,
            observed + Duration::from_millis(59_999),
            |_| Some(activity),
            |_| ToolState::Clear,
        )
        .expect("recent activity projects");
        let quiet = answer_at(
            &head,
            None,
            observed + Duration::from_secs(60),
            |_| Some(activity),
            |_| ToolState::Clear,
        )
        .expect("boundary activity projects");

        assert_eq!(
            recent.lanes[0].liveness,
            wire::intent::LaneLiveness::Working
        );
        assert_eq!(quiet.lanes[0].liveness, wire::intent::LaneLiveness::Running);
        assert_eq!(recent.lanes[0].name.as_deref(), Some("Ava"));
        assert_eq!(recent.lanes[0].life, "drive");
        assert_eq!(recent.lanes[0].disposition, "occupied");
        assert_eq!(
            head, unchanged,
            "clock fold mutated durable lifecycle state"
        );
    }

    #[test]
    fn a_long_tool_call_is_blocked_and_names_the_command() {
        let head = wire::ResidencyHead {
            epoch: uuid(4),
            state: ResidentState::Seated,
            topology_revision: 7,
            lanes: vec![occupied()],
            subscriptions: subscriptions(),
            renewals: Vec::new(),
        };
        let now = Monotonic::now();
        let pending = answer_at(
            &head,
            None,
            now,
            |_| Some(lane::occupant::Activity::new(uuid(2), now)),
            |_| ToolState::Pending(TOOL_BLOCK_HORIZON),
        )
        .expect("pre-bound supervision projects");
        assert_eq!(
            pending.lanes[0].liveness,
            wire::intent::LaneLiveness::Working
        );
        assert_eq!(
            pending.refresh_after_millis,
            Some(
                u32::try_from(TOOL_BLOCK_HORIZON.as_millis() + 1)
                    .expect("tool horizon fits browser milliseconds")
            )
        );

        let answer = answer_at(
            &head,
            None,
            now + TOOL_BLOCK_HORIZON,
            |_| Some(lane::occupant::Activity::new(uuid(2), now)),
            |_| ToolState::Blocked("spice room contribute DIRECTI-example".to_owned()),
        )
        .expect("blocked supervision projects");

        assert_eq!(
            answer.lanes[0].liveness,
            wire::intent::LaneLiveness::Blocked
        );
        assert_eq!(
            answer.lanes[0].blocked_on.as_deref(),
            Some("spice room contribute DIRECTI-example")
        );
    }

    #[test]
    fn lane_reads_expose_display_identity_without_capabilities() {
        let row = occupied();
        let capability = row.capabilities[0].to_string();
        let renewal = renewal();
        let head = wire::ResidencyHead {
            epoch: uuid(4),
            state: ResidentState::Seated,
            topology_revision: 7,
            lanes: vec![row],
            subscriptions: subscriptions(),
            renewals: vec![renewal.clone()],
        };
        let observed = Monotonic::now();
        let answer = answer_at(
            &head,
            None,
            observed,
            |_| Some(lane::occupant::Activity::new(uuid(2), observed)),
            |_| ToolState::Clear,
        )
        .expect("supervised lane projects");
        let encoded = String::from_utf8(wire::encode(&answer).expect("answer encodes"))
            .expect("wire encoding is UTF-8");

        assert!(answer.lanes[0].bound);
        assert_eq!(answer.topology_revision, 7);
        assert_eq!(answer.lanes[0].model.as_deref(), Some("sonnet"));
        assert_eq!(answer.lanes[0].effort.as_deref(), Some("high"));
        assert!(answer.lanes[0].fast_mode);
        assert_eq!(answer.lanes[0].git.as_deref(), Some("current"));
        let rtk = answer.lanes[0]
            .rtk
            .as_ref()
            .expect("recorded RTK health projects");
        assert_eq!(rtk.state(), wire::body::RtkHealthState::Active);
        assert_eq!(rtk.version(), Some("0.45.0"));
        assert_eq!(rtk.detail(), "fixture active");
        assert_eq!(answer.lanes[0].session.as_deref(), Some("private-session"));
        assert_eq!(answer.renewals, vec![renewal]);
        assert!(!encoded.contains(&capability));
        assert!(!encoded.contains("capabilities"));
        assert!(encoded.contains("private-session"));
    }

    #[test]
    fn a_durable_occupant_without_the_same_supervised_observation_faults() {
        let head = wire::ResidencyHead {
            epoch: uuid(4),
            state: ResidentState::Seated,
            topology_revision: 7,
            lanes: vec![occupied()],
            subscriptions: subscriptions(),
            renewals: Vec::new(),
        };
        let error = answer_at(
            &head,
            None,
            Monotonic::now(),
            |_| None,
            |_| ToolState::Clear,
        )
        .expect_err("unobserved occupancy must fail loudly");
        let Outcome::Fault(fault) = error else {
            panic!("unobserved occupancy must be a fault")
        };
        assert_eq!(fault.reason, "lane::liveness-unobserved");
    }

    #[test]
    fn a_simulated_occupant_is_running_without_a_supervised_process() {
        let head = wire::ResidencyHead {
            epoch: uuid(4),
            state: ResidentState::Seated,
            topology_revision: 7,
            lanes: vec![simulated()],
            subscriptions: subscriptions(),
            renewals: Vec::new(),
        };

        let answer = answer_at(
            &head,
            None,
            Monotonic::now(),
            |_| None,
            |_| ToolState::Clear,
        )
        .expect("simulated occupancy projects without a process");

        assert_eq!(
            answer.lanes[0].liveness,
            wire::intent::LaneLiveness::Working
        );
        assert!(!answer.lanes[0].bound);
        assert_eq!(
            answer.lanes[0].vendor.as_str(),
            crate::resident::SIMULATED_VENDOR
        );
        assert_eq!(
            answer.lanes[0].disposition,
            crate::resident::SIMULATED_DISPOSITION
        );
    }

    #[test]
    fn a_simulated_occupant_with_real_supervision_faults() {
        let row = simulated();
        let occupant = row.occupant.expect("simulated fixture occupant");
        let head = wire::ResidencyHead {
            epoch: uuid(4),
            state: ResidentState::Seated,
            topology_revision: 7,
            lanes: vec![row],
            subscriptions: subscriptions(),
            renewals: Vec::new(),
        };
        let error = answer_at(
            &head,
            None,
            Monotonic::now(),
            |_| Some(lane::occupant::Activity::new(occupant, Monotonic::now())),
            |_| ToolState::Clear,
        )
        .expect_err("simulated occupancy must never acquire real supervision");
        let Outcome::Fault(fault) = error else {
            panic!("simulated supervision must be a fault")
        };
        assert_eq!(fault.reason, "lane::simulated-supervised");
    }
}
