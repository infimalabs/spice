use super::*;

mod responses;

use responses::{cover_packed_frames, cover_refusal_frames, cover_reply_frames};
// Executable samples for wire-crossing identity.

fn at(text: &str) -> Instant {
    text.parse().expect("instant")
}

/// A committed position covering exactly one sequence.
fn committed(position: u64) -> ReplyPosition {
    ReplyPosition::Committed(SeqRange {
        first: seq(position),
        last: seq(position),
    })
}

/// The refusal the reply frames reuse, carrying every optional field.
fn stale_witness() -> Refusal {
    Refusal::bare(wire::RefusalDiagnostic::new(
        "witness::stale".to_owned(),
        Some("task".to_owned()),
        Some("48190".to_owned()),
        Some("48213".to_owned()),
    ))
}

/// The fault the reply frames reuse.
fn moved_epoch() -> Fault {
    Fault {
        class: FaultClass::Protocol,
        reason: "epoch::moved".to_owned(),
        detail: "resident epoch moved".to_owned(),
    }
}

#[test]
fn sampled_crossing_values_preserve_identity() {
    cover_spellings();
    cover_config_scalars();
    cover_doctor_vocabularies();
    cover_environment_requirements();
    cover_journal_frames();
    cover_task_heads();
    cover_claim_heads();
    cover_policy_heads();
    cover_steering_heads();
    cover_residency_heads();
    cover_head_enums();
    cover_command_frames();
    cover_message_annotations();
    cover_history_cursors();
    cover_stream_frames();
    cover_packed_frames();
    cover_refusal_frames();
    cover_reply_frames();
    cover_witness_values();
    cover_vocabularies();
}

/// Canonical task constraints whose fact variants own their value semantics.
fn cover_environment_requirements() {
    law(
        "EnvironmentRequirements",
        &[
            EnvironmentRequirements::default(),
            EnvironmentRequirements::one(
                EnvironmentRequirement::journal_schema(15).expect("positive schema"),
            ),
        ],
    );
}

fn cover_message_annotations() {
    law(
        "DirectiveBadge",
        &[DirectiveBadge::Review, DirectiveBadge::Taste],
    );
    law(
        "MessageAcknowledgement",
        &[
            MessageAcknowledgement::Pending,
            MessageAcknowledgement::Acknowledged,
            MessageAcknowledgement::Declined,
        ],
    );
    law(
        "AutomaticSpeech",
        &[
            AutomaticSpeech {
                kind: AutomaticSpeechKind::Final,
                text: "settled final".to_owned(),
            },
            AutomaticSpeech {
                kind: AutomaticSpeechKind::Acknowledgement,
                text: "applied".to_owned(),
            },
        ],
    );
}

fn cover_history_cursors() {
    law(
        "MessageHistoryCursor",
        &[
            MessageHistoryCursor {
                generation: 1,
                lane: uuid("018f82d4-6f59-7d06-92e3-bce9131163d1"),
                before: None,
            },
            MessageHistoryCursor {
                generation: 2,
                lane: uuid("018f82d4-6f59-7d06-92e3-bce9131163d2"),
                before: Some(MessageLocus {
                    member: 1,
                    offset: 2,
                    length: 3,
                    event: 4,
                }),
            },
        ],
    );
}

/// Closed Doctor report states and metadata spellings.
fn cover_doctor_vocabularies() {
    spelling_law("SpiceVariable", SpiceVariable::ALL);
    ranked_law("DoctorSubsystem", DoctorSubsystem::ALL);
    spelling_law("DoctorToolMode", DoctorToolMode::ALL);
    ranked_law("DoctorToolRequirement", DoctorToolRequirement::ALL);
    ranked_law("DoctorToolSource", DoctorToolSource::ALL);
    ranked_law("DoctorToolBounds", DoctorToolBounds::ALL);
    spelling_law("DoctorToolCancellation", DoctorToolCancellation::ALL);
    ranked_law("DoctorToolNetwork", DoctorToolNetwork::ALL);
    law(
        "DoctorObservation",
        &[
            DoctorObservation::Ready {},
            DoctorObservation::Unavailable {
                reason: "rtk is absent".to_owned(),
            },
            DoctorObservation::InputRequired {
                reason: "the caller supplies CMD".to_owned(),
            },
            DoctorObservation::NotApplicable {
                reason: "the command declares no tool".to_owned(),
            },
            DoctorObservation::Unsupported {
                reason: "inspection would execute the tool".to_owned(),
            },
        ],
    );
}

