//! Occupant exits decided by the typed prompt lifecycle.

use std::fs;
use std::path::PathBuf;

use lane::capture::CaptureExit;
use lane::occupant::Seats;

use super::reaped_seat::{
    fact_kinds, fixture_root, occupied_seed, occupied_seed_with_session, residency_row,
    runtime_paths, session,
};
use super::*;
use crate::resident::runtime::reap::account_exit;
use crate::resident::supervisor::test_support::record_exited_for_tests;
use crate::resident::supervisor::{Adoption, adopt};
use crate::resident::{CommandResult, RuntimePaths, lifecycle};

const REFUSAL: &str =
    "Refusal (reasoning_extraction): This request was blocked by our content filtering policy.";

const DECLARATION: &[u8] = br#"{"id":"spice-state","type":"response","command":"get_state","success":true,"data":{"sessionId":"018f7777-7777-7444-8888-999999999999"}}
"#;
const PROMPT: &[u8] = br#"{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"Resume the claim."}]}}
"#;
const REFUSED_RESULT: &[u8] = br#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"error","sessionSettled":true,"error":{"message":"Refusal (reasoning_extraction): This request was blocked by our content filtering policy.","provider":"anthropic","model":"claude-opus-4-6","httpStatus":400,"retryable":false}}
"#;
const TRAILING_WIDGET: &[u8] =
    br#"{"type":"extension_ui_request","id":"1583f94d24ebac0b","method":"setWidget"}
"#;
const FINISHED_PROSE: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Finished the claim."}],"stopReason":"stop","usage":{"input":7,"cacheRead":5,"cacheWrite":3,"output":2}}}
"#;
const FINISHED_RESULT: &[u8] = br#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"completed","sessionSettled":true}
"#;
const COMPLETED_ASIDE: &[u8] = br#"{"type":"prompt_result","id":"spice-aside-1","agentInvoked":false,"status":"completed","sessionSettled":false}
"#;
/// The occupant's answer headed with the key `operator_direction_write` addresses to the lane.
const ACKED_PROSE: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"ACK 1kKRRsDSx: Finished the claim."}],"stopReason":"stop","usage":{"input":7,"cacheRead":5,"cacheWrite":3,"output":2}}}
"#;
const ACKED_RESULT: &[u8] = br#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"completed","sessionSettled":true}
"#;

#[test]
fn an_exit_whose_only_turn_the_vendor_refused_holds_the_lane_before_any_reseat() {
    let fixture = VendorExitFixture::new("refused", &[PROMPT, REFUSED_RESULT, TRAILING_WIDGET]);
    let config = lane::Config::default();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    let before = fact_kinds(&fixture.bus);
    let mut diagnostic = Vec::new();

    fixture
        .exit(&mut lifecycle, &mut diagnostic)
        .expect("the refused exit is accounted");

    assert_eq!(
        fact_kinds(&fixture.bus)[before.len()..],
        [wire::FactKind::LaneExited, wire::FactKind::LaneHeld],
        "the exit vacates the lane and the hold is journaled against it"
    );
    let held = fixture.held_fact();
    assert_eq!(held.lane, lane());
    assert_eq!(
        held.evidence,
        format!(
            "occupant {} ended on a vendor error: {REFUSAL}",
            fixture.occupant
        ),
        "the journal names the occupant and the vendor's own words"
    );
    assert_eq!(
        lifecycle.held_until(lane()),
        Some(held.until),
        "the runtime holds the lane on the journaled deadline"
    );
    assert_eq!(
        held.vendor_failure,
        Some(body::VendorFailureInput {
            session: Some("reaped-session".to_owned()),
            directives: Vec::new(),
            renewal: None,
        }),
        "the hold records the exact resumable session and pending operator input"
    );
    assert_eq!(
        residency_row(&fixture.bus).occupant,
        None,
        "the exit is still accounted"
    );
    let (_, lane_read) = crate::resident::lane_surface::current_answer(
        &fixture.bus,
        &fixture.paths,
        epoch(),
        &Seats::new(),
    )
    .expect("operator lane read projects the hold");
    assert_eq!(
        lane_read.lanes[0].hold,
        Some(wire::intent::LaneHold {
            evidence: held.evidence.clone(),
            until: held.until,
        }),
        "the operator lane read carries the journaled deadline and evidence"
    );
    assert_eq!(
        lane_read.lanes[0].parked,
        Some(body::LanePark::Hold),
        "a held lane reads as parked"
    );
    assert_eq!(
        recalled_hold(&fixture),
        lane_read.lanes[0].hold,
        "the recall residency row carries the same current hold"
    );
    let projection = fixture.projection();
    let snapshot = fixture.bus.task_snapshot().expect("vacant snapshot reads");
    let holds = replay_holds(snapshot.facts().iter());
    assert!(!holds.seats(lane()), "the replayed journal holds the lane");
    assert!(
        project_targets(
            &snapshot,
            &projection,
            instant(50),
            &holds,
            &board::NextConfig::default(),
            None,
            &owners(&snapshot),
        )
        .expect("lifecycle targets project")
        .is_empty(),
        "a held lane is no target for a re-seat"
    );
    assert_eq!(
        String::from_utf8(diagnostic).expect("diagnostic is UTF-8"),
        format!(
            "lifecycle held lane={} until={} evidence={}\n",
            lane(),
            held.until,
            held.evidence
        )
    );
    lifecycle
        .operator_wake(&fixture.bus, epoch(), lane())
        .expect("operator wake releases the hold");
    let (_, resumed_read) = crate::resident::lane_surface::current_answer(
        &fixture.bus,
        &fixture.paths,
        epoch(),
        &Seats::new(),
    )
    .expect("operator lane read projects the resumed lane");
    assert_eq!(
        resumed_read.lanes[0].hold, None,
        "the operator lane read drops a released hold"
    );
    assert_eq!(resumed_read.lanes[0].parked, None);
    assert_eq!(
        recalled_hold(&fixture),
        None,
        "the recall residency row drops a released hold"
    );
    fixture.finish();
}

