use super::*;

use crate::resident::lane_surface::{LaunchFixture, seed_task_claim};
use bus::CommitOutcome;
use journal::{Journal, JournalWrite};
use wire::{ClaimDisposition, LaneResidency, Origin, Producer, ResidentState, TaskHandle};

/// One lifecycle reconcile of review feedback with the default lifetime.
fn feedback_pass(
    bus: &Bus,
    fixture: &LaunchFixture,
    cursor: Option<Seq>,
    held: &mut BTreeSet<Seq>,
) -> bool {
    let snapshot = bus.task_snapshot().expect("feedback snapshot reads");
    reconcile(
        bus,
        &fixture.paths,
        fixture.epoch,
        &snapshot,
        FindingWindow {
            cursor,
            held,
            now: now(),
            lifetime: Duration::from_secs(3_600),
        },
    )
    .expect("review feedback reconciles")
}

fn pending_feedback(bus: &Bus, lane: CanonicalUuid) -> usize {
    steer::Ledger::from_snapshot(
        &bus.transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("feedback facts read"),
    )
    .expect("feedback steering replays")
    .pending(lane)
    .len()
}

fn finding_seq(bus: &Bus) -> Seq {
    bus.task_snapshot()
        .expect("finding snapshot reads")
        .facts()
        .iter()
        .rev()
        .find(|envelope| envelope.fact.kind() == wire::FactKind::TaskReviewed)
        .map(|envelope| envelope.seq)
        .expect("the finding review")
}

#[test]
fn finding_feedback_reaches_the_live_author_once_across_restart() {
    let fixture = LaunchFixture::new();
    let authority = fixture.root.join("authority.sqlite3");
    let (bus, task) = reviewed_fixture(&fixture, &authority, true, wire::body::Verdict::Finding);

    assert!(feedback_pass(&bus, &fixture, None, &mut BTreeSet::new()));
    let facts = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("feedback facts read");
    let ledger = steer::Ledger::from_snapshot(&facts).expect("feedback steering replays");
    let pending = ledger.pending(fixture.lane);
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].text(),
        feedback_text(
            &TaskHandle::for_topic(
                &"/task/claim".parse::<wire::Topic>().expect("fixture topic"),
                &task
            ),
            "missing current coverage\n\nexercise the live refusal",
        ),
    );
    let snapshot = bus.task_snapshot().expect("feedback snapshot reads");
    assert_eq!(
        crate::resident::message_surface::directive_context::review_presentation(
            Some(&snapshot),
            pending[0].control(),
        )
        .expect("review presentation resolves")
        .expect("finding has review presentation"),
        wire::MessageReview {
            task: TaskHandle::for_topic(
                &"/task/claim".parse::<wire::Topic>().expect("fixture topic"),
                &task
            ),
            finding: "missing current coverage\n\nexercise the live refusal".to_owned(),
            guidance: wire::REVIEW_FEEDBACK_GUIDANCE.to_owned(),
        },
    );

    bus.shutdown().expect("feedback bus stops");
    let reopened = Bus::start(Journal::open(&authority).expect("feedback journal reopens"))
        .expect("feedback bus restarts");
    assert!(
        !feedback_pass(&reopened, &fixture, None, &mut BTreeSet::new()),
        "replayed review feedback stays deduplicated"
    );
    assert_eq!(pending_feedback(&reopened, fixture.lane), 1);
    reopened.shutdown().expect("restarted feedback bus stops");
}