/// Shared configuration words and the typed provenance surrounding them.
fn cover_config_scalars() {
    spelling_law(
        "DurationWord",
        &[
            "0s".parse::<DurationWord>().expect("duration word"),
            "00s".parse::<DurationWord>().expect("duration word"),
            "15m".parse::<DurationWord>().expect("duration word"),
            format!("{}s", u64::MAX)
                .parse::<DurationWord>()
                .expect("maximum duration word"),
        ],
    );

    law(
        "Provenance",
        &[
            Provenance::<DurationWord>::Builtin {},
            Provenance::Tracked { builtin: None },
            Provenance::Tracked {
                builtin: Some("5m".parse().expect("duration word")),
            },
            Provenance::Local { builtin: None },
            Provenance::Local {
                builtin: Some("10m".parse().expect("duration word")),
            },
            Provenance::Flag { builtin: None },
            Provenance::Flag {
                builtin: Some("15m".parse().expect("duration word")),
            },
        ],
    );
}

/// The spelling newtypes.
///
/// Instant's samples include sub-millisecond, pre-epoch, and boundary readings,
/// which is the narrowing path that broke this law before.
fn cover_spellings() {
    law(
        "AgentMediaReference",
        &[
            AgentMediaReference::from_media(
                uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"),
                0,
                &wire::Media::from_base64("capture.png", "image/png", "iVBORw0KGgo=")
                    .expect("media"),
            )
            .expect("agent media reference"),
            AgentMediaReference::from_media(
                uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                1,
                &wire::Media::from_base64("detail.png", "image/png", "iVBORw0KGgo=")
                    .expect("media"),
            )
            .expect("agent media reference"),
        ],
    );
    spelling_law(
        "Instant",
        &[
            Instant::new("0000-01-01T00:00:00Z".parse().expect("minimum wire year"))
                .expect("minimum wire instant"),
            Instant::new("2030-04-12T18:44:09.271828Z".parse().expect("timestamp"))
                .expect("reading is in the wire year range"),
            Instant::new("2030-04-12T18:44:09.271Z".parse().expect("timestamp"))
                .expect("reading is in the wire year range"),
            Instant::new("1969-12-31T23:59:59.999999Z".parse().expect("timestamp"))
                .expect("reading is in the wire year range"),
            Instant::new(jiff::Timestamp::MAX).expect("maximum wire instant"),
        ],
    );
    spelling_law(
        "CanonicalUuid",
        &[
            uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"),
            uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
        ],
    );
    spelling_law(
        "TaskHandle",
        &[handle("SPICE-1kKRRsDPx"), handle("WIRE-1kJJP2Qcx")],
    );
    spelling_law(
        "TaskStamp",
        &[stamp("00000000x"), stamp("00000001x"), stamp("0000000Bx")],
    );
    spelling_law("Phase", &[phase("todo"), phase("review")]);
    spelling_law(
        "Origin",
        &[
            "task:SPICE-1kKRRsDPx".parse::<Origin>().expect("origin"),
            "repo:genesis".parse::<Origin>().expect("origin"),
        ],
    );
    spelling_law(
        "FoldName",
        &[
            "board::ready".parse::<FoldName>().expect("fold name"),
            "bus::refusals".parse::<FoldName>().expect("fold name"),
        ],
    );
    spelling_law(
        "Actor",
        &[
            Actor::Lane(uuid("018f82d4-6f59-7d06-92e3-bce9131163c2")),
            Actor::TEST_PRINCIPAL,
        ],
    );
    // A position spells decimal digits but ranks as a number, and the journal
    // reads it as a number: `9` before `10`, which is the reverse of how the
    // two spell. Samples span that step so the fixture pins the numeric order
    // rather than a lexical one that happens to agree on the samples chosen.
    ranked_law("Seq", &[seq(1), seq(9), seq(10), seq(48213)]);
    spelling_law(
        "Name",
        &[
            "next".parse::<Name>().expect("name"),
            "reads".parse::<Name>().expect("name"),
        ],
    );
    spelling_law("Digest", &[digest("a"), digest("b")]);
}

/// The frames that rest in the journal.
///
/// These derive only Eq and inherit the law structurally, so their samples vary
/// one field at a time against a shared base.
fn cover_journal_frames() {
    law(
        "Envelope",
        &[
            Envelope {
                v: WireVersion(1),
                seq: seq(48213),
                at: at("2030-04-12T18:44:09.271Z"),
                actor: Actor::TEST_PRINCIPAL,
                origin: "task:SPICE-1kKRRsDPx".parse().expect("origin"),
                cause: Some(seq(48190)),
                fact: Fact::new(body("checked")),
            },
            Envelope {
                v: WireVersion(1),
                seq: seq(48214),
                at: at("2030-04-12T18:44:09.272Z"),
                actor: Actor::TEST_PRINCIPAL,
                origin: "task:SPICE-1kKRRsDPx".parse().expect("origin"),
                cause: None,
                fact: Fact::new(body("cargo test --workspace")),
            },
        ],
    );
    law(
        "Fact",
        &[
            Fact::new(body("checked")),
            Fact::new(body("cargo test --workspace")),
        ],
    );

    // The opaque read of the same fact. It obeys the identity law for the same
    // reason the typed one does — a verifier that re-encodes a row it read must
    // get the row back — and the samples are the typed bodies above so a drift
    // between the two readings would show up here as a byte difference.
    law(
        "AnyFact",
        &[
            any_fact(Fact::new(body("checked"))),
            any_fact(Fact::new(body("cargo test --workspace"))),
        ],
    );
}