/// The hold the operator recall reply projects onto the fixture lane's row.
fn recalled_hold(fixture: &VendorExitFixture) -> Option<wire::intent::LaneHold> {
    let request = wire::intent::LaneRecall { lane: Some(lane()) };
    let command = wire::Command {
        v: wire::WireVersion::CURRENT,
        id: "018f82d4-6f59-7d06-92e3-000000000008"
            .parse()
            .expect("fixture command identity"),
        epoch: epoch(),
        kind: wire::CommandKind::LaneRecall,
        expected: Vec::new(),
        body: serde_json::to_value(&request).expect("recall intent encodes"),
    }
    .admit(
        &wire::Accepted {
            wire: wire::WireVersion::CURRENT,
            actor: Actor::TEST_PRINCIPAL,
            epoch: epoch(),
            release: "1.2.3".parse().expect("fixture release"),
        },
        &wire::principal::PrincipalRoles::default(),
    )
    .expect("operator recall admits");
    let wire::Outcome::Reply(reply) = crate::resident::lane_surface::recall(
        &fixture.bus,
        &fixture.paths,
        epoch(),
        &Seats::new(),
        &command,
        &request,
    ) else {
        panic!("operator recall replies")
    };
    let wire::intent::CommandAnswer::Recall(recall) = reply.body else {
        panic!("recall answers with a continuity account")
    };
    recall
        .residency
        .lanes
        .into_iter()
        .find(|row| row.lane == lane())
        .expect("recall carries the fixture lane")
        .hold
}