#[test]
fn a_shared_finding_reaches_each_author_once_including_prior_delivery() {
    use wire::body::{RoundParticipant, SourceBoundary, TaskCompletionStatus, TaskRoundCompleted};

    for prior_delivery in [true, false] {
        let fixture = LaunchFixture::new();
        let authority = fixture.root.join("shared-authority.sqlite3");
        let bus = Bus::start(Journal::open(&authority).expect("shared journal opens"))
            .expect("shared bus starts");
        let (partner, reviewer) = (uuid(20), uuid(30));
        seat(
            &bus,
            &fixture,
            vec![
                author_row(&fixture, true),
                reviewer_row(&fixture, partner, uuid(21)),
                reviewer_row(&fixture, reviewer, uuid(31)),
            ],
        );
        let task = claimed_task(&bus, &fixture);
        let participant = || {
            let snapshot = bus.task_snapshot().expect("participant snapshot reads");
            let claim = snapshot.tasks()[0].latest_claim().unwrap().claim().body();
            RoundParticipant {
                claimed: claim.claimed,
                actor: claim.actor.clone(),
                lane: claim.lane,
                occupant: claim.occupant,
                validation: "author validation".to_owned(),
                git: None,
                source: SourceBoundary::Proven { heads: Vec::new() },
            }
        };
        let first = participant();
        claim_review(&bus, fixture.epoch, &task, partner, uuid(21));
        let second = participant();
        let mut write = JournalWrite::new(
            second.actor.clone(),
            format!("task:{task}").parse().unwrap(),
            now(),
            Producer::CommandDoor,
        );
        write
            .fact(
                TaskRoundCompleted {
                    task: task.clone(),
                    phase: "todo".parse().unwrap(),
                    round: first.claimed,
                    participants: vec![first, second],
                    status: TaskCompletionStatus::Pending,
                    next: None,
                    disposition: None,
                },
                FactCause::Root,
            )
            .expect("shared barrier builds");
        committed(bus.commit(write).expect("shared barrier commits"));
        claim_review(&bus, fixture.epoch, &task, reviewer, uuid(31));
        complete_review(&bus, &task, reviewer, wire::body::Verdict::Finding);

        if prior_delivery {
            use super::super::super::steering;

            let control = wire::review_feedback_control(&task, finding_seq(&bus));
            let mut write = steering::internal_write(fixture.epoch).unwrap();
            steering::append_internal_directive(
                &bus,
                &mut write,
                steering::InternalDirective {
                    directive: steering::internal_directive_identity(&format!(
                        "lane:{}:{control}",
                        fixture.lane
                    )),
                    lane: fixture.lane,
                    text: "previously delivered finding".to_owned(),
                    control,
                    lifetime: Duration::from_secs(3_600),
                    cause: FactCause::Root,
                },
            )
            .expect("prior review feedback builds");
            committed(bus.commit(write).expect("prior review feedback commits"));
        }

        assert!(feedback_pass(&bus, &fixture, None, &mut BTreeSet::new()));
        assert!(!feedback_pass(&bus, &fixture, None, &mut BTreeSet::new()));
        assert_eq!(pending_feedback(&bus, fixture.lane), 1);
        assert_eq!(pending_feedback(&bus, partner), 1);
        assert_eq!(pending_feedback(&bus, reviewer), 0);
        bus.shutdown().expect("shared bus stops");
    }
}

#[test]
fn a_finding_held_between_seats_reaches_the_resumed_session_once() {
    let fixture = LaunchFixture::new();
    let authority = fixture.root.join("held-authority.sqlite3");
    declare(&fixture, &authority, fixture.occupant, "author-session");
    let (bus, _) = reviewed_fixture(&fixture, &authority, false, wire::body::Verdict::Finding);
    let finding = finding_seq(&bus);
    let mut held = BTreeSet::new();

    assert!(!feedback_pass(&bus, &fixture, None, &mut held));
    assert_eq!(held, BTreeSet::from([finding]));
    assert_eq!(pending_feedback(&bus, fixture.lane), 0);

    // The projection cursor has passed the finding; the same vendor session
    // resumes under a new occupant.
    let resumed = uuid(40);
    declare_on(&bus, &fixture, resumed, "author-session");
    reseat(&bus, &fixture, resumed, "author-session");
    assert!(feedback_pass(&bus, &fixture, Some(finding), &mut held));
    assert!(held.is_empty());
    assert_eq!(pending_feedback(&bus, fixture.lane), 1);

    assert!(!feedback_pass(&bus, &fixture, None, &mut held));
    assert_eq!(pending_feedback(&bus, fixture.lane), 1);
    bus.shutdown().expect("held feedback bus stops");
}