/// Read a typed fact back the way a journal verifier reads a stored row.
fn any_fact<F: wire::Body>(fact: Fact<F>) -> AnyFact {
    let bytes = wire::encode(&fact).expect("a typed fact encodes");
    wire::decode(&bytes).expect("a stored fact reads back without its body type")
}

/// Task statement and workflow heads.
fn cover_task_heads() {
    let task = stamp("1kKRRsDPx");

    // Principal-authored crossing fixtures.
    law(
        "TaskHead",
        &[
            TaskHead {
                preparation: None,
                intent: None,
                born: task.clone(),
                topic_origin: seq(9),
                title: "Build the gate".to_owned(),
                pitch: "The gate must exist before anything crosses.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: EnvironmentRequirements::default(),
                resources: wire::ResourceRequirements::default(),
                topic: "/gate".parse().expect("fixture topic"),
                priority: None,
                taste: None,
                dependencies: Vec::new(),
                provenance: wire::TaskProvenance::OperatorDraft,
                creation: body::TaskCreationContext::unavailable(
                    body::TaskCreationUnavailable::PrincipalActor,
                ),
                revision: seq(10),
            },
            TaskHead {
                preparation: None,
                intent: None,
                born: task.clone(),
                topic_origin: seq(9),
                title: "Review the gate".to_owned(),
                pitch: "A built gate deserves a second look.".to_owned(),
                description: Some("Confirm every edge".to_owned()),
                term: vec!["The gate passes".to_owned()],
                requirements: EnvironmentRequirements::one(
                    EnvironmentRequirement::journal_schema(15).expect("positive schema"),
                ),
                resources: wire::ResourceRequirements::new([(
                    "windows-guest".parse().expect("resource name"),
                    2,
                )])
                .expect("resource requirements"),
                topic: "/gate".parse().expect("fixture topic"),
                priority: None,
                taste: None,
                dependencies: vec![stamp("1kKRRsDQx")],
                provenance: "task:1kKRRsDPx".parse().expect("origin"),
                creation: body::TaskCreationContext::unavailable(
                    body::TaskCreationUnavailable::PrincipalActor,
                ),
                revision: seq(11),
            },
        ],
    );
    law(
        "WorkflowHead",
        &[
            WorkflowHead {
                flow: vec![wire::Phase::Draft, wire::Phase::Todo, wire::Phase::Review],
                phase: wire::Phase::Draft,
                status: wire::WorkflowStatus::Pending(wire::WorkflowReview::Clean),
                deferral: Some(at("2030-04-12T18:44:09.271Z")),
                submitted: true,
                generation: 0,
            },
            WorkflowHead {
                flow: vec![wire::Phase::Draft, wire::Phase::Todo, wire::Phase::Review],
                phase: wire::Phase::Review,
                status: wire::WorkflowStatus::reviewed(at("2030-04-12T18:44:09.272Z")),
                deferral: None,
                submitted: false,
                generation: 0,
            },
        ],
    );
}

/// Claim possession and lease heads.
fn cover_claim_heads() {
    let first_uuid = uuid("018f82d4-6f59-7d06-92e3-bce9131163c2");
    let second_uuid = uuid("018f82d4-6f59-7d06-92e3-bce9131163c3");

    law(
        "ClaimHead",
        &[
            ClaimHead {
                actor: Actor::TEST_PRINCIPAL,
                fact: seq(12),
                claimed: seq(12),
                cell: wire::ClaimCell {
                    topic: Some("/gate".parse().expect("fixture topic")),
                    phase: "todo".parse().expect("phase"),
                },
                lane: first_uuid,
                occupant: second_uuid,
                epoch: first_uuid,
                recovery: None,
                disposition: ClaimDisposition::Active,
                acknowledged: 0,
            },
            ClaimHead {
                actor: Actor::Lane(first_uuid),
                fact: seq(13),
                claimed: seq(12),
                cell: wire::ClaimCell {
                    topic: Some("/gate".parse().expect("fixture topic")),
                    phase: "review".parse().expect("phase"),
                },
                lane: second_uuid,
                occupant: first_uuid,
                epoch: second_uuid,
                recovery: None,
                disposition: ClaimDisposition::Lapsed,
                acknowledged: 0,
            },
        ],
    );
    law(
        "LeaseHead",
        &[
            LeaseHead {
                claimed: wire::Seq::new(4).expect("claim identity"),
                start: at("2030-04-12T18:44:09.271Z"),
                renewal: at("2030-04-12T18:44:09.272Z"),
                maximum: at("2030-04-12T18:45:09.271Z"),
                expiry: at("2030-04-12T18:45:00.271Z"),
                epoch: first_uuid,
            },
            LeaseHead {
                claimed: wire::Seq::new(9).expect("claim identity"),
                start: at("2030-04-12T18:44:10.271Z"),
                renewal: at("2030-04-12T18:44:10.272Z"),
                maximum: at("2030-04-12T18:46:09.271Z"),
                expiry: at("2030-04-12T18:46:00.271Z"),
                epoch: second_uuid,
            },
        ],
    );
}