#[test]
fn a_new_operator_direction_releases_the_vendor_hold() {
    let fixture = VendorExitFixture::new(
        "changed-direction",
        &[PROMPT, REFUSED_RESULT, TRAILING_WIDGET],
    );
    fixture.steer();
    let config = lane::Config::default();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    let mut diagnostic = Vec::new();
    fixture
        .exit(&mut lifecycle, &mut diagnostic)
        .expect("the refused exit is accounted");
    assert_eq!(
        fixture
            .held_fact()
            .vendor_failure
            .expect("vendor hold carries input")
            .directives,
        [uuid(11)]
    );
    let unchanged = fixture
        .bus
        .task_snapshot()
        .expect("unchanged snapshot reads");
    lifecycle
        .projection
        .advance(&unchanged)
        .expect("unchanged projection advances");
    assert!(
        !lifecycle
            .release_changed_vendor_hold(&fixture.bus, epoch(), &unchanged)
            .expect("unchanged input reconciles"),
        "the still-pending refused direction is unchanged input and keeps the hold"
    );
    assert!(lifecycle.held_until(lane()).is_some());

    committed(
        fixture
            .bus
            .commit(second_operator_direction_write())
            .expect("new operator direction commits"),
    );
    let snapshot = fixture.bus.task_snapshot().expect("changed snapshot reads");
    lifecycle
        .projection
        .advance(&snapshot)
        .expect("changed projection advances");
    assert!(
        lifecycle
            .release_changed_vendor_hold(&fixture.bus, epoch(), &snapshot)
            .expect("changed input reconciles"),
        "changed operator input commits a hold release"
    );
    assert_eq!(lifecycle.held_until(lane()), None);
    let resumed = fixture
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("released facts read")
        .facts()
        .iter()
        .rev()
        .find_map(|fact| fact.fact.read::<body::LaneResumed>())
        .expect("changed input journals a release");
    assert_eq!(resumed.release, body::Release::InputChanged);
    fixture.finish();
}

#[test]
fn cancelling_the_refused_operator_direction_releases_the_vendor_hold() {
    let fixture = VendorExitFixture::new(
        "cancelled-direction",
        &[PROMPT, REFUSED_RESULT, TRAILING_WIDGET],
    );
    fixture.steer();
    let config = lane::Config::default();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    let mut diagnostic = Vec::new();
    fixture
        .exit(&mut lifecycle, &mut diagnostic)
        .expect("the refused exit is accounted");

    committed(
        fixture
            .bus
            .commit(cancel_operator_direction_write(&fixture.bus))
            .expect("operator cancellation commits"),
    );
    let snapshot = fixture
        .bus
        .task_snapshot()
        .expect("cancelled snapshot reads");
    lifecycle
        .projection
        .advance(&snapshot)
        .expect("cancelled projection advances");
    assert!(
        lifecycle
            .release_changed_vendor_hold(&fixture.bus, epoch(), &snapshot)
            .expect("cancelled input reconciles"),
        "cancelling refused operator input commits a hold release"
    );
    assert_eq!(lifecycle.held_until(lane()), None);
    let resumed = fixture
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("released facts read")
        .facts()
        .iter()
        .rev()
        .find_map(|fact| fact.fact.read::<body::LaneResumed>())
        .expect("cancelled input journals a release");
    assert_eq!(resumed.release, body::Release::InputChanged);
    fixture.finish();
}

#[test]
fn a_settled_exit_followed_by_an_aside_is_mature_however_young_the_occupant() {
    let config = one_death_holds();
    let mut lifecycle = lifecycle::Runtime::new(&config);

    for tag in ["finished-first", "finished-second"] {
        let fixture = VendorExitFixture::new(
            tag,
            &[
                PROMPT,
                FINISHED_PROSE,
                FINISHED_RESULT,
                COMPLETED_ASIDE,
                TRAILING_WIDGET,
            ],
        );
        let before = fact_kinds(&fixture.bus);
        let mut diagnostic = Vec::new();

        fixture
            .exit(&mut lifecycle, &mut diagnostic)
            .expect("the finished exit is accounted");

        assert_eq!(
            fact_kinds(&fixture.bus)[before.len()..],
            [wire::FactKind::LaneExited],
            "a settled exit journals no hold even where one young death would"
        );
        assert_eq!(lifecycle.held_until(lane()), None);
        let snapshot = fixture.bus.task_snapshot().expect("vacant snapshot reads");
        assert!(
            replay_holds(snapshot.facts().iter()).seats(lane()),
            "the settled exit leaves the vacant lane eligible"
        );
        assert!(
            diagnostic.is_empty(),
            "{}",
            String::from_utf8_lossy(&diagnostic)
        );
        fixture.finish();
    }
}

