//! A seat that closes presents what its occupant left on the tape.

use super::*;

const EARLIER: &[u8] = br#"{"timestamp":"2030-04-12T18:44:09.271Z","type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"earlier answer whose notice was refused"}]}}
"#;
const LAST: &[u8] = br#"{"timestamp":"2030-04-12T18:44:10.271Z","type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"final answer written moments before exit"}]}}
"#;
const MIDDLE: &[u8] = br#"{"timestamp":"2030-04-12T18:44:09.771Z","type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"middle answer another pass accounted"}]}}
"#;

/// The exit is accounted before the last records' notices are read, so the
/// closing sweep presents them itself, and a notice that arrives afterwards
/// is refused by name rather than dropped.
#[test]
fn a_closing_seat_presents_every_record_its_occupant_left_behind() {
    let (root, paths) = resident_fixture("closed-transcript");
    let bus = Bus::start(journal::Journal::in_memory().expect("closing authority opens"))
        .expect("closing bus starts");
    let lane = test_uuid(5);
    let occupant = test_uuid(6);
    let epoch = test_uuid(4);
    app_announcement::seat_ingestion_residency(&bus, lane, epoch);
    let tape = tape::Tape::open(paths.lane_tape(lane).join("omp")).expect("the lane tape opens");
    let earlier = tape.tee(EARLIER).expect("the earlier record is durable");
    let last = tape.tee(LAST).expect("the last record is durable");
    let mut seats = Seats::new();
    seats
        .seat_detached(
            lane,
            occupant,
            Box::new(DetachedFixture {
                dropped: Arc::new(AtomicU64::new(0)),
            }),
        )
        .expect("the closing fixture occupant is seated");
    let source = message_source(bus.reader(), epoch);
    let publisher = source.message_publisher();
    let mut updates = source.subscribe_messages();
    let mut diagnostics = Vec::new();
    let message_state = std::sync::Mutex::new(MessageState::default());
    let tastes = taste::Bag::new(Vec::new()).expect("empty taste bag");
    let steering = steer::Config::default();
    let generation = publisher.batch().expect("closing generation begins");
    let joining = crate::board_command::JoiningFixture::discover(&root);
    let surfaces = CaptureSurfaces::new(&paths, &tastes, &steering, &generation, joining.joining());

    drop(seats.vacate(lane));
    for (closed_lane, closed_occupant) in seats.take_vacated() {
        message_surface::present_closed_transcript(
            &bus,
            epoch,
            &surfaces,
            message_surface::TasteRecognition::Direct,
            closed_lane,
            closed_occupant,
            CaptureViews::new(&message_state, &mut diagnostics),
        )
        .expect("the closing sweep presents");
    }
    assert!(generation.commit().expect("closing generation commits"));

    let generation = updates
        .try_recv()
        .expect("one closing generation publishes");
    assert!(
        updates.try_recv().is_err(),
        "the closing sweep published a partial second generation",
    );
    let presented = generation
        .messages
        .into_iter()
        .map(|message| ((message.locus.member, message.locus.offset), message.text))
        .collect::<Vec<_>>();
    assert_eq!(
        presented,
        vec![
            (
                (earlier.member, earlier.offset),
                "earlier answer whose notice was refused".to_owned(),
            ),
            (
                (last.member, last.offset),
                "final answer written moments before exit".to_owned(),
            ),
        ],
        "the closing sweep presented something other than the two records left behind: {}",
        String::from_utf8_lossy(&diagnostics),
    );
    assert!(
        diagnostics.is_empty(),
        "the closing sweep raised diagnostics: {}",
        String::from_utf8_lossy(&diagnostics),
    );

    let late = supervisor::Notice::Record {
        lane,
        occupant,
        stream: "stdout".to_owned(),
        boundary: "complete".to_owned(),
        locus: wire::TapeLocus {
            member: last.member,
            offset: last.offset,
            length: last.length,
        },
    };
    let residency = current_residency(&bus, epoch).expect("the residency head reads");
    let materialized = capture::materialize(&paths, residency.body(), late, &mut diagnostics)
        .expect("a late notice is refused rather than faulted");
    assert!(
        materialized.is_none(),
        "a late notice materialized a record"
    );
    let spoken = String::from_utf8_lossy(&diagnostics);
    assert!(
        spoken.contains(&format!(
            "lane {lane} record notice from occupant {occupant} after its seat closed"
        )),
        "the late notice was refused silently: {spoken:?}",
    );

    seats.release_all();
    bus.shutdown().expect("closing bus stops");
    fs::remove_dir_all(root).expect("closing fixture cleanup");
}