/// Trust heads.
fn cover_policy_heads() {
    law(
        "TrustHead",
        &[
            TrustHead {
                digest: digest("a"),
                state: TrustState::Pended,
                worktree: None,
            },
            TrustHead {
                digest: digest("b"),
                state: TrustState::Granted,
                worktree: Some("/worktrees/trust-b".to_owned()),
            },
        ],
    );
}

/// Steering heads with current required mint moments and optional metadata.
fn cover_steering_heads() {
    let directive = uuid("018f82d4-6f59-7d06-92e3-bce9131163c2");
    let lane = uuid("018f82d4-6f59-7d06-92e3-bce9131163c3");
    law(
        "SteeringHead",
        &[
            SteeringHead {
                sender: wire::Actor::TEST_PRINCIPAL,
                room: None,
                directive,
                fanout: None,
                recipients: Vec::new(),
                key: "000000000".to_owned(),
                recipient: wire::Actor::Lane(lane),
                text: "hold the indexed line".to_owned(),
                control: None,
                attachments: Vec::new(),
                published: at("2030-04-12T18:44:09.271Z"),
                expires: at("2030-04-12T19:44:09.271Z"),
                stage: body::SteeringStage::Published,
                escalations: Vec::new(),
                disposition: Disposition::Live,
                origin: "repo:genesis".parse().expect("origin"),
                created: seq(16),
                minted_micros: 0,
            },
            SteeringHead {
                sender: wire::Actor::TEST_PRINCIPAL,
                room: None,
                directive,
                fanout: Some(uuid("018f82d4-6f59-7d06-92e3-bce9131163c4")),
                recipients: Vec::new(),
                key: "000000000".to_owned(),
                recipient: wire::Actor::Lane(lane),
                text: "hold the indexed line".to_owned(),
                control: Some("drive-drain-queue".to_owned()),
                attachments: vec![
                    wire::Media::from_base64("capture.png", "image/png", "iVBORw0KGgo=")
                        .expect("fixture media"),
                ],
                published: at("2030-04-12T18:44:09.271Z"),
                expires: at("2030-04-12T20:44:09.271Z"),
                stage: body::SteeringStage::Reminded,
                escalations: vec![body::SteeringTransition {
                    stage: body::SteeringStage::Reminded,
                    at: at("2030-04-12T18:54:09.271Z"),
                }],
                disposition: Disposition::Declined,
                origin: "task:1kKRRsDPx".parse().expect("origin"),
                created: seq(17),
                minted_micros: 0,
            },
        ],
    );
}