#[test]
fn an_exit_that_never_answered_the_prompt_is_a_young_death() {
    let fixture = VendorExitFixture::new("silent", &[PROMPT, TRAILING_WIDGET]);
    let config = one_death_holds();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    let before = fact_kinds(&fixture.bus);
    let mut diagnostic = Vec::new();

    fixture
        .exit(&mut lifecycle, &mut diagnostic)
        .expect("the silent exit is accounted");

    assert_eq!(
        fact_kinds(&fixture.bus)[before.len()..],
        [wire::FactKind::LaneExited, wire::FactKind::LaneHeld],
        "an exit that answered nothing inside the young window is one young death"
    );
    let held = fixture.held_fact();
    assert_eq!(held.evidence, "1 consecutive young occupant deaths");
    assert_eq!(
        String::from_utf8(diagnostic).expect("diagnostic is UTF-8"),
        format!(
            "lifecycle held lane={} until={} evidence={}\n",
            lane(),
            held.until,
            held.evidence
        ),
        "the young-death hold is written down where the vendor-failure hold is"
    );
    fixture.finish();
}

#[test]
fn an_exit_that_answered_but_left_its_direction_live_is_a_young_death() {
    let fixture = VendorExitFixture::new(
        "unsettled",
        &[PROMPT, FINISHED_PROSE, FINISHED_RESULT, TRAILING_WIDGET],
    );
    fixture.steer();
    let config = one_death_holds();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    let before = fact_kinds(&fixture.bus);
    let mut diagnostic = Vec::new();

    fixture
        .exit(&mut lifecycle, &mut diagnostic)
        .expect("the unsettled exit is accounted");

    assert_eq!(
        fact_kinds(&fixture.bus)[before.len()..],
        [wire::FactKind::LaneExited, wire::FactKind::LaneHeld],
        "an answer that retires nothing hands the same wake to a successor, so a quick exit on it is one young death"
    );
    let held = fixture.held_fact();
    assert_eq!(held.evidence, "1 consecutive young occupant deaths");
    assert_eq!(
        String::from_utf8(diagnostic).expect("diagnostic is UTF-8"),
        format!(
            "lane {lane} steering 1kKRRsDSx left pending at exit: \
             steer::transcript-evidence-absent: the newest assistant text \
             (member 1 offset {offset}) does not carry the header: \"Finished the claim.\"\n\
             lifecycle held lane={lane} until={} evidence={}\n",
            held.until,
            held.evidence,
            lane = lane(),
            offset = DECLARATION.len() + PROMPT.len(),
        ),
        "the direction left pending and the hold it earns are written down together"
    );
    fixture.finish();
}

#[test]
fn an_exit_that_retired_its_direction_in_its_final_prose_is_a_mature_death() {
    let fixture = VendorExitFixture::new(
        "settled",
        &[PROMPT, ACKED_PROSE, ACKED_RESULT, TRAILING_WIDGET],
    );
    fixture.steer();
    let config = one_death_holds();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    let before = fact_kinds(&fixture.bus);
    let mut diagnostic = Vec::new();

    fixture
        .exit(&mut lifecycle, &mut diagnostic)
        .expect("the settled exit is accounted");

    assert_eq!(
        fact_kinds(&fixture.bus)[before.len()..],
        [wire::FactKind::LaneExited, wire::FactKind::SteerAcked],
        "the answer in the final prose retires the direction before the death is judged, so nothing holds"
    );
    assert_eq!(lifecycle.held_until(lane()), None);
    assert_eq!(
        String::from_utf8(diagnostic).expect("diagnostic is UTF-8"),
        format!(
            "lane {} steering 1kKRRsDSx acked at exit from its transcript\n",
            lane()
        )
    );
    fixture.finish();
}

/// Lane policy under which a single young death holds the lane, so one exit
/// shows whether it was counted young.
fn one_death_holds() -> lane::Config {
    serde_json::from_value(serde_json::json!({"deaths": 1}))
        .expect("a one-death lane policy admits")
}

