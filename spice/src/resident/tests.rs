//! Runtime and shutdown regressions for the split resident surface.
//! Long startup and media cases live in child modules to keep this root bounded.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant as Monotonic};

use sill::{ObservationFailure, OutputStream};

use super::*;

// App announcement and capture fixtures live behind one named boundary so
// this resident root stays below the file-shape base, not merely its ratchet.
#[path = "tests/app_announcement.rs"]
mod app_announcement;
#[path = "tests/closed_transcript.rs"]
mod closed_transcript;
#[path = "tests/lane_consent.rs"]
mod lane_consent;
#[path = "tests/media.rs"]
mod media;
#[path = "tests/message_backlog.rs"]
mod message_backlog;
#[path = "tests/narration_prod.rs"]
mod narration_prod;
#[path = "tests/principal.rs"]
mod principal;
#[path = "tests/quote_delivery.rs"]
mod quote_delivery;
#[path = "tests/read_materialization_bounds.rs"]
mod read_materialization_bounds;
#[path = "tests/startup.rs"]
mod startup;
#[path = "tests/startup_window.rs"]
mod startup_window;
#[path = "tests/steering_auto.rs"]
mod steering_auto;
#[path = "tests/steering_heard.rs"]
mod steering_heard;
#[path = "tests/steering_inbox.rs"]
mod steering_inbox;
#[path = "tests/steering_message.rs"]
mod steering_message;
#[path = "tests/steering_vacant.rs"]
mod steering_vacant;
#[path = "tests/sweep_publish.rs"]
mod sweep_publish;

use app_announcement::{
    resident_fixture, seed_lane, seed_lane_in_worktree, seed_vacant_lane_in_worktree,
    seed_vendor_lane_in_worktree, test_uuid,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
fn quote_presenter() -> serve::stream::QuotePresenter {
    serve::stream::QuotePresenter::new(|_| Vec::new())
}

/// A message stream source over `reader` whose role book is unpublished.
fn message_source(reader: bus::BusReader, epoch: CanonicalUuid) -> serve::stream::Source {
    serve::stream::Source::new(
        reader,
        epoch,
        8,
        quote_presenter(),
        serve::RoleBook::default(),
        serve::stream::ConfigRevision::new(move || Ok(epoch)),
    )
}

#[test]
fn consent_initialization_resumes_across_small_materializer_steps() {
    let (root, paths) = resident_fixture("consent-page-resume");
    let mut journal = journal::Journal::open(&paths.journal).expect("consent journal opens");
    let lane = test_uuid(91);
    let mut write = write(Actor::Lane(lane), test_uuid(92), Producer::CommandDoor)
        .expect("consent write starts");
    for request in [test_uuid(93), test_uuid(94)] {
        write
            .fact(
                wire::body::ConsentAsked {
                    request,
                    lane,
                    operation: "fixture operation".to_owned(),
                },
                FactCause::Root,
            )
            .expect("consent request fact builds");
    }
    let applied = journal.apply(write).expect("consent requests commit");
    let bus = Bus::start(journal).expect("consent bus starts");
    let policy =
        consent::Policy::new(consent::Answer::Queue, Vec::new()).expect("fixture consent policy");
    let budget = journal::FactReadBudget::new(1, journal::MAX_RETAINED_FACT_PAGE_BYTES)
        .expect("one-row materializer budget");
    let reads =
        read_materializations::CurrentReads::start_with_budget(&bus, &paths, &policy, budget)
            .expect("consent materializer starts");

    let generation = reads.current().expect("consent pages settle");

    assert_eq!(generation.cursor(), applied.last());
    assert_eq!(generation.materialization_steps(), 2);
    let incremental = generation
        .consent()
        .answer(&policy)
        .expect("incremental consent state remains valid");
    let complete = consent::Queue::from_snapshot(
        &bus.transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("complete consent snapshot reads"),
        &policy,
    )
    .expect("complete consent replay remains valid")
    .answer(&policy)
    .expect("complete consent answer renders");
    assert_eq!(incremental, complete);
    drop(reads);
    bus.shutdown().expect("consent bus stops");
    fs::remove_dir_all(root).expect("consent fixture cleanup");
}

/// A root unique among live processes; one left by a dead process whose pid
/// this run reuses is stale and cleared rather than colliding.
fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "spice-resident-{name}-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    root
}

struct DetachedFixture {
    dropped: Arc<AtomicU64>,
}

impl Drop for DetachedFixture {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

impl lane::occupant::DetachedProcess for DetachedFixture {
    fn group(&self) -> u32 {
        411
    }

    fn termination_grace(&self) -> Duration {
        Duration::from_secs(20)
    }