/// Resident generation, lane, and cursor heads.
fn cover_residency_heads() {
    let first_uuid = uuid("018f82d4-6f59-7d06-92e3-bce9131163c2");
    let second_uuid = uuid("018f82d4-6f59-7d06-92e3-bce9131163c3");

    let occupied = LaneResidency {
        lane: first_uuid,
        team: first_uuid,
        name: Some("Ava".to_owned()),
        worktree: "/worktrees/lane-a".to_owned(),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: true,
        occupant: Some(second_uuid),
        lineage: vec![first_uuid],
        capabilities: vec![second_uuid],
        session: Some(LaneSession {
            lane: first_uuid,
            occupant: second_uuid,
            worktree: "/worktrees/lane-a".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
            epoch: first_uuid,
            stream: "stdout".to_owned(),
            identity: "018f7777-7777-7444-8888-999999999999".to_owned(),
            locus: TapeLocus {
                member: 7,
                offset: 31,
                length: 93,
            },
        }),
        disposition: "seated".to_owned(),
    };
    let empty = LaneResidency {
        lane: second_uuid,
        team: second_uuid,
        name: None,
        worktree: "/worktrees/lane-b".to_owned(),
        life: "steer".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage: Vec::new(),
        capabilities: Vec::new(),
        session: None,
        disposition: "empty".to_owned(),
    };
    let recorded_session = occupied.session.clone().expect("occupied session");
    let mut distinct_session = recorded_session.clone();
    distinct_session.launch = wire::body::LaneLaunchIdentity::new("opus", "max");
    distinct_session.launch.fast_refusal =
        Some("'priority' is not supported for 'service_tier' on this model".to_owned());
    law("LaneSession", &[recorded_session, distinct_session]);
    law("LaneResidency", &[occupied.clone(), empty.clone()]);
    let drive = TeamSubscription {
        team: first_uuid,
        formed_at: Some(
            "2030-04-12T18:44:09.271Z"
                .parse()
                .expect("fixture formation instant"),
        ),
        drive: true,
        drain: false,
        name: Some("builders".to_owned()),
        fast_mode: true,
    };
    let steer = TeamSubscription {
        team: second_uuid,
        formed_at: None,
        drive: false,
        drain: false,
        name: None,
        fast_mode: false,
    };
    law("TeamSubscription", &[drive.clone(), steer.clone()]);
    law(
        "ResidencyHead",
        &[
            ResidencyHead {
                epoch: first_uuid,
                state: ResidentState::Seated,
                topology_revision: 2,
                lanes: vec![occupied],
                subscriptions: vec![drive],
                renewals: Vec::new(),
            },
            ResidencyHead {
                epoch: second_uuid,
                state: ResidentState::Dark,
                topology_revision: 7,
                lanes: vec![empty],
                subscriptions: vec![steer],
                renewals: Vec::new(),
            },
        ],
    );
    law(
        "CursorHead",
        &[
            CursorHead {
                seq: None,
                wake: None,
            },
            CursorHead {
                seq: Some(seq(16)),
                wake: Some(seq(15)),
            },
        ],
    );
}

/// Closed enums carried inside authoritative heads.
fn cover_head_enums() {
    law(
        "ClaimDisposition",
        &[ClaimDisposition::Active, ClaimDisposition::Freed],
    );
}

/// The frames a caller sends across the socket.
///
/// A command is checked under the encoder its identity binds rather than the
/// generic one, so the pairwise arm reads canonical bytes. Its samples carry
/// several witnesses in more than one order, because a single-witness sample
/// cannot tell a sorted encoding from an unsorted one.
fn cover_command_frames() {
    assert_identity_law(
        "Command",
        &[
            command("task::done", vec![task_witness()]),
            command("task::done", vec![task_witness(), workflow_witness()]),
            command("task::done", vec![workflow_witness(), task_witness()]),
            command("task::done", vec![task_witness(), task_witness()]),
            Command {
                v: WireVersion(1),
                id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c4"),
                kind: "task::note".parse::<CommandKind>().expect("command kind"),
                expected: Vec::new(),
                body: body("checked"),
            },
        ],
        canonical_command_identity,
    );
    law(
        "Hello",
        &[
            Hello {
                wire: WireVersion(1),
                capability: uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"),
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
            },
            Hello {
                wire: WireVersion(1),
                capability: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
            },
        ],
    );
    law(
        "Accepted",
        &[
            Accepted {
                wire: WireVersion(1),
                actor: Actor::TEST_PRINCIPAL,
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"),
                release: "1.2.3".parse().expect("fixture release"),
            },
            Accepted {
                wire: WireVersion(1),
                actor: Actor::Spice,
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"),
                release: "1.2.3".parse().expect("fixture release"),
            },
            Accepted {
                wire: WireVersion(1),
                actor: Actor::Lane(uuid("018f82d4-6f59-7d06-92e3-bce9131163c4")),
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"),
                release: "1.2.3".parse().expect("fixture release"),
            },
        ],
    );
}

/// Every client request and resident frame in the closed app stream family.
///
/// Both enums carry serde narrowing: the request omits an absent reconnect
/// cursor and a fault omits an unavailable correlation id. Sampling every arm,
/// both optional states, and both packed row types proves those renderings are
/// the complete value rather than an identity-losing fold.
fn cover_stream_frames() {
    let epoch = uuid("018f82d4-6f59-7d06-92e3-bce9131163c3");
    let next_epoch = uuid("018f82d4-6f59-7d06-92e3-bce9131163c4");
    let first_id = uuid("018f82d4-6f59-7d06-92e3-bce9131163d1");
    let second_id = uuid("018f82d4-6f59-7d06-92e3-bce9131163d2");
    let cursor = StreamCursor {
        epoch,
        seq: seq(48213),
    };
    law("AgentMessage", &agent_message_samples(first_id, second_id));
    let requests = stream_request_samples(epoch, first_id, second_id, cursor);
    law("StreamRequest", &requests);

    let first_fact = opaque_stream_envelope(48213, None, "checked");
    let second_fact = opaque_stream_envelope(48214, Some(48213), "verified");
    let lane_fact = stream_lane_seated(48215, Some(48214));
    law(
        "StreamFact",
        &[first_fact.clone(), second_fact.clone(), lane_fact],
    );
    let facts = Packed {
        rows: vec![first_fact.clone(), second_fact.clone()],
        overflow: Some(Overflow {
            omitted: 3,
            continuation: seq(48215),
        }),
        horizon: seq(48220),
    };
    let trust = Packed {
        rows: vec![TrustView {
            position: seq(48213),
            head: TrustHead {
                digest: digest("a"),
                state: TrustState::Granted,
                worktree: Some("/worktrees/trust-a".to_owned()),
            },
        }],
        overflow: None,
        horizon: seq(48220),
    };
    let mut frames = stream_data_samples(epoch, first_id, second_id, second_fact, facts, trust);
    frames.extend(stream_boundary_samples(
        epoch, next_epoch, first_id, second_id, cursor,
    ));
    law("StreamFrame", &frames);
}