#[test]
fn a_renewed_session_on_the_author_lane_drops_the_finding() {
    let fixture = LaunchFixture::new();
    let authority = fixture.root.join("renewed-authority.sqlite3");
    declare(&fixture, &authority, fixture.occupant, "author-session");
    let (bus, _) = reviewed_fixture(&fixture, &authority, false, wire::body::Verdict::Finding);
    let finding = finding_seq(&bus);
    let mut held = BTreeSet::new();
    assert!(!feedback_pass(&bus, &fixture, None, &mut held));

    let renewed = uuid(41);
    declare_on(&bus, &fixture, renewed, "renewed-session");
    reseat(&bus, &fixture, renewed, "renewed-session");
    assert!(!feedback_pass(&bus, &fixture, Some(finding), &mut held));
    assert!(held.is_empty());
    assert_eq!(pending_feedback(&bus, fixture.lane), 0);
    bus.shutdown().expect("renewed feedback bus stops");
}

#[test]
fn a_held_finding_expires_with_its_directive_lifetime() {
    let fixture = LaunchFixture::new();
    let authority = fixture.root.join("expired-authority.sqlite3");
    let (bus, _) = reviewed_fixture(&fixture, &authority, false, wire::body::Verdict::Finding);
    let finding = finding_seq(&bus);
    let mut held = BTreeSet::from([finding]);
    reseat(
        &bus,
        &fixture,
        fixture.occupant,
        &fixture.occupant.to_string(),
    );
    let snapshot = bus.task_snapshot().expect("expired snapshot reads");
    assert!(
        !reconcile(
            &bus,
            &fixture.paths,
            fixture.epoch,
            &snapshot,
            FindingWindow {
                cursor: Some(finding),
                held: &mut held,
                now: shifted(now(), 3_601),
                lifetime: Duration::from_secs(3_600),
            },
        )
        .expect("expired feedback reconciles")
    );
    assert!(held.is_empty());
    assert_eq!(pending_feedback(&bus, fixture.lane), 0);
    bus.shutdown().expect("expired feedback bus stops");
}

#[test]
fn a_finding_reviewed_from_the_author_session_is_not_delivered() {
    let fixture = LaunchFixture::new();
    let authority = fixture.root.join("self-authority.sqlite3");
    let bus = Bus::start(Journal::open(&authority).expect("self journal opens"))
        .expect("self bus starts");
    let resumed = uuid(42);
    declare_on(&bus, &fixture, fixture.occupant, "author-session");
    declare_on(&bus, &fixture, resumed, "author-session");
    seat_reviewers(&bus, &fixture, uuid(20), uuid(21), true);
    let task = claimed_task(&bus, &fixture);
    complete_author_phase(&bus, &task);
    claim_review(&bus, fixture.epoch, &task, fixture.lane, resumed);
    complete_review(&bus, &task, fixture.lane, wire::body::Verdict::Finding);
    let mut held = BTreeSet::new();
    assert!(!feedback_pass(&bus, &fixture, None, &mut held));
    assert!(held.is_empty());
    assert_eq!(pending_feedback(&bus, fixture.lane), 0);
    bus.shutdown().expect("self feedback bus stops");
}

#[test]
fn a_clean_review_creates_no_feedback() {
    let fixture = LaunchFixture::new();
    let authority = fixture.root.join("clean-authority.sqlite3");
    let (bus, _) = reviewed_fixture(&fixture, &authority, true, wire::body::Verdict::Clean);
    assert!(!feedback_pass(&bus, &fixture, None, &mut BTreeSet::new()));
    assert!(
        steer::Ledger::from_snapshot(
            &bus.transact(|journal| bus::JournalTransaction::settled(
                journal.fact_snapshot().expect("journal snapshot")
            ))
            .expect("clean facts read")
        )
        .expect("clean steering replays")
        .directives()
        .is_empty(),
    );
    bus.shutdown().expect("clean feedback bus stops");
}