/// A closing sweep whose live hold another accounting pass overtook — as an
/// outbox submission does when it accounts its own packet — accounts from the
/// head that pass left and still presents every record no pass presented,
/// including those that pass accounted, rather than refusing the batch and
/// losing the occupant's last message.
#[test]
fn a_closing_sweep_after_another_pass_accounted_presents_the_last_message() {
    let (root, paths) = resident_fixture("overtaken-hold");
    let bus = Bus::start(journal::Journal::in_memory().expect("overtaken authority opens"))
        .expect("overtaken bus starts");
    let lane = test_uuid(5);
    let occupant = test_uuid(6);
    let epoch = test_uuid(4);
    app_announcement::seat_ingestion_residency(&bus, lane, epoch);
    let tape = tape::Tape::open(paths.lane_tape(lane).join("omp")).expect("the lane tape opens");
    tape.tee(EARLIER).expect("the held record is durable");
    let mut diagnostics = Vec::new();
    let (_, live) = super::ingest_transcripts(
        &bus,
        &paths,
        Some(lane),
        lane::ingest::Bound::LIVE,
        lane::ingest::Coalescing::new(),
        &mut diagnostics,
    )
    .expect("the live pass holds the earlier record");
    let middle = tape.tee(MIDDLE).expect("the overtaken record is durable");
    super::ingest_transcripts(
        &bus,
        &paths,
        Some(lane),
        lane::ingest::Bound::IMMEDIATE,
        lane::ingest::Coalescing::new(),
        &mut diagnostics,
    )
    .expect("the second pass accounts past the hold");
    let last = tape.tee(LAST).expect("the last record is durable");
    let mut seats = Seats::new();
    seats
        .seat_detached(
            lane,
            occupant,
            Box::new(DetachedFixture {
                dropped: Arc::new(AtomicU64::new(0)),
            }),
        )
        .expect("the overtaken fixture occupant is seated");
    let source = message_source(bus.reader(), epoch);
    let publisher = source.message_publisher();
    let mut updates = source.subscribe_messages();
    let message_state = std::sync::Mutex::new(MessageState::holding(live));
    let tastes = taste::Bag::new(Vec::new()).expect("empty taste bag");
    let steering = steer::Config::default();
    let generation = publisher.batch().expect("closing generation begins");
    let joining = crate::board_command::JoiningFixture::discover(&root);
    let surfaces = CaptureSurfaces::new(&paths, &tastes, &steering, &generation, joining.joining());

    drop(seats.vacate(lane));
    for (closed_lane, closed_occupant) in seats.take_vacated() {
        message_surface::present_closed_transcript(
            &bus,
            epoch,
            &surfaces,
            message_surface::TasteRecognition::Direct,
            closed_lane,
            closed_occupant,
            CaptureViews::new(&message_state, &mut diagnostics),
        )
        .expect("the closing sweep presents");
    }
    assert!(
        generation.commit().expect("closing generation commits"),
        "the closing sweep presented nothing: {}",
        String::from_utf8_lossy(&diagnostics),
    );

    let presented = updates
        .try_recv()
        .expect("one closing generation publishes")
        .messages
        .into_iter()
        .map(|message| ((message.locus.member, message.locus.offset), message.text))
        .collect::<Vec<_>>();
    assert_eq!(
        presented,
        vec![
            (
                (middle.member, middle.offset),
                "middle answer another pass accounted".to_owned(),
            ),
            (
                (last.member, last.offset),
                "final answer written moments before exit".to_owned(),
            ),
        ],
        "the closing sweep lost or replayed a record: {}",
        String::from_utf8_lossy(&diagnostics),
    );
    assert!(
        diagnostics.is_empty(),
        "the overtaken hold raised diagnostics: {}",
        String::from_utf8_lossy(&diagnostics),
    );

    seats.release_all();
    bus.shutdown().expect("overtaken bus stops");
    fs::remove_dir_all(root).expect("overtaken fixture cleanup");
}