#[test]
fn a_vendor_failure_holds_for_the_configured_hold_from_the_injected_clock() {
    let occupant = uuid(3);
    let mut journal = Journal::in_memory().expect("held fixture journal opens");
    journal
        .apply(occupied_seed(occupant))
        .expect("held fixture seeds");
    let bus = Bus::start(journal).expect("held fixture bus starts");
    let config = lane::Config::default();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    let before = fact_kinds(&bus);
    let mut diagnostic = Vec::new();

    lifecycle
        .observe_vendor_failure_at(
            &bus,
            epoch(),
            lane::capture::CaptureExit {
                lane: lane(),
                occupant,
            },
            &format!("  {REFUSAL}\n"),
            instant(10),
            &mut diagnostic,
        )
        .expect("the failure holds the lane");

    let until = Instant::new(
        instant(10)
            .get()
            .checked_add(jiff::SignedDuration::from_secs(30 * 60))
            .expect("the hold deadline is representable"),
    )
    .expect("the hold deadline is an instant");
    assert_eq!(fact_kinds(&bus)[before.len()..], [wire::FactKind::LaneHeld]);
    let held = held_fact(&bus);
    assert_eq!(held.lane, lane());
    assert_eq!(
        held.until, until,
        "the hold runs the configured 30m from now"
    );
    assert_eq!(
        held.evidence,
        format!("occupant {occupant} ended on a vendor error: {REFUSAL}"),
        "the vendor text is trimmed and kept whole"
    );
    assert_eq!(lifecycle.held_until(lane()), Some(until));
    assert_eq!(
        String::from_utf8(diagnostic).expect("diagnostic is UTF-8"),
        format!(
            "lifecycle held lane={} until={until} evidence={}\n",
            lane(),
            held.evidence
        )
    );

    let mut again = Vec::new();
    lifecycle
        .observe_vendor_failure_at(
            &bus,
            epoch(),
            lane::capture::CaptureExit {
                lane: lane(),
                occupant,
            },
            "a second failure while held",
            instant(20),
            &mut again,
        )
        .expect("a failure while held is absorbed");
    assert_eq!(
        fact_kinds(&bus)[before.len()..],
        [wire::FactKind::LaneHeld],
        "a lane already held takes no second hold"
    );
    assert_eq!(lifecycle.held_until(lane()), Some(until));
    assert!(again.is_empty(), "{}", String::from_utf8_lossy(&again));
    bus.verify().expect("the held authority verifies");
    bus.shutdown().expect("held fixture stops");
}

#[test]
fn verbose_vendor_text_is_cut_at_a_character_boundary_in_the_evidence() {
    let occupant = uuid(3);
    let mut journal = Journal::in_memory().expect("verbose fixture journal opens");
    journal
        .apply(occupied_seed(occupant))
        .expect("verbose fixture seeds");
    let bus = Bus::start(journal).expect("verbose fixture bus starts");
    let config = lane::Config::default();
    let mut lifecycle = lifecycle::Runtime::new(&config);
    let verbose = "é".repeat(600);
    let mut diagnostic = Vec::new();

    lifecycle
        .observe_vendor_failure_at(
            &bus,
            epoch(),
            lane::capture::CaptureExit {
                lane: lane(),
                occupant,
            },
            &verbose,
            instant(10),
            &mut diagnostic,
        )
        .expect("the verbose failure holds the lane");

    let held = held_fact(&bus);
    let prefix = format!("occupant {occupant} ended on a vendor error: ");
    let kept = held
        .evidence
        .strip_prefix(&prefix)
        .expect("the evidence opens on the occupant");
    let kept = kept
        .strip_suffix('…')
        .expect("cut evidence ends on the mark");
    assert_eq!(kept.len(), 512, "the cut lands on the byte budget");
    assert_eq!(
        kept.chars().count(),
        256,
        "the cut lands on a character boundary"
    );
    assert!(kept.chars().all(|character| character == 'é'));
    bus.shutdown().expect("verbose fixture stops");
}

fn cancel_operator_direction_write(bus: &Bus) -> JournalWrite {
    let observed = bus
        .read(|read| read.steering_head(uuid(11)))
        .expect("operator direction head reads")
        .expect("operator direction exists");
    let at = instant(12);
    let origin = format!("resident:{}", epoch())
        .parse::<Origin>()
        .expect("fixture resident origin");
    let mut write = JournalWrite::new(Actor::TEST_PRINCIPAL, origin, at, Producer::CommandDoor);
    let canceled = write
        .fact(
            body::SteerCanceled {
                key: observed.body().key.clone(),
                recipient: observed.body().recipient.clone(),
                reason: None,
            },
            FactCause::Existing(observed.position()),
        )
        .expect("operator cancellation fact builds");
    let mut next = observed.body().clone();
    next.disposition = wire::Disposition::Canceled;
    write.steering_head(canceled, move |_| next);
    write
}

