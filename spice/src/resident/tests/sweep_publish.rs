//! One surviving supervisor notice presents every record its sweep accounted.

use super::*;

const FIRST: &[u8] = br#"{"timestamp":"2030-04-12T18:44:09.271Z","type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"first answer without a notice"}]}}
"#;
const SECOND: &[u8] = br#"{"timestamp":"2030-04-12T18:44:10.271Z","type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"second answer without a notice"}]}}
"#;
const THIRD: &[u8] = br#"{"timestamp":"2030-04-12T18:44:11.271Z","type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"third answer with the notice"}]}}
"#;

/// A notice is a wake-up: the records whose own notices the kernel refused are
/// on the tape, the sweep accounts them, and the app sees all of them in
/// tape order rather than only the one record the surviving notice named.
#[test]
fn a_surviving_notice_presents_every_record_its_sweep_accounted() {
    let (root, paths) = resident_fixture("sweep-publish");
    let bus = Bus::start(journal::Journal::in_memory().expect("sweep authority opens"))
        .expect("sweep bus starts");
    let lane = test_uuid(5);
    let occupant = test_uuid(6);
    let epoch = test_uuid(4);
    app_announcement::seat_ingestion_residency(&bus, lane, epoch);
    let tape = tape::Tape::open(paths.lane_tape(lane).join("omp")).expect("the lane tape opens");
    let first = tape.tee(FIRST).expect("the first record is durable");
    let second = tape.tee(SECOND).expect("the second record is durable");
    let third = tape.tee(THIRD).expect("the third record is durable");
    let mut seats = Seats::new();
    seats
        .seat_detached(
            lane,
            occupant,
            Box::new(DetachedFixture {
                dropped: Arc::new(AtomicU64::new(0)),
            }),
        )
        .expect("the sweep fixture occupant is seated");
    let source = message_source(bus.reader(), epoch);
    let publisher = source.message_publisher();
    let mut updates = source.subscribe_messages();
    let mut diagnostics = Vec::new();
    let message_state = std::sync::Mutex::new(MessageState::default());
    let tastes = taste::Bag::new(Vec::new()).expect("empty taste bag");
    let steering = steer::Config::default();
    let generation = publisher.batch().expect("wake generation begins");
    let joining = crate::board_command::JoiningFixture::discover(&root);
    let surfaces = CaptureSurfaces::new(&paths, &tastes, &steering, &generation, joining.joining());

    accept_capture_record(
        &bus,
        epoch,
        &mut seats,
        &surfaces,
        crate::resident::message_surface::TasteRecognition::Direct,
        &noticed(lane, occupant, THIRD, third),
        CaptureViews::new(&message_state, &mut diagnostics),
    )
    .expect("the surviving notice is accepted");

    assert!(generation.commit().expect("wake generation commits"));
    let generation = updates.try_recv().expect("one wake generation publishes");
    assert!(
        updates.try_recv().is_err(),
        "the wake sweep published a partial second generation",
    );
    let presented = generation
        .messages
        .into_iter()
        .map(|message| {
            (
                (message.locus.member, message.locus.offset),
                message.activity,
                message.text,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        presented,
        vec![
            (
                (first.member, first.offset),
                wire::AgentActivityKind::Prose,
                "first answer without a notice".to_owned(),
            ),
            (
                (second.member, second.offset),
                wire::AgentActivityKind::Prose,
                "second answer without a notice".to_owned(),
            ),
            (
                (third.member, third.offset),
                wire::AgentActivityKind::Prose,
                "third answer with the notice".to_owned(),
            ),
        ],
        "the sweep presented something other than its three accounted records: {}",
        String::from_utf8_lossy(&diagnostics),
    );

    // A notice that arrives after its bytes were accounted (the startup overlap)
    // still crosses, and the retained locus refuses the repeat presentation.
    let surfaces = CaptureSurfaces::new(&paths, &tastes, &steering, &publisher, joining.joining());
    accept_capture_record(
        &bus,
        epoch,
        &mut seats,
        &surfaces,
        crate::resident::message_surface::TasteRecognition::Direct,
        &noticed(lane, occupant, FIRST, first),
        CaptureViews::new(&message_state, &mut diagnostics),
    )
    .expect("a late notice for an accounted record is accepted");
    assert!(
        updates.try_recv().is_err(),
        "an already presented record was presented again",
    );
    assert!(
        diagnostics.is_empty(),
        "the sweep raised diagnostics: {}",
        String::from_utf8_lossy(&diagnostics),
    );
    seats.release_all();
    bus.shutdown().expect("sweep bus stops");
    fs::remove_dir_all(root).expect("sweep fixture cleanup");
}

const COMPACTION: &[u8] = br#"{"type":"manual_compaction_end","result":{"summary":"so far","shortSummary":"Archived 38,080 chars","firstKeptEntryId":"e1","tokensBefore":56112,"tokensAfter":33642,"method":"snapcompact"},"aborted":false}
"#;

/// The captured completion announcement is the only source of a compaction
/// boundary: one committed compaction is one `LaneCompacted` fact and one
/// card at the announcement's tape locus, carrying the announced method and
/// summary at the accounting time that read it.
#[test]
fn a_captured_compaction_is_one_fact_and_one_card_at_its_tape_locus() {
    let (root, paths) = resident_fixture("captured-compaction");
    let bus = Bus::start(journal::Journal::in_memory().expect("compaction authority opens"))
        .expect("compaction bus starts");
    let lane = test_uuid(5);
    let occupant = test_uuid(6);
    let epoch = test_uuid(4);
    app_announcement::seat_occupied_ingestion_residency(&bus, lane, occupant, epoch);
    let tape = tape::Tape::open(paths.lane_tape(lane).join("omp")).expect("the lane tape opens");
    let locus = tape.tee(COMPACTION).expect("the announcement is durable");
    let mut seats = Seats::new();
    seats
        .seat_detached(
            lane,
            occupant,
            Box::new(DetachedFixture {
                dropped: Arc::new(AtomicU64::new(0)),
            }),
        )
        .expect("the compaction fixture occupant is seated");
    let source = message_source(bus.reader(), epoch);
    let publisher = source.message_publisher();
    let mut updates = source.subscribe_messages();
    let mut diagnostics = Vec::new();
    let message_state = std::sync::Mutex::new(MessageState::default());
    let tastes = taste::Bag::new(Vec::new()).expect("empty taste bag");
    let steering = steer::Config::default();
    let generation = publisher.batch().expect("wake generation begins");
    let joining = crate::board_command::JoiningFixture::discover(&root);
    let surfaces = CaptureSurfaces::new(&paths, &tastes, &steering, &generation, joining.joining());

    accept_capture_record(
        &bus,
        epoch,
        &mut seats,
        &surfaces,
        crate::resident::message_surface::TasteRecognition::Direct,
        &noticed(lane, occupant, COMPACTION, locus),
        CaptureViews::new(&message_state, &mut diagnostics),
    )
    .expect("the announcement is accepted");
    assert!(generation.commit().expect("wake generation commits"));
    assert!(
        diagnostics.is_empty(),
        "the announcement raised diagnostics: {}",
        String::from_utf8_lossy(&diagnostics),
    );

    let generation = updates.try_recv().expect("one generation publishes");
    let [card] = generation.messages.as_slice() else {
        panic!("one announcement is one card: {:#?}", generation.messages);
    };
    assert_eq!(card.activity, wire::AgentActivityKind::Compaction);
    assert_eq!(
        (card.locus.member, card.locus.offset, card.locus.length),
        (locus.member, locus.offset, locus.length),
    );
    assert_eq!(card.compaction_method.as_deref(), Some("snapcompact"));
    assert_eq!(card.text, "so far");
    assert!(
        card.authored_synthetic && card.observed.is_some(),
        "an undated announcement is placed at its accounting time: {card:#?}",
    );

    let compacted = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts read")
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::body::LaneCompacted>())
        .collect::<Vec<_>>();
    assert_eq!(
        compacted,
        [wire::body::LaneCompacted {
            lane,
            occupant,
            stream: "stdout".to_owned(),
            member: locus.member,
            offset: locus.offset,
            length: locus.length,
        }],
    );
    seats.release_all();
    bus.shutdown().expect("compaction bus stops");
    fs::remove_dir_all(root).expect("compaction fixture cleanup");
}

/// The capture record one supervisor notice materializes for a tape record.
pub(super) fn noticed(
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    bytes: &[u8],
    locus: tape::Locus,
) -> lane::capture::CaptureRecord {
    lane::capture::CaptureRecord {
        lane,
        occupant,
        stream: vendor::RecordStream::Stdout,
        boundary: sill::RecordBoundary::Complete,
        locus,
        declaration: None,
        decoded: listen::decode(listen::Dialect::Omp, bytes, locus)
            .expect("the fixture record decodes under its own locus"),
    }
}