/// One continuing-session declaration for `lane` under `occupant`.
fn continuing_session(
    lane: CanonicalUuid,
    epoch: CanonicalUuid,
    occupant: CanonicalUuid,
) -> wire::body::LaneSessionDeclared {
    wire::body::LaneSessionDeclared {
        lane,
        occupant,
        worktree: "/worktree".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        launch: wire::body::LaneLaunchIdentity::new("model", "effort"),
        epoch,
        stream: "session".to_owned(),
        source: test_uuid(80),
        identity: "continuing-session".to_owned(),
        member: 1,
        offset: 2,
        length: 3,
    }
}

/// Declare `occupant`'s session holding one conversation card.
fn seat_conversation(
    state: &mut MessageState,
    lane: CanonicalUuid,
    epoch: CanonicalUuid,
    occupant: CanonicalUuid,
) {
    use spice::rooms::evidence::{Conversation, Evidence};
    use spice::rooms::profile::EvidenceKey;

    let conversation = Conversation::read(
        "The closing sweep vacates an occupant binding while retaining the profile evidence \
         the departed session gathered for its successor.",
    )
    .expect("enough distinct terms for one conversation card");
    state
        .profiles
        .declare(&continuing_session(lane, epoch, occupant));
    state.profiles.insert(
        lane,
        occupant,
        Evidence::conversation(
            EvidenceKey::Conversation {
                source: test_uuid(80),
                member: 1,
                offset: 10,
                length: 20,
                caused_by: Vec::new(),
            },
            &conversation,
        ),
    );
}

/// The closing sweep forgets a departed occupant's exact binding while its
/// lane's profile and retained conversation stay for every later occupant.
#[test]
fn a_closing_seat_forgets_its_binding_but_keeps_its_profile() {
    let (root, paths) = resident_fixture("closed-transcript-vacancy");
    let bus = Bus::start(journal::Journal::in_memory().expect("closing authority opens"))
        .expect("closing bus starts");
    let lane = test_uuid(5);
    let occupant = test_uuid(6);
    let epoch = test_uuid(4);
    app_announcement::seat_ingestion_residency(&bus, lane, epoch);
    let mut seats = Seats::new();
    seats
        .seat_detached(
            lane,
            occupant,
            Box::new(DetachedFixture {
                dropped: Arc::new(AtomicU64::new(0)),
            }),
        )
        .expect("the closing fixture occupant is seated");
    let message_state = std::sync::Mutex::new(MessageState::default());
    seat_conversation(
        &mut message_state.lock().expect("message state locks"),
        lane,
        epoch,
        occupant,
    );
    let tastes = taste::Bag::new(Vec::new()).expect("empty taste bag");
    let steering = steer::Config::default();
    let source = message_source(bus.reader(), epoch);
    let publisher = source.message_publisher();
    let generation = publisher.batch().expect("closing generation begins");
    let joining = crate::board_command::JoiningFixture::discover(&root);
    let surfaces = CaptureSurfaces::new(&paths, &tastes, &steering, &generation, joining.joining());
    let mut diagnostics = Vec::new();

    drop(seats.vacate(lane));
    for (closed_lane, closed_occupant) in seats.take_vacated() {
        message_surface::present_closed_transcript(
            &bus,
            epoch,
            &surfaces,
            message_surface::TasteRecognition::Direct,
            closed_lane,
            closed_occupant,
            CaptureViews::new(&message_state, &mut diagnostics),
        )
        .expect("the closing sweep presents");
    }
    drop(generation);

    let state = message_state.lock().expect("message state locks");
    assert!(
        state.profiles.session(lane, occupant).is_none(),
        "the vacated binding survived the closing sweep",
    );
    let retained = state
        .profiles
        .profile(lane)
        .expect("the lane profile outlives its occupant")
        .evidence()
        .conversation_messages()
        .count();
    assert_eq!(
        retained, 1,
        "the lane profile lost its retained conversation"
    );
    drop(state);

    seats.release_all();
    bus.shutdown().expect("closing bus stops");
    fs::remove_dir_all(root).expect("closing fixture cleanup");
}