fn second_operator_direction_write() -> JournalWrite {
    let at = instant(12);
    let origin = format!("resident:{}", epoch())
        .parse::<Origin>()
        .expect("fixture resident origin");
    let direction = body::SteerSent {
        sender: Actor::TEST_PRINCIPAL,
        room: None,
        directive: uuid(21),
        fanout: Some(uuid(22)),
        recipients: Vec::new(),
        key: "1kKRRsDTx".to_owned(),
        recipient: Actor::Lane(lane()),
        text: "New input after the refusal.".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: instant(59),
    };
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let sent = write
        .fact(direction.clone(), FactCause::Root)
        .expect("second operator direction fact builds");
    write.steering_head(sent, move |position| {
        Ledger::sent_head(&direction, origin, at, position)
            .expect("second operator direction head builds")
    });
    write
}

fn held_fact(bus: &Bus) -> body::LaneHeld {
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("facts read")
    .facts()
    .iter()
    .rev()
    .find_map(|fact| fact.fact.read::<body::LaneHeld>())
    .expect("the hold fact is journaled")
}

/// A seated occupant whose declared session names the transcript it wrote.
struct VendorExitFixture {
    root: PathBuf,
    paths: RuntimePaths,
    bus: Bus,
    occupant: CanonicalUuid,
}

impl VendorExitFixture {
    fn new(tag: &str, records: &[&[u8]]) -> Self {
        let root = fixture_root(&format!("vendor-failure-{tag}"));
        let root = fs::canonicalize(root).expect("fixture root canonicalizes");
        let paths = runtime_paths(&root);
        let occupant = uuid(3);
        let tape =
            tape::Tape::open(paths.lane_tape(lane()).join("omp")).expect("the lane tape opens");
        let declared = tape
            .tee(DECLARATION)
            .expect("the session declaration is captured");
        for record in records {
            tape.tee(record).expect("the transcript record is captured");
        }
        let mut session = session(&root, occupant);
        session.locus = wire::TapeLocus {
            member: declared.member,
            offset: declared.offset,
            length: declared.length,
        };
        let mut journal = Journal::in_memory().expect("vendor exit fixture journal opens");
        journal
            .apply(occupied_seed_with_session(
                occupant,
                &root.to_string_lossy(),
                Some(session.clone()),
            ))
            .expect("vendor exit fixture seeds");
        let bus = Bus::start(journal).expect("vendor exit fixture bus starts");
        record_exited_for_tests(&paths, lane(), occupant, session)
            .expect("the supervisor exit record writes");
        Self {
            root,
            paths,
            bus,
            occupant,
        }
    }

    fn exit(
        &self,
        lifecycle: &mut lifecycle::Runtime,
        diagnostic: &mut Vec<u8>,
    ) -> CommandResult<()> {
        let row = residency_row(&self.bus);
        let Some(Adoption::Exited(launch)) =
            adopt(&self.paths, &row).expect("published state adopts")
        else {
            panic!("the published exit was not adoptable");
        };
        let mut seats = Seats::new();
        seats
            .seat_detached(lane(), self.occupant, launch.process)
            .expect("published occupant seats");
        let outcome = account_exit(
            &self.bus,
            &self.paths,
            (
                &steer::Config::default(),
                epoch(),
                crate::board_command::JoiningFixture::discover(&self.root).joining(),
            ),
            &mut seats,
            lifecycle,
            CaptureExit {
                lane: lane(),
                occupant: self.occupant,
            },
            diagnostic,
        );
        assert!(!seats.occupied(lane()));
        outcome
    }

    /// Address the operator's direction to the lane while the occupant sits.
    fn steer(&self) {
        committed(
            self.bus
                .commit(operator_direction_write())
                .expect("operator direction commits"),
        );
    }

    fn held_fact(&self) -> body::LaneHeld {
        held_fact(&self.bus)
    }

    fn projection(&self) -> Projection {
        let mut projection = Projection::new();
        projection
            .advance(&self.bus.task_snapshot().expect("snapshot reads"))
            .expect("projection advances");
        projection
    }

    fn finish(self) {
        self.bus.verify().expect("the accounted authority verifies");
        self.bus.shutdown().expect("vendor exit fixture stops");
        fs::remove_dir_all(self.root).expect("fixture cleanup");
    }
}