    fn reap_bound(&self) -> Duration {
        Duration::from_secs(20)
    }

    fn wait_for(
        self: Box<Self>,
        _duration: Duration,
    ) -> Result<lane::occupant::DetachedWait, sill::WaitError> {
        panic!("resident shutdown must not wait for a detached supervisor")
    }

    fn terminate(&self) -> Result<(), sill::WaitError> {
        panic!("resident shutdown must not terminate a detached supervisor")
    }

    fn kill(&self) -> Result<(), sill::WaitError> {
        panic!("resident shutdown must not kill a detached supervisor")
    }

    fn wait(self: Box<Self>) -> Result<sill::WaitOutcome, sill::WaitError> {
        panic!("resident shutdown must not reap a detached supervisor")
    }
}

#[test]
fn resident_shutdown_releases_handles_without_ending_lane_work() {
    let (root, paths) = resident_fixture("resident-detach");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let surface =
        crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    let epoch = test_uuid(1);
    commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
    let mut seats = Seats::new();

    let lane = test_uuid(2);
    let occupant = test_uuid(3);
    let dropped = Arc::new(AtomicU64::new(0));
    seats
        .seat_detached(
            lane,
            occupant,
            Box::new(DetachedFixture {
                dropped: Arc::clone(&dropped),
            }),
        )
        .expect("supervisor handle seats");
    seed_lane(&bus, epoch, lane, occupant, &root);

    fs::create_dir_all(&paths.tape).expect("tape directory");
    let tape = tape::Tape::open(paths.lane_tape(lane).join("omp")).expect("live tape opens");
    tape.tee(b"before resident replacement")
        .expect("supervisor tape captures before shutdown");

    let shutdown_started = Monotonic::now();
    assert!(matches!(
        drive_shutdown(&bus, &paths, epoch, &mut seats, Actor::Spice),
        Outcome::Reply(_)
    ));
    assert!(
        shutdown_started.elapsed() < Duration::from_secs(1),
        "resident replacement waited on the supervisor's four-hour runtime"
    );
    assert!(seats.is_empty());
    assert_eq!(
        dropped.load(Ordering::Relaxed),
        1,
        "the resident released its lightweight supervisor handle"
    );
    bus.verify().expect("Dark authority verifies");
    let snapshot = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts read through bus");
    let holds = |kind| snapshot.facts().iter().any(|fact| fact.fact.kind() == kind);
    assert!(holds(wire::FactKind::UpDark));
    for kind in [
        wire::FactKind::LaneDrained,
        wire::FactKind::LaneKilled,
        wire::FactKind::LaneExited,
        wire::FactKind::LaneTimedOut,
        wire::FactKind::LaneCancelled,
        wire::FactKind::LaneSignalled,
    ] {
        assert!(!holds(kind), "resident detach fabricated {kind:?}");
    }
    let head = bus
        .residency_head()
        .expect("head read")
        .expect("head exists")
        .into_body();
    assert_eq!(head.state, ResidentState::Dark);
    assert_eq!(head.lanes.len(), 1);
    assert_eq!(head.lanes[0].lane, lane);
    assert_eq!(head.lanes[0].occupant, Some(occupant));
    let after = tape
        .tee(b"after resident replacement")
        .expect("supervisor tape remains writable while resident is dark");
    assert_eq!(
        tape.read(after).expect("replacement bytes remain readable"),
        b"after resident replacement"
    );
    drop(tape);
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn a_post_seated_surface_failure_compensates_to_verified_dark() {
    let (root, paths) = resident_fixture("compensation");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let surface =
        crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    let epoch = test_uuid(7);
    let startup = commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
    let original = environment_fault("serve::fixture-failed", "after UpSeated");

    let settled = settle_surface(
        &bus,
        &paths,
        epoch,
        startup,
        &mut Seats::new(),
        Err(original),
    );

    let Outcome::Fault(fault) = settled else {
        panic!("the original startup failure must remain visible");
    };
    assert_eq!(fault.reason, "serve::fixture-failed");
    let head = bus
        .residency_head()
        .expect("head read")
        .expect("head exists")
        .into_body();
    assert_eq!(head.state, ResidentState::Dark);
    assert!(head.lanes.is_empty());
    bus.verify().expect("compensated authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn capture_failure_vacates_and_durably_degrades_the_exact_occupant() {
    let (root, paths) = resident_fixture("capture-failure");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let surface =
        crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    let epoch = test_uuid(1);
    commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
    let lane = test_uuid(2);
    let occupant = test_uuid(3);
    let mut seats = Seats::new();
    seed_lane(&bus, epoch, lane, occupant, &root);

    super::lane_surface::capture_failed(
        &bus,
        epoch,
        &mut seats,
        &lane::capture::CaptureFailure {
            lane,
            occupant,
            failure: ObservationFailure::SinkRefused {
                stream: OutputStream::Stdout,
                detail: "fixture tape refusal".to_owned(),
            },
        },
    )
    .expect("capture failure commits");

    assert!(!seats.occupied(lane));
    let head = current_residency(&bus, epoch)
        .expect("capture residency reads")
        .into_body();
    let row = head
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .expect("capture lane remains durable");
    assert_eq!((row.occupant, row.disposition.as_str()), (None, "degraded"));
    let snapshot = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("capture facts read");
    let degraded = snapshot
        .facts()
        .iter()
        .find(|fact| fact.fact.kind() == wire::FactKind::LaneDegraded)
        .expect("capture degradation is named");
    assert!(
        degraded.fact.body()["marker"]
            .as_str()
            .is_some_and(|marker| marker.contains("fixture tape refusal"))
    );
    bus.shutdown().expect("capture fixture bus stops");
    fs::remove_dir_all(root).expect("capture fixture cleanup");
}

#[test]
fn failed_pending_inbox_start_parks_key_and_reports_exact_requeue() {
    let (root, paths) = resident_fixture("pending-dead-letter");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let surface =
        crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    let epoch = test_uuid(1);
    commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
    let lane = test_uuid(2);
    let occupant = test_uuid(3);
    let directive = test_uuid(4);
    seed_lane(&bus, epoch, lane, occupant, &root);
    let observed = current_residency(&bus, epoch).expect("occupied residency reads");
    let mut vacant = observed.body().clone();
    let row = vacant
        .lanes
        .iter_mut()
        .find(|row| row.lane == lane)
        .expect("fixture lane exists");
    row.occupant = None;
    row.disposition = "vacant".to_owned();
    let mut ending =
        write(Actor::Spice, epoch, Producer::ResidentRecognition).expect("ending write starts");
    super::shutdown::guard_residency(&mut ending, &observed);
    let exited = ending
        .fact(
            wire::body::LaneExited {
                lane,
                occupant,
                code: 17,
            },
            FactCause::Existing(observed.position()),
        )
        .expect("exit fact is valid");
    ending.residency_head(exited, move |_| vacant);
    commit(&bus, ending).expect("vacancy commits");
    let residency = current_residency(&bus, epoch).expect("vacant residency reads");
    let expires: wire::Instant = "2026-08-10T07:07:19.000Z".parse().expect("fixture expiry");
    let at = wire::Instant::new(jiff::Timestamp::now()).expect("steering instant");
    let origin: wire::Origin = format!("resident:{epoch}")
        .parse()
        .expect("steering origin");
    let mut sent = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let body = wire::body::SteerSent {
        sender: Actor::TEST_PRINCIPAL,
        room: None,
        directive,
        fanout: None,
        recipients: Vec::new(),
        key: "1kJK6N3Rx".to_owned(),
        recipient: Actor::Lane(lane),
        text: "do not lose me".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires,
    };
    let pending = sent
        .fact(body.clone(), FactCause::Existing(residency.position()))
        .expect("steering fact is valid");
    sent.steering_head(pending, move |seq| {
        steer::Ledger::sent_head(&body, origin, at, seq).expect("steering head is valid")
    });
    commit(&bus, sent).expect("pending steering commits");
    let woken = super::steering::woken_inbox(&bus, lane).expect("pending steering reads");
    // A supervisor's start failure carries its state.json failure text as
    // detail; the dead-letter line must reproduce it byte for byte.
    let recorded = "lane::cargo-isolation-unavailable; cargo-isolation::native-toolchain-unavailable: /home/bin/rustup";
    let failure = store_fault("lane::supervisor-start-failed", recorded);

    let dead = super::steering::dead_letter_pending(&bus, &paths, epoch, lane, &woken, &failure)
        .expect("terminal start failure parks pending steering");

    assert_eq!(dead.parked.len(), 1);
    assert_eq!(dead.parked[0].key, "1kJK6N3Rx");
    assert_eq!(
        dead.parked[0].requeue,
        format!("spice steer recover {directive}")
    );
    let mut log = Vec::new();
    dead.log(&mut log, "pending-inbox", lane);
    let log = String::from_utf8(log).expect("dead-letter log is UTF-8");
    let line = log.strip_suffix('\n').expect("one terminated line");
    assert!(!line.contains('\n'), "{log}");
    assert!(
        line.contains(" reason=lane::supervisor-start-failed "),
        "{line}"
    );
    assert_eq!(
        line.split_once(" detail=").map(|(_, detail)| detail),
        Some(recorded)
    );
    let snapshot = bus.task_snapshot().expect("dead-letter facts read");
    let ledger = steer::Ledger::from_facts(snapshot.facts().iter()).expect("dead letter replays");
    assert_eq!(
        ledger
            .directive(directive)
            .expect("directive remains durable")
            .disposition(),
        wire::Disposition::DeadLettered,
    );
    let parked_fact = snapshot
        .facts()
        .iter()
        .find(|fact| fact.fact.kind() == wire::FactKind::SteerDeadLettered)
        .expect("distinct dead-letter fact exists");
    assert_eq!(parked_fact.fact.body()["key"], "1kJK6N3Rx");
    assert_eq!(
        parked_fact.fact.body()["requeue"],
        format!("spice steer recover {directive}")
    );
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("dead-letter fixture cleanup");
}

#[test]
fn startup_admits_only_one_ordered_subscription_per_resident_team() {
    let team = test_uuid(2);
    let sibling = test_uuid(5);
    let row = |lane, team, life: &str| wire::LaneResidency {
        lane,
        team,
        name: None,
        worktree: format!("/fixture/{lane}"),
        life: life.to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage: Vec::new(),
        capabilities: Vec::new(),
        session: None,
        disposition: "vacant".to_owned(),
    };
    let lanes = vec![
        row(test_uuid(3), team, "drive"),
        row(test_uuid(4), team, "drive"),
        row(sibling, sibling, "steer"),
    ];
    let current = vec![
        wire::TeamSubscription {
            team,
            formed_at: Some(
                "2030-04-12T18:44:09.271Z"
                    .parse()
                    .expect("fixture formation instant"),
            ),
            drive: true,
            drain: false,
            name: None,
            fast_mode: false,
        },
        wire::TeamSubscription {
            team: sibling,
            formed_at: None,
            drive: true,
            drain: false,
            name: None,
            fast_mode: true,
        },
    ];
    assert_eq!(
        super::startup_subscriptions(&lanes, current.clone())
            .expect("the current complete table is admitted"),
        current,
    );
    assert_eq!(
        super::startup_subscriptions(&[], Vec::new()).expect("no teams need no table"),
        Vec::new(),
    );

    let misaligned = [
        ("an absent table", Vec::new()),
        ("a partial table", vec![current[0].clone()]),
        (
            "an unordered table",
            vec![current[1].clone(), current[0].clone()],
        ),
    ];
    for (case, stored) in misaligned {
        let Err(Outcome::Fault(fault)) = super::startup_subscriptions(&lanes, stored) else {
            panic!("{case} must fault rather than derive policy")
        };
        assert_eq!(fault.reason, "team::subscription-misaligned", "{case}");
    }
    let Err(Outcome::Fault(fault)) = super::startup_subscriptions(
        &lanes,
        vec![current[0].clone(), current[0].clone(), current[1].clone()],
    ) else {
        panic!("duplicate subscription authority must fault")
    };
    assert_eq!(fault.reason, "team::subscription-ambiguous");
}

#[test]
fn startup_admits_a_formation_instant_exactly_for_each_teamed_identity() {
    let team = test_uuid(2);
    let row = |lane| wire::LaneResidency {
        lane,
        team,
        name: None,
        worktree: format!("/fixture/{lane}"),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage: Vec::new(),
        capabilities: Vec::new(),
        session: None,
        disposition: "vacant".to_owned(),
    };
    let subscription = |formed: bool| wire::TeamSubscription {
        team,
        formed_at: formed.then(|| {
            "2030-04-12T18:44:09.271Z"
                .parse()
                .expect("fixture formation instant")
        }),
        drive: true,
        drain: false,
        name: None,
        fast_mode: false,
    };
    let self_identified = vec![row(team)];
    let lone_member = vec![row(test_uuid(3))];
    let pair = vec![row(test_uuid(3)), row(test_uuid(4))];
    for (case, lanes, formed) in [
        ("a self-identified seat", &self_identified, false),
        ("one member under another identity", &lone_member, true),
        ("several members", &pair, true),
    ] {
        assert_eq!(
            super::startup_subscriptions(lanes, vec![subscription(formed)])
                .unwrap_or_else(|_| panic!("{case} is admitted with its formation state")),
            vec![subscription(formed)],
        );
        let Err(Outcome::Fault(fault)) =
            super::startup_subscriptions(lanes, vec![subscription(!formed)])
        else {
            panic!("{case} refuses the opposite formation state")
        };
        assert_eq!(fault.reason, "team::formation-misaligned", "{case}");
    }
}