fn agent_message_samples(first_id: CanonicalUuid, second_id: CanonicalUuid) -> [AgentMessage; 4] {
    [
        AgentMessage {
            prose_origin: None,
            lane: first_id,
            author: second_id,
            locus: MessageLocus {
                member: 1,
                offset: 2,
                length: 3,
                event: 4,
            },
            authored: at("2030-04-12T18:44:09.271Z"),
            authored_synthetic: false,
            observed: None,
            activity: AgentActivityKind::Prose,
            compaction_method: None,
            final_answer: false,
            acknowledgement: Some(MessageAcknowledgement::Acknowledged),
            text: "settled".to_owned(),
            provider_error: None,
            quote_target: Some(wire::QuoteTarget::Tape {
                lane: first_id,
                source: second_id,
                locus: MessageLocus {
                    member: 1,
                    offset: 2,
                    length: 3,
                    event: 4,
                },
                digest: wire::QuoteTarget::text_digest("settled"),
            }),
            directive_contexts: vec![directive_sample(first_id)],
            sends: vec![send_sample(first_id, second_id)],
            quotes: Vec::new(),
            automatic_speech: None,
            media: Vec::new(),
            media_quotes: Vec::new(),
            markdown_images: Vec::new(),
            context_bytes: 0,
        },
        tool_message_sample(first_id, second_id),
        provider_failure_message_sample(first_id, second_id),
        compaction_message_sample(first_id),
    ]
}

/// A tool row whose lane and author cross the first sample's.
fn tool_message_sample(first_id: CanonicalUuid, second_id: CanonicalUuid) -> AgentMessage {
    AgentMessage {
        prose_origin: None,
        lane: second_id,
        author: first_id,
        locus: MessageLocus {
            member: 5,
            offset: 6,
            length: 7,
            event: 8,
        },
        authored: at("2030-04-12T18:44:09.272Z"),
        authored_synthetic: false,
        observed: None,
        activity: AgentActivityKind::Tool,
        compaction_method: None,
        final_answer: false,
        acknowledgement: None,
        text: "Bash".to_owned(),
        provider_error: None,
        quote_target: None,
        directive_contexts: Vec::new(),
        sends: Vec::new(),
        quotes: vec![wire::QuotePresentation::Unavailable {
            target: "spice://quote/v1/unknown".to_owned(),
            reason: wire::QuoteUnavailable::Malformed,
        }],
        automatic_speech: None,
        media: Vec::new(),
        media_quotes: Vec::new(),
        markdown_images: Vec::new(),
        context_bytes: 0,
    }
}

/// A structured provider failure crossing with its report.
fn provider_failure_message_sample(
    first_id: CanonicalUuid,
    second_id: CanonicalUuid,
) -> AgentMessage {
    let mut message = tool_message_sample(first_id, second_id);
    message.activity = AgentActivityKind::Failure;
    message.text = "capacity exhausted".to_owned();
    message.provider_error = Some(AgentProviderError {
        provider: Some("openai".to_owned()),
        model: Some("gpt-5.4".to_owned()),
        http_status: Some(429),
        retryable: true,
    });
    message
}

/// A compaction row its own lane authored.
fn compaction_message_sample(first_id: CanonicalUuid) -> AgentMessage {
    AgentMessage {
        prose_origin: None,
        lane: first_id,
        author: first_id,
        locus: MessageLocus {
            member: 9,
            offset: 10,
            length: 11,
            event: 12,
        },
        authored: at("2030-04-12T18:44:09.273Z"),
        authored_synthetic: false,
        observed: None,
        activity: AgentActivityKind::Compaction,
        compaction_method: Some("snapcompact".to_owned()),
        final_answer: false,
        acknowledgement: None,
        text: "Context compacted".to_owned(),
        provider_error: None,
        quote_target: None,
        directive_contexts: Vec::new(),
        sends: Vec::new(),
        quotes: Vec::new(),
        automatic_speech: None,
        media: Vec::new(),
        media_quotes: Vec::new(),
        markdown_images: Vec::new(),
        context_bytes: 0,
    }
}

