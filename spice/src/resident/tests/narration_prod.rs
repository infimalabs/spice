//! A seated lane that keeps running tools without visible prose is prodded,
//! and the prod stops once it speaks.

use super::*;
use sweep_publish::noticed;
use wire::body::{NarrationCleared, NarrationProdded, NarrationRelief};

/// Tool moments the fixture lane may run without visible prose before the prod.
const BOUND: u32 = 3;

const PROSE: &[u8] = br#"{"timestamp":"2030-04-12T18:44:20.271Z","type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"three listings in, nothing renamed yet"}]}}
"#;

/// One assistant message that calls a shell command.
fn command(second: u8) -> Vec<u8> {
    let mut bytes = format!(
        r#"{{"timestamp":"2030-04-12T18:44:{second:02}.271Z","type":"message_end","message":{{"role":"assistant","content":[{{"type":"toolCall","id":"call-{second}","name":"bash","arguments":{{"command":"ls"}}}}],"stopReason":"toolUse"}}}}"#
    )
    .into_bytes();
    bytes.push(b'\n');
    bytes
}

/// One seated lane whose supervisor notices reach the resident sweep.
struct Occupied<'a> {
    bus: &'a Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    tape: tape::Tape,
    seats: Seats,
    state: std::sync::Mutex<MessageState>,
    diagnostics: Vec<u8>,
}

impl Occupied<'_> {
    /// Tee one record onto the lane tape and sweep it as its own notice.
    fn accept<P: AgentMessagePublisher>(
        &mut self,
        surfaces: &CaptureSurfaces<'_, P>,
        bytes: &[u8],
    ) {
        let locus = self.tape.tee(bytes).expect("the record is durable");
        accept_capture_record(
            self.bus,
            self.epoch,
            &mut self.seats,
            surfaces,
            crate::resident::message_surface::TasteRecognition::Direct,
            &noticed(self.lane, self.occupant, bytes, locus),
            CaptureViews::new(&self.state, &mut self.diagnostics),
        )
        .expect("the notice is accepted");
    }
}

/// Every prod the resident recorded, in journal order.
fn prods(bus: &Bus) -> Vec<NarrationProdded> {
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("the journal reads")
    .facts()
    .iter()
    .filter_map(|envelope| envelope.fact.read::<NarrationProdded>())
    .collect()
}

/// Every clearance the resident recorded, in journal order.
fn clearances(bus: &Bus) -> Vec<NarrationCleared> {
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("the journal reads")
    .facts()
    .iter()
    .filter_map(|envelope| envelope.fact.read::<NarrationCleared>())
    .collect()
}

/// The resident measures the lane from its tape, records the prod as a fact,
/// and clears it on the first visible prose; further tool calls under the
/// bound stay silent.
#[test]
fn a_silent_lane_is_prodded_and_falls_silent_once_it_speaks() {
    let (root, paths) = resident_fixture("narration-prod");
    let bus = Bus::start(journal::Journal::open(&paths.journal).expect("narration journal opens"))
        .expect("narration bus starts");
    let lane = test_uuid(5);
    let occupant = test_uuid(6);
    let epoch = test_uuid(4);
    app_announcement::seat_occupied_ingestion_residency(&bus, lane, occupant, epoch);
    let mut seats = Seats::new();
    seats
        .seat_detached(
            lane,
            occupant,
            Box::new(DetachedFixture {
                dropped: Arc::new(AtomicU64::new(0)),
            }),
        )
        .expect("the narration fixture occupant is seated");
    let mut occupied = Occupied {
        bus: &bus,
        epoch,
        lane,
        occupant,
        tape: tape::Tape::open(paths.lane_tape(lane).join("omp")).expect("the lane tape opens"),
        seats,
        state: std::sync::Mutex::new(MessageState::default()),
        diagnostics: Vec::new(),
    };
    let source = message_source(bus.reader(), epoch);
    let publisher = source.message_publisher();
    let tastes = taste::Bag::new(Vec::new()).expect("empty taste bag");
    let steering = steer::Config::default();
    let joining = crate::board_command::JoiningFixture::discover(&root);
    let surfaces = CaptureSurfaces::new(&paths, &tastes, &steering, &publisher, joining.joining())
        .narration(BOUND);

    occupied.accept(&surfaces, &command(1));
    occupied.accept(&surfaces, &command(2));
    assert!(prods(&bus).is_empty(), "a lane under the bound was prodded");

    occupied.accept(&surfaces, &command(3));
    assert_eq!(
        prods(&bus),
        vec![NarrationProdded {
            lane,
            occupant,
            moments: BOUND,
            count: 1,
        }],
    );

    occupied.accept(&surfaces, PROSE);
    assert_eq!(
        clearances(&bus),
        vec![NarrationCleared {
            lane,
            occupant,
            moments: BOUND,
            prods: 1,
            relief: NarrationRelief::Prose,
        }],
    );

    occupied.accept(&surfaces, &command(4));
    occupied.accept(&surfaces, &command(5));
    assert_eq!(
        prods(&bus).len(),
        1,
        "a lane that spoke was prodded again under the bound"
    );
    assert!(
        occupied.diagnostics.is_empty(),
        "the sweep raised diagnostics: {}",
        String::from_utf8_lossy(&occupied.diagnostics),
    );

    occupied.seats.release_all();
    drop(occupied);
    bus.shutdown().expect("narration bus stops");
    fs::remove_dir_all(root).expect("narration fixture cleanup");
}