/// A failed batch acquisition leaves the seat ledger as the retry authority.
#[test]
fn closing_batch_failure_does_not_take_vacancies() {
    let mut seats = Seats::new();
    for (lane, occupant) in [(test_uuid(2), test_uuid(12)), (test_uuid(1), test_uuid(11))] {
        seats
            .seat_detached(
                lane,
                occupant,
                Box::new(DetachedFixture {
                    dropped: Arc::new(AtomicU64::new(0)),
                }),
            )
            .expect("the fixture occupant is seated");
    }
    drop(seats.vacate(test_uuid(2)));
    drop(seats.vacate(test_uuid(1)));

    let failed = runtime::reap::begin_vacated_batch(&mut seats, || -> CommandResult<()> {
        Err(Outcome::Fault(wire::Fault {
            class: wire::FaultClass::Store,
            reason: "serve::message-publish-failed".to_owned(),
            detail: "batch lock unavailable".to_owned(),
        }))
    })
    .expect_err("batch acquisition fails");
    assert!(matches!(failed, Outcome::Fault(fault) if fault.class == wire::FaultClass::Store));
    assert!(
        seats.has_vacated(),
        "batch failure drained the vacancy ledger"
    );

    let (batch, vacated) = runtime::reap::begin_vacated_batch(&mut seats, || Ok("batch"))
        .expect("the next batch begins")
        .expect("vacancies remain");
    assert_eq!(batch, "batch");
    assert_eq!(
        vacated,
        vec![(test_uuid(1), test_uuid(11)), (test_uuid(2), test_uuid(12)),],
        "a successful take is deterministic rather than vacate ordered"
    );
    assert!(!seats.has_vacated());
}

/// One terminal presentation cannot strand later exact profile bindings.
#[test]
fn closing_sweep_attempts_every_vacancy_before_returning_the_first_failure() {
    let epoch = test_uuid(90);
    let first = (test_uuid(1), test_uuid(11));
    let later = (test_uuid(2), test_uuid(12));
    let message_state = std::sync::Mutex::new(MessageState::default());
    {
        let mut state = message_state.lock().expect("message state locks");
        state
            .profiles
            .declare(&continuing_session(first.0, epoch, first.1));
        state
            .profiles
            .declare(&continuing_session(later.0, epoch, later.1));
    }
    let mut attempted = Vec::new();
    let mut diagnostic = Vec::new();

    let failure =
        runtime::reap::attempt_vacated(vec![first, later], &mut diagnostic, |lane, occupant, _| {
            attempted.push((lane, occupant));
            message_state
                .lock()
                .expect("message state locks")
                .profiles
                .vacate(lane, occupant);
            if (lane, occupant) == first {
                Err(Outcome::Fault(wire::Fault {
                    class: wire::FaultClass::Protocol,
                    reason: "transcript::record-invalid".to_owned(),
                    detail: "the first closing record is malformed".to_owned(),
                }))
            } else {
                Err(Outcome::Fault(wire::Fault {
                    class: wire::FaultClass::Store,
                    reason: "journal::commit-failed".to_owned(),
                    detail: "database is locked".to_owned(),
                }))
            }
        })
        .expect_err("the first terminal presentation is returned");

    assert!(
        matches!(
            failure,
            Outcome::Fault(fault)
                if fault.class == wire::FaultClass::Protocol
                    && fault.reason == "transcript::record-invalid"
        ),
        "the later deferred store fault replaced the first terminal outcome"
    );
    assert_eq!(attempted, vec![first, later], "a later vacancy was skipped");
    let state = message_state.lock().expect("message state locks");
    assert!(state.profiles.session(first.0, first.1).is_none());
    assert!(state.profiles.session(later.0, later.1).is_none());
    assert_eq!(
        String::from_utf8(diagnostic).expect("diagnostic is text"),
        "closing transcript sweep deferred to the next reconciliation: journal::commit-failed: database is locked\n"
    );
}