/// An operator directive context the first sample answers, delivered to its lane.
fn directive_sample(lane: CanonicalUuid) -> wire::MessageDirective {
    wire::MessageDirective {
        start: 0,
        end: 5,
        quote_target: wire::QuoteTarget::Steering {
            key: "amberfern".to_owned(),
            digest: wire::QuoteTarget::text_digest("settled directive"),
        },
        media_quotes: Vec::new(),
        attribution: wire::QuoteAttribution {
            author: wire::QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            },
            recipients: vec![wire::QuoteRecipient {
                lane,
                occupant: None,
            }],
            packet: false,
        },
        authored: at("2030-04-12T18:44:08.000Z"),
        badge: Some(DirectiveBadge::Taste),
        review: None,
        line: None,
        delivery: None,
    }
}

/// An outgoing message quote whose sending and recipient identities cross.
fn send_sample(first_id: CanonicalUuid, second_id: CanonicalUuid) -> wire::MessageSend {
    wire::MessageSend {
        start: 12,
        end: 19,
        from: first_id,
        sender: second_id,
        recipients: vec![wire::MessageRecipient {
            address: "6K26".parse().expect("fixture address parses"),
            lane: second_id,
            occupant: first_id,
        }],
        lanes: vec![first_id],
        operator: false,
        contexts: Vec::new(),
        source: wire::rooms::TextRange { start: 4, end: 11 },
        body: "message".to_owned(),
    }
}

fn stream_request_samples(
    epoch: CanonicalUuid,
    first_id: CanonicalUuid,
    second_id: CanonicalUuid,
    cursor: StreamCursor,
) -> Vec<StreamRequest> {
    vec![
        StreamRequest::Subscribe {
            v: WireVersion::CURRENT,
            id: first_id,
            epoch,
            subjects: vec![ViewSubject::Facts],
            after: None,
        },
        StreamRequest::Subscribe {
            v: WireVersion::CURRENT,
            id: second_id,
            epoch,
            subjects: vec![ViewSubject::Facts, ViewSubject::Trust],
            after: Some(cursor),
        },
        StreamRequest::Continue {
            v: WireVersion::CURRENT,
            id: first_id,
            epoch,
            subject: ViewSubject::Trust,
            cursor,
        },
        StreamRequest::Thread {
            v: WireVersion::CURRENT,
            id: second_id,
            epoch,
            cursor,
        },
        StreamRequest::MessageHistory {
            v: WireVersion::CURRENT,
            id: first_id,
            epoch,
            frontiers: vec![MessageHistoryCursor {
                generation: 7,
                lane: second_id,
                before: Some(MessageLocus {
                    member: 1,
                    offset: 2,
                    length: 3,
                    event: 4,
                }),
            }],
        },
    ]
}

fn stream_data_samples(
    epoch: CanonicalUuid,
    first_id: CanonicalUuid,
    second_id: CanonicalUuid,
    fact: StreamFact,
    facts: Packed<StreamFact>,
    trust: Packed<TrustView>,
) -> Vec<StreamFrame> {
    let history_message = wire::AgentMessage {
        prose_origin: None,
        lane: second_id,
        author: first_id,
        locus: MessageLocus {
            member: 1,
            offset: 2,
            length: 3,
            event: 4,
        },
        authored: at("2030-04-12T18:44:09.271Z"),
        authored_synthetic: false,
        observed: None,
        activity: wire::AgentActivityKind::Prose,
        compaction_method: None,
        final_answer: false,
        acknowledgement: None,
        text: "older retained prose".to_owned(),
        provider_error: None,
        quote_target: None,
        directive_contexts: Vec::new(),
        sends: Vec::new(),
        quotes: Vec::new(),
        automatic_speech: None,
        media: Vec::new(),
        media_quotes: Vec::new(),
        markdown_images: Vec::new(),
        context_bytes: 0,
    };
    vec![
        StreamFrame::Facts {
            v: WireVersion::CURRENT,
            id: first_id,
            epoch,
            packed: facts.clone(),
        },
        StreamFrame::Trust {
            v: WireVersion::CURRENT,
            id: first_id,
            epoch,
            packed: trust,
        },
        StreamFrame::Thread {
            v: WireVersion::CURRENT,
            id: second_id,
            epoch,
            packed: facts,
        },
        StreamFrame::MessageHistory {
            v: WireVersion::CURRENT,
            id: first_id,
            epoch,
            requested: vec![MessageHistoryCursor {
                generation: 7,
                lane: second_id,
                before: Some(MessageLocus {
                    member: 5,
                    offset: 6,
                    length: 7,
                    event: 8,
                }),
            }],
            generation: wire::TranscriptGeneration {
                generation: 7,
                horizon: seq(20),
                messages: vec![history_message],
                frontiers: vec![wire::TranscriptLaneFrontier {
                    lane: second_id,
                    state: wire::TranscriptLaneState::Truncated,
                    before: Some(MessageLocus {
                        member: 5,
                        offset: 4,
                        length: 3,
                        event: 2,
                    }),
                }],
            },
        },
        StreamFrame::Fact {
            v: WireVersion::CURRENT,
            id: first_id,
            epoch,
            fact,
        },
    ]
}