#[test]
fn every_finding_is_judged_by_the_authors_of_the_phase_it_reviewed() {
    let fixture = LaunchFixture::new();
    let authority = fixture.root.join("twice-authority.sqlite3");
    let (bus, task) = reviewed_fixture(&fixture, &authority, true, wire::body::Verdict::Finding);
    // The author takes the task back, completes it again, and is reviewed again.
    claim_review(&bus, fixture.epoch, &task, fixture.lane, fixture.occupant);
    complete_author_phase(&bus, &task);
    claim_review(&bus, fixture.epoch, &task, uuid(20), uuid(21));
    complete_review(&bus, &task, uuid(20), wire::body::Verdict::Finding);
    let snapshot = bus.task_snapshot().expect("twice-reviewed snapshot reads");
    let first = snapshot
        .facts()
        .iter()
        .find(|envelope| envelope.fact.kind() == wire::FactKind::TaskReviewed)
        .map(|envelope| envelope.seq)
        .expect("the first review");

    // Only the finding past the cursor is judged.
    assert!(feedback_pass(
        &bus,
        &fixture,
        Some(first),
        &mut BTreeSet::new()
    ));
    assert_eq!(pending_feedback(&bus, fixture.lane), 1);

    // From the start, the earlier finding is judged by its own phase's authors.
    assert!(feedback_pass(&bus, &fixture, None, &mut BTreeSet::new()));
    assert_eq!(pending_feedback(&bus, fixture.lane), 2);
    bus.shutdown().expect("twice-reviewed bus stops");
}

fn reviewed_fixture(
    fixture: &LaunchFixture,
    authority: &std::path::Path,
    author_live: bool,
    verdict: wire::body::Verdict,
) -> (Bus, TaskStamp) {
    let bus = Bus::start(Journal::open(authority).expect("feedback journal opens"))
        .expect("feedback bus starts");
    let reviewer_lane = uuid(20);
    let reviewer = uuid(21);
    seat_reviewers(&bus, fixture, reviewer_lane, reviewer, author_live);
    let task = claimed_task(&bus, fixture);
    complete_author_phase(&bus, &task);
    claim_review(&bus, fixture.epoch, &task, reviewer_lane, reviewer);
    complete_review(&bus, &task, reviewer_lane, verdict);
    (bus, task)
}

/// The fixture task, claimed by the author lane's fixture occupant.
fn claimed_task(bus: &Bus, fixture: &LaunchFixture) -> TaskStamp {
    let task: TaskStamp = "1kKRRsDRx".parse().expect("feedback task stamp");
    let now = now();
    seed_task_claim(
        bus,
        &task,
        &author_row(fixture, true),
        fixture.epoch,
        shifted(now, -60),
        shifted(now, 3_600),
        shifted(now, 7_200),
    );
    task
}

/// Declare `identity` as the author lane's vendor session for `occupant` in a
/// fresh authority, before the reviewed fixture opens it.
fn declare(
    fixture: &LaunchFixture,
    authority: &std::path::Path,
    occupant: CanonicalUuid,
    identity: &str,
) {
    let bus = Bus::start(Journal::open(authority).expect("declaration journal opens"))
        .expect("declaration bus starts");
    declare_on(&bus, fixture, occupant, identity);
    bus.shutdown().expect("declaration bus stops");
}

fn declare_on(bus: &Bus, fixture: &LaunchFixture, occupant: CanonicalUuid, identity: &str) {
    let mut write = JournalWrite::new(
        Actor::Spice,
        resident_origin(fixture),
        now(),
        Producer::ResidentRecognition,
    );
    write
        .fact(
            wire::body::LaneSessionDeclared {
                lane: fixture.lane,
                occupant,
                worktree: fixture.root.display().to_string(),
                vendor: fixture.vendor.clone(),
                launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
                epoch: fixture.epoch,
                stream: "session".to_owned(),
                source: uuid(99),
                identity: identity.to_owned(),
                member: 0,
                offset: 0,
                length: 1,
            },
            FactCause::Root,
        )
        .expect("session declaration builds");
    committed(bus.commit(write).expect("session declaration commits"));
}

/// Seat `occupant` on the author lane in vendor session `identity`.
fn reseat(bus: &Bus, fixture: &LaunchFixture, occupant: CanonicalUuid, identity: &str) {
    let mut author = author_row(fixture, true);
    author.occupant = Some(occupant);
    author.lineage.push(occupant);
    let mut seated = session(fixture, fixture.lane, occupant);
    seated.identity = identity.to_owned();
    author.session = Some(seated);
    let reviewer = reviewer_row(fixture, uuid(20), uuid(21));
    seat(bus, fixture, vec![author, reviewer]);
}