fn stream_boundary_samples(
    epoch: CanonicalUuid,
    next_epoch: CanonicalUuid,
    first_id: CanonicalUuid,
    second_id: CanonicalUuid,
    cursor: StreamCursor,
) -> Vec<StreamFrame> {
    vec![
        StreamFrame::Refusal {
            v: WireVersion::CURRENT,
            id: first_id,
            epoch,
            refusal: stale_witness(),
        },
        StreamFrame::Fault {
            v: WireVersion::CURRENT,
            epoch,
            id: None,
            fault: moved_epoch(),
        },
        StreamFrame::Fault {
            v: WireVersion::CURRENT,
            epoch,
            id: Some(second_id),
            fault: Fault {
                class: FaultClass::Store,
                reason: "stream::heads-failed".to_owned(),
                detail: "authority snapshot failed".to_owned(),
            },
        },
        StreamFrame::Overflow {
            v: WireVersion::CURRENT,
            id: first_id,
            epoch,
            after: cursor,
            dropped: 7,
        },
        StreamFrame::Closing {
            v: WireVersion::CURRENT,
            id: second_id,
            epoch: next_epoch,
            after: StreamCursor {
                epoch: next_epoch,
                seq: seq(48220),
            },
        },
    ]
}

fn opaque_stream_envelope(position: u64, cause: Option<u64>, validation: &str) -> StreamFact {
    let typed = Envelope {
        v: WireVersion::CURRENT,
        seq: seq(position),
        at: at("2030-04-12T18:44:09.271Z"),
        actor: Actor::TEST_PRINCIPAL,
        origin: "task:SPICE-1kKRRsDPx".parse().expect("origin"),
        cause: cause.map(seq),
        fact: Fact::new(body(validation)),
    };
    let opaque: Envelope<AnyFact> =
        wire::decode(&wire::encode(&typed).expect("typed envelope encodes"))
            .expect("opaque envelope decodes");
    StreamFact::from(opaque)
}

fn stream_lane_seated(position: u64, cause: Option<u64>) -> StreamFact {
    let typed = Envelope {
        v: WireVersion::CURRENT,
        seq: seq(position),
        at: at("2030-04-12T18:44:09.273Z"),
        actor: Actor::Spice,
        origin: "resident:018f82d4-6f59-7d06-92e3-bce9131163c3"
            .parse()
            .expect("origin"),
        cause: cause.map(seq),
        fact: Fact::new(body::LaneSeated {
            lane: uuid("018f82d4-6f59-7d06-92e3-bce9131163d8"),
            team: uuid("018f82d4-6f59-7d06-92e3-bce9131163d8"),
            capability: uuid("018f82d4-6f59-7d06-92e3-bce9131163d9"),
            name: Some("Ava".to_owned()),
            worktree: "/tmp/lane".to_owned(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("vendor"),
            model: None,
            effort: None,
        }),
    };
    let opaque: Envelope<AnyFact> =
        wire::decode(&wire::encode(&typed).expect("lane envelope encodes"))
            .expect("lane envelope decodes");
    StreamFact::from(opaque)
}

/// Concrete command predicates and their nested state vocabularies.
///
/// Every type here derives `Ord` and `Hash` over a declared sequence rather
/// than over its spelling, so each states that sequence in full and the
/// fixture pins it. Reordering a variant is then a failing test naming the
/// pair that moved, not a silent change to how a rendered column sorts.
fn cover_witness_values() {
    ranked_law(
        "Witness",
        &[
            Witness::Task {
                task: stamp("1kKRRsDPx"),
                head: seq(48190),
            },
            Witness::Workflow {
                task: stamp("1kKRRsDPx"),
                head: seq(48190),
                phase: "todo".parse().expect("phase"),
            },
            Witness::Directive {
                steer: uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"),
                disposition: Disposition::Live,
            },
            // Recovery names the same stable subject without collapsing its
            // lapsed and dead-lettered states into one false disposition.
            Witness::Recoverable {
                steer: uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"),
            },
            Witness::Consent {
                request: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                answer: Answer::Asked,
            },
            Witness::Trust {
                digest: digest("c"),
                standing: TrustState::Pended,
            },
            Witness::Transition {
                from: ResidentState::Seated,
                to: ResidentState::Quiescing,
            },
        ],
    );
}