fn resident_origin(fixture: &LaunchFixture) -> Origin {
    format!("resident:{}", fixture.epoch)
        .parse()
        .expect("resident origin")
}

fn seat_reviewers(
    bus: &Bus,
    fixture: &LaunchFixture,
    reviewer_lane: CanonicalUuid,
    reviewer: CanonicalUuid,
    author_live: bool,
) {
    let rows = vec![
        author_row(fixture, author_live),
        reviewer_row(fixture, reviewer_lane, reviewer),
    ];
    seat(bus, fixture, rows);
}

fn reviewer_row(
    fixture: &LaunchFixture,
    reviewer_lane: CanonicalUuid,
    reviewer: CanonicalUuid,
) -> LaneResidency {
    LaneResidency {
        lane: reviewer_lane,
        team: reviewer_lane,
        name: Some("reviewer".to_owned()),
        worktree: fixture.root.display().to_string(),
        life: "drive".to_owned(),
        vendor: fixture.vendor.clone(),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: Some(reviewer),
        lineage: vec![reviewer],
        capabilities: vec![uuid(22)],
        session: Some(session(fixture, reviewer_lane, reviewer)),
        disposition: "occupied".to_owned(),
    }
}

fn seat(bus: &Bus, fixture: &LaunchFixture, rows: Vec<LaneResidency>) {
    let mut write = JournalWrite::new(
        Actor::Spice,
        resident_origin(fixture),
        now(),
        Producer::CommandDoor,
    );
    let seated = write
        .fact(
            wire::body::UpSeated {
                epoch: fixture.epoch,
            },
            FactCause::Root,
        )
        .expect("resident seating fact builds");
    let epoch = fixture.epoch;
    write.residency_head(seated, move |_| wire::ResidencyHead {
        epoch,
        state: ResidentState::Seated,
        topology_revision: 1,
        lanes: rows,
        subscriptions: Vec::new(),
        renewals: Vec::new(),
    });
    committed(bus.commit(write).expect("resident seating commits"));
}

fn author_row(fixture: &LaunchFixture, live: bool) -> LaneResidency {
    LaneResidency {
        lane: fixture.lane,
        team: fixture.lane,
        name: Some("author".to_owned()),
        worktree: fixture.root.display().to_string(),
        life: "drive".to_owned(),
        vendor: fixture.vendor.clone(),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: live.then_some(fixture.occupant),
        lineage: vec![fixture.occupant],
        capabilities: vec![fixture.capability],
        session: live.then(|| session(fixture, fixture.lane, fixture.occupant)),
        disposition: if live { "occupied" } else { "vacant" }.to_owned(),
    }
}

fn session(
    fixture: &LaunchFixture,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> wire::LaneSession {
    wire::LaneSession {
        lane,
        occupant,
        worktree: fixture.root.display().to_string(),
        vendor: fixture.vendor.clone(),
        launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
        epoch: fixture.epoch,
        stream: "session".to_owned(),
        identity: occupant.to_string(),
        locus: wire::TapeLocus {
            member: 0,
            offset: 0,
            length: 1,
        },
    }
}

fn complete_author_phase(bus: &Bus, task: &TaskStamp) {
    let snapshot = bus.task_snapshot().expect("author snapshot reads");
    let heads = &snapshot.tasks()[0];
    let claim = heads
        .latest_claim()
        .map(|held| held.claim())
        .expect("author claim")
        .body()
        .clone();
    let mut write = JournalWrite::new(
        claim.actor.clone(),
        format!("task:{task}").parse().expect("task origin"),
        now(),
        Producer::CommandDoor,
    );
    let done = write
        .fact(
            wire::body::TaskDone {
                task: task.clone(),
                validation: "author validation".to_owned(),
                disposition: None,
                terminal: None,
                git: None,
                source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Root,
        )
        .expect("task completion builds");
    let mut workflow = heads.workflow().body().clone();
    workflow.flow = vec!["todo".parse().unwrap(), "review".parse().unwrap()];
    workflow.phase = "review".parse().expect("review phase");
    workflow.status = wire::WorkflowStatus::pending();
    write.workflow_head(task.clone(), done, move |_| workflow);
    write.claim_head(task.clone(), done, move |fact| wire::ClaimHead {
        fact,
        disposition: ClaimDisposition::Freed,
        ..claim
    });
    committed(bus.commit(write).expect("author completion commits"));
}

fn claim_review(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &TaskStamp,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) {
    let actor = Actor::Lane(lane);
    let at = now();
    let expiry = shifted(at, 3_600);
    let mut write = JournalWrite::new(
        actor.clone(),
        format!("task:{task}").parse().expect("task origin"),
        at,
        Producer::CommandDoor,
    );
    let won = write
        .fact(
            wire::body::ClaimWon {
                task: task.clone(),
                actor: actor.clone(),
                lane,
                occupant,
                epoch,
                cell: wire::ClaimCell {
                    topic: Some("/canvas/message".parse().expect("fixture topic")),
                    phase: "review".parse().expect("review phase"),
                },
                until: expiry,
                git: None,
                recovery: None,
                source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Root,
        )
        .expect("review claim builds");
    let phase = "review".parse().expect("review phase");
    write.claim_head(task.clone(), won, move |fact| wire::ClaimHead {
        actor,
        fact,
        claimed: fact,
        cell: wire::ClaimCell {
            topic: Some("/canvas/message".parse().expect("fixture topic")),
            phase,
        },
        lane,
        occupant,
        epoch,
        recovery: None,
        disposition: ClaimDisposition::Active,
        acknowledged: 0,
    });
    write.lease_head(task.clone(), won, move |claimed| wire::LeaseHead {
        claimed,
        start: at,
        renewal: at,
        expiry,
        maximum: expiry,
        epoch,
    });
    committed(bus.commit(write).expect("review claim commits"));
}

fn complete_review(bus: &Bus, task: &TaskStamp, lane: CanonicalUuid, verdict: wire::body::Verdict) {
    let snapshot = bus.task_snapshot().expect("review snapshot reads");
    let heads = &snapshot.tasks()[0];
    let claim = heads
        .latest_claim()
        .map(|held| held.claim())
        .expect("review claim")
        .body()
        .clone();
    let actor = Actor::Lane(lane);
    let at = now();
    let mut write = JournalWrite::new(
        actor.clone(),
        format!("task:{task}").parse().expect("task origin"),
        at,
        Producer::CommandDoor,
    );
    let reviewed = write
        .fact(
            wire::body::TaskReviewed {
                task: task.clone(),
                reviewer: actor,
                verdict,
                status: match verdict {
                    wire::body::Verdict::Clean => wire::body::TaskCompletionStatus::Completed,
                    wire::body::Verdict::Finding => wire::body::TaskCompletionStatus::Pending,
                },
                finding: (verdict == wire::body::Verdict::Finding)
                    .then(|| "missing current coverage\n\nexercise the live refusal".to_owned()),
                note: None,
                followups: Vec::new(),
                round: None,
                git: None,
                source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Root,
        )
        .expect("review completion builds");
    let mut workflow = heads.workflow().body().clone();
    workflow.status = match verdict {
        wire::body::Verdict::Clean => wire::WorkflowStatus::reviewed(at),
        wire::body::Verdict::Finding => wire::WorkflowStatus::pending(),
    };
    write.workflow_head(task.clone(), reviewed, move |_| workflow);
    write.claim_head(task.clone(), reviewed, move |fact| wire::ClaimHead {
        fact,
        disposition: ClaimDisposition::Freed,
        ..claim
    });
    committed(bus.commit(write).expect("review completion commits"));
}

fn uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .expect("fixture UUID")
}

fn now() -> wire::Instant {
    wire::Instant::new(jiff::Timestamp::now()).expect("fixture instant")
}

fn shifted(at: wire::Instant, seconds: i64) -> wire::Instant {
    wire::Instant::new(
        at.get()
            .checked_add(jiff::SignedDuration::from_secs(seconds))
            .expect("fixture instant shifts"),
    )
    .expect("shifted fixture instant")
}

fn committed(outcome: CommitOutcome) {
    assert!(matches!(outcome, CommitOutcome::Committed(_)));
}
