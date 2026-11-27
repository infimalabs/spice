use std::collections::BTreeMap;

use board::Entry;
use bus::{Bus, CommitOutcome};
use journal::{FactCause, Journal, JournalWrite, PendingFact};
use lane::hold::Holds;
use steer::Ledger;
use wire::{
    Actor, CanonicalUuid, Instant, LaneResidency, Origin, Phase, Producer, ResidencyHead,
    ResidentState, TaskHandle, TaskHead, TaskStamp, WorkflowHead, body,
};

use super::materialization::Projection;
use super::{
    Available, Backoff, Resolution, Target, Trigger, await_armed_lines, project_targets, resolve,
};

mod deferred_exit;
mod exit_disposal;
mod line_parks;
mod matched_selection;
mod reaped_seat;
mod released_last_resort;
mod selection_wait;
mod trust_ice;
mod vendor_failure;

const TOPIC: &str = "/lifecycle/work";
const TASK_MOMENT: &str = "1kKRRsDPx";
const NEWER_TASK_MOMENT: &str = "1kKRRsDTx";
const INVALID_TASK_MOMENT: &str = "1kKRRsDQx";
const SEED_TITLE: &str = "Project incremental lifecycle";

#[derive(Debug, PartialEq, Eq)]
struct SemanticState {
    residency: ResidencyHead,
    entries: Vec<Entry>,
    ledger: Ledger,
    ready_at: BTreeMap<TaskStamp, Instant>,
    holds: Holds,
    resolution: Resolution,
}

#[test]
fn late_refusal_rolls_back_the_complete_projection_and_retry_is_idempotent() {
    let (bus, mut projection) = caught_up_fixture(0);
    let before = projection.clone();
    committed(
        bus.commit(invalid_late_suffix())
            .expect("late-invalid suffix commits to the durable fixture"),
    );
    let snapshot = bus.task_snapshot().expect("late-invalid snapshot reads");

    let first = projection
        .advance(&snapshot)
        .expect_err("an invalid task head refuses after the earlier steering fact");
    assert!(format!("{first:?}").contains("lifecycle::task-head-invalid"));
    assert_eq!(
        projection, before,
        "cursor, steering, entries, dependencies, subscriptions, fact instants, and ready times all roll back"
    );

    let retry = projection
        .advance(&snapshot)
        .expect_err("retrying the same suffix still refuses");
    assert!(format!("{retry:?}").contains("lifecycle::task-head-invalid"));
    assert_eq!(
        projection, before,
        "retry cannot duplicate the planned lane subscription or steering directive"
    );
    assert!(projection.ledger().directives().is_empty());

    bus.shutdown().expect("fixture bus stops");
}

#[test]
fn one_fact_advancement_is_suffix_bounded_and_restart_equivalent_at_different_scales() {
    let small = one_fact_scenario(1);
    let large = one_fact_scenario(2_048);

    assert_eq!(small, large);
}

#[test]
fn allocation_index_rebuilds_only_for_changed_task_heads() {
    let (bus, mut projection) = caught_up_fixture(0);
    assert_eq!(projection.selection_rebuilds(), 1);

    committed(
        bus.commit(unrelated_write("allocation-index", 32))
            .expect("unrelated journal facts commit"),
    );
    projection
        .advance(&bus.task_snapshot().expect("unrelated snapshot reads"))
        .expect("unrelated facts advance the lifecycle cursor");
    assert_eq!(
        projection.selection_rebuilds(),
        1,
        "unrelated commits retain the task-generation selection index"
    );

    committed(
        bus.commit(ready_task_write(NEWER_TASK_MOMENT, instant(31)))
            .expect("changed task heads commit"),
    );
    projection
        .advance(&bus.task_snapshot().expect("changed-task snapshot reads"))
        .expect("changed task heads advance");
    assert_eq!(
        projection.selection_rebuilds(),
        2,
        "one task-head generation rebuilds shared allocation evidence once"
    );
    bus.shutdown().expect("fixture bus stops");
}

#[test]
fn drain_burst_uses_the_selected_winners_ready_instant() {
    let (bus, mut projection) = policy_fixture(true, false);
    committed(
        bus.commit(ready_task_write(NEWER_TASK_MOMENT, instant(7)))
            .expect("newer ready candidate commits"),
    );
    let snapshot = bus.task_snapshot().expect("drive burst snapshot reads");
    projection
        .advance(&snapshot)
        .expect("drive burst projection advances");

    let targets = project_targets(
        &snapshot,
        &projection,
        instant(8),
        &Holds::new(3),
        &board::NextConfig::default(),
        None,
        &owners(&snapshot),
    )
    .expect("drive burst targets project");
    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0].drain_queue,
        Available::At(instant(4)),
        "the selected older winner settles from its own READY transition rather than the newer nonwinner"
    );
    assert!(matches!(
        resolve(&targets, &Backoff::default(), instant(8),),
        Resolution::Attempt {
            trigger: Trigger::DrainQueue,
            ..
        }
    ));
    bus.shutdown().expect("drive burst fixture bus stops");
}

#[test]
fn submitting_a_draft_arms_the_existing_drain_wake() {
    let (bus, mut projection) = policy_fixture(true, false);
    committed(
        bus.commit(submit_seed_draft(instant(7)))
            .expect("draft submission commits"),
    );
    let snapshot = bus.task_snapshot().expect("submitted draft snapshot reads");
    projection
        .advance(&snapshot)
        .expect("submitted draft projection advances");

    let targets = project_targets(
        &snapshot,
        &projection,
        instant(8),
        &Holds::new(3),
        &board::NextConfig::default(),
        None,
        &owners(&snapshot),
    )
    .expect("submitted draft target projects");
    assert_eq!(targets.len(), 1);
    assert!(
        targets[0]
            .task
            .is_some_and(board::Entry::is_submitted_draft)
    );
    assert_eq!(targets[0].drain_queue, Available::At(instant(10)));
    assert_eq!(
        resolve(&targets, &Backoff::default(), instant(10)),
        Resolution::Attempt {
            trigger: Trigger::DrainQueue,
            lane: lane(),
        }
    );
    bus.shutdown().expect("submitted draft fixture bus stops");
}

#[test]
fn drain_ready_reentry_restarts_the_first_members_timer_deadline() {
    let (bus, mut projection) = policy_fixture(true, false);
    committed(
        bus.commit(deferral_write(true, instant(5)))
            .expect("task deferral commits"),
    );
    projection
        .advance(&bus.task_snapshot().expect("deferred snapshot reads"))
        .expect("deferred projection advances");
    committed(
        bus.commit(deferral_write(false, instant(7)))
            .expect("task wake commits"),
    );
    let snapshot = bus.task_snapshot().expect("woken snapshot reads");
    projection
        .advance(&snapshot)
        .expect("woken projection advances");

    let targets = project_targets(
        &snapshot,
        &projection,
        instant(8),
        &Holds::new(3),
        &board::NextConfig::default(),
        None,
        &owners(&snapshot),
    )
    .expect("woken Drain target projects");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].drain_queue, Available::At(instant(10)));
    assert_eq!(
        resolve(&targets, &Backoff::default(), instant(8),),
        Resolution::Wait {
            trigger: Trigger::DrainQueue,
            lane: lane(),
            deadline: instant(10),
        }
    );
    assert_eq!(
        bus.task_snapshot()
            .expect("post-resolution snapshot reads")
            .cursor(),
        snapshot.cursor(),
        "deadline projection writes no claim or lifecycle fact"
    );
    assert_eq!(
        resolve(&targets, &Backoff::default(), instant(10),),
        Resolution::Attempt {
            trigger: Trigger::DrainQueue,
            lane: lane(),
        }
    );
    bus.shutdown().expect("Drain re-entry fixture bus stops");
}

#[test]
fn synthesized_guidance_and_peer_messages_stay_pending_without_resurrecting_a_vacant_lane() {
    let (bus, mut projection) = policy_fixture(false, false);
    committed(
        bus.commit(reminder_write())
            .expect("taste reminder commits"),
    );
    committed(
        bus.commit(peer_message_write())
            .expect("peer message commits"),
    );
    let snapshot = bus.task_snapshot().expect("reminder snapshot reads");
    projection
        .advance(&snapshot)
        .expect("reminder projection advances");
    assert_eq!(projection.ledger().pending(lane()).len(), 2);
    assert_eq!(
        projected_resolution(&snapshot, &projection),
        Resolution::Idle,
        "resident guidance and another lane's message stay answerable without becoming wake \
         authority"
    );

    committed(
        bus.commit(operator_direction_write())
            .expect("operator direction commits"),
    );
    let snapshot = bus
        .task_snapshot()
        .expect("operator direction snapshot reads");
    projection
        .advance(&snapshot)
        .expect("operator direction projection advances");
    assert_eq!(
        projected_resolution(&snapshot, &projection),
        Resolution::Attempt {
            trigger: Trigger::PendingInbox,
            lane: lane(),
        },
        "genuine operator steering resurrects the vacant lane"
    );
    bus.shutdown().expect("guidance fixture bus stops");
}

fn projected_resolution(snapshot: &bus::TaskSnapshot, projection: &Projection) -> Resolution {
    let now = instant(50);
    let targets = project_targets(
        snapshot,
        projection,
        now,
        &Holds::new(3),
        &board::NextConfig::default(),
        None,
        &owners(snapshot),
    )
    .expect("guidance lifecycle targets project");
    resolve(&targets, &Backoff::default(), now)
}

fn reminder_write() -> JournalWrite {
    let at = instant(10);
    let origin = format!("resident:{}", epoch())
        .parse::<Origin>()
        .expect("fixture resident origin");
    let reminder = body::TasteReminded {
        directive: uuid(10),
        key: "1kKRRsDRx".to_owned(),
        lane: lane(),
        taste: "polling".to_owned(),
        content: wire::Digest::compute(wire::Domain::TasteProse, b"poll until complete"),
        occupant: uuid(9),
        compaction: 1,
        locus: body::TranscriptLocus {
            source: Some(uuid(8)),
            member: 1,
            offset: 2,
            length: 3,
            event: 0,
            line: 1,
            column: 1,
        },
        text: "[taste:polling] Wait for the completion signal.".to_owned(),
        expires: instant(59),
    };
    let mut write = JournalWrite::new(
        Actor::Spice,
        origin.clone(),
        at,
        Producer::ResidentRecognition,
    );
    let reminded = write
        .fact(reminder.clone(), FactCause::Root)
        .expect("taste reminder fact builds");
    write.steering_head(reminded, move |position| {
        Ledger::reminded_head(&reminder, origin, at, position).expect("taste reminder head builds")
    });
    write
}

fn operator_direction_write() -> JournalWrite {
    let at = instant(11);
    let origin = format!("resident:{}", epoch())
        .parse::<Origin>()
        .expect("fixture resident origin");
    let direction = body::SteerSent {
        sender: Actor::TEST_PRINCIPAL,
        room: None,
        directive: uuid(11),
        fanout: Some(uuid(12)),
        recipients: Vec::new(),
        key: "1kKRRsDSx".to_owned(),
        recipient: Actor::Lane(lane()),
        text: "Resume for this operator request.".to_owned(),
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
        .expect("operator direction fact builds");
    write.steering_head(sent, move |position| {
        Ledger::sent_head(&direction, origin, at, position).expect("operator direction head builds")
    });
    write
}

fn peer_message_write() -> JournalWrite {
    let at = instant(10);
    let sender = uuid(40);
    let origin = format!("resident:{}", epoch())
        .parse::<Origin>()
        .expect("fixture resident origin");
    let told = body::SteerSent {
        sender: Actor::Lane(sender),
        room: None,
        directive: uuid(41),
        fanout: None,
        recipients: Vec::new(),
        key: "1kKRRsDTx".to_owned(),
        recipient: Actor::Lane(lane()),
        text: "The parser moved.".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: instant(59),
    };
    let mut write = JournalWrite::new(
        Actor::Lane(sender),
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let sent = write
        .fact(told.clone(), FactCause::Root)
        .expect("peer message fact builds");
    write.steering_head(sent, move |position| {
        Ledger::sent_head(&told, origin, at, position).expect("peer message head builds")
    });
    write
}

#[test]
fn drain_org_startup_projects_every_current_member() {
    let (bus, mut projection) = policy_fixture(true, false);
    add_vacant_org_members(&bus, &[lane_at(2), lane_at(3)]);
    let snapshot = bus.task_snapshot().expect("expanded team snapshot reads");
    projection
        .advance(&snapshot)
        .expect("expanded team projection advances");
    assert_eq!(
        projection.selection_rebuilds(),
        1,
        "adding vacant lanes reuses the existing task-generation selection index"
    );

    let targets = project_targets(
        &snapshot,
        &projection,
        instant(50),
        &Holds::new(3),
        &board::NextConfig::default(),
        None,
        &owners(&snapshot),
    )
    .expect("expanded team targets project");
    assert_eq!(
        targets.iter().map(|target| target.lane).collect::<Vec<_>>(),
        vec![lane(), lane_at(2), lane_at(3)],
    );
    assert!(
        targets
            .iter()
            .all(|target| target.drain_queue == Available::At(instant(4)))
    );
    bus.shutdown().expect("expanded team bus stops");
    assert_eq!(
        projection.selection_rebuilds(),
        1,
        "projecting every vacant lane performs no per-lane graph rebuild"
    );
}

fn one_fact_scenario(unrelated_history: usize) -> SemanticState {
    let (bus, mut incremental) = caught_up_fixture(unrelated_history);
    let before = bus.task_snapshot().expect("admitted snapshot reads");
    let admitted = incremental.cursor();
    let mut holds = replay_holds(before.facts().iter());
    committed(bus.commit(hold_write()).expect("one-fact tail commits"));
    let snapshot = bus.task_snapshot().expect("advanced snapshot reads");
    assert_eq!(
        snapshot.facts().after(admitted).count(),
        1,
        "a caught-up lifecycle projection touches only the committed suffix"
    );
    for envelope in snapshot.facts().after(admitted) {
        absorb_hold(&mut holds, envelope);
    }
    assert_eq!(holds, replay_holds(snapshot.facts().iter()));

    incremental
        .advance(&snapshot)
        .expect("incremental projection advances");
    let mut restarted = Projection::new();
    restarted
        .advance(&snapshot)
        .expect("restart projection rebuilds");
    assert_eq!(
        incremental, restarted,
        "one-fact incremental state is byte-for-byte equivalent to restart state"
    );

    let now = instant(50);
    let targets = project_targets(
        &snapshot,
        &incremental,
        now,
        &holds,
        &board::NextConfig::default(),
        None,
        &owners(&snapshot),
    )
    .expect("wake targets project from incremental state");
    let resolution = resolve(&targets, &Backoff::default(), now);
    let residency = snapshot
        .residency()
        .expect("fixture residency head exists")
        .body()
        .clone();
    let state = SemanticState {
        residency,
        entries: incremental.entries().to_vec(),
        ledger: incremental.ledger().clone(),
        ready_at: incremental.ready_at().clone(),
        holds,
        resolution,
    };

    bus.shutdown().expect("fixture bus stops");
    state
}

fn caught_up_fixture(unrelated_history: usize) -> (Bus, Projection) {
    let mut journal = Journal::in_memory().expect("fixture journal opens");
    journal.apply(seed_write()).expect("fixture task seeds");
    let bus = Bus::start(journal).expect("fixture bus starts");
    let mut projection = Projection::new();
    projection
        .advance(&bus.task_snapshot().expect("seed snapshot reads"))
        .expect("seed projection builds");

    if unrelated_history > 0 {
        committed(
            bus.commit(unrelated_write("retained-history", unrelated_history))
                .expect("unrelated history commits"),
        );
        projection
            .advance(&bus.task_snapshot().expect("history snapshot reads"))
            .expect("unrelated history advances");
    }
    (bus, projection)
}

fn policy_fixture(policy: bool, terminal: bool) -> (Bus, Projection) {
    seated_policy_fixture(policy, terminal, team())
}

/// The policy fixture with its one lane seated under `team`, which is the
/// lane's own identity until an operator places it elsewhere.
fn seated_policy_fixture(policy: bool, terminal: bool, team: CanonicalUuid) -> (Bus, Projection) {
    let mut journal = Journal::in_memory().expect("policy fixture journal opens");
    journal
        .apply(seed_write_seated(policy, terminal, team))
        .expect("policy fixture seeds");
    let bus = Bus::start(journal).expect("policy fixture bus starts");
    let mut projection = Projection::new();
    projection
        .advance(&bus.task_snapshot().expect("policy fixture snapshot reads"))
        .expect("policy fixture projection builds");
    (bus, projection)
}

fn seed_write() -> JournalWrite {
    seed_write_with_policy(true, false)
}

fn seed_write_with_policy(policy: bool, terminal: bool) -> JournalWrite {
    seed_write_seated(policy, terminal, team())
}

fn seed_write_seated(policy: bool, terminal: bool, team: CanonicalUuid) -> JournalWrite {
    let task = task(TASK_MOMENT);
    let phase = phase();
    let origin = format!("task:{}", handle(TASK_MOMENT))
        .parse::<Origin>()
        .expect("fixture task origin");
    let at = instant(1);
    let mut write = JournalWrite::new(
        Actor::Lane(lane()),
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let added = write
        .fact(
            body::TaskAdded {
                preparation: None,
                intent: None,
                task: task.clone(),
                title: "Project incremental lifecycle".to_owned(),
                pitch: "This work keeps the fixture honest.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: TOPIC.parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: vec![wire::Phase::Draft, phase],
                phase,
                creation: lane_creation(),
                revision: 1,
            },
            FactCause::Root,
        )
        .expect("task fact builds");
    attach_task_heads(&mut write, added, task, phase, Vec::new());
    let seated = write
        .fact(body::UpSeated { epoch: epoch() }, FactCause::Pending(added))
        .expect("resident fact builds");
    write.residency_head(seated, move |_| ResidencyHead {
        epoch: epoch(),
        state: ResidentState::Seated,
        topology_revision: 1,
        lanes: vec![LaneResidency {
            lane: lane(),
            team,
            name: None,
            worktree: "/fixture".to_owned(),
            life: if terminal { "drain" } else { "drive" }.to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: None,
            lineage: Vec::new(),
            capabilities: Vec::new(),
            session: None,
            disposition: "vacant".to_owned(),
        }],
        subscriptions: vec![wire::TeamSubscription {
            team,
            formed_at: None,
            drive: true,
            drain: policy,
            name: None,
            fast_mode: false,
        }],
        renewals: Vec::new(),
    });
    write
}

fn ready_task_write(moment: &str, at: Instant) -> JournalWrite {
    titled_ready_task_write(moment, at, "Newer ready lifecycle candidate")
}

/// A ready task whose creation fact and statement head both carry `title`.
fn titled_ready_task_write(moment: &str, at: Instant, title: &str) -> JournalWrite {
    let task = task(moment);
    let phase = phase();
    let origin = format!("task:{}", handle(moment))
        .parse::<Origin>()
        .expect("ready task origin");
    let mut write = JournalWrite::new(
        Actor::Lane(lane()),
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let added = write
        .fact(
            body::TaskAdded {
                preparation: None,
                intent: None,
                task: task.clone(),
                title: title.to_owned(),
                pitch: "This work keeps the fixture honest.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: TOPIC.parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: vec![wire::Phase::Draft, phase],
                phase,
                creation: lane_creation(),
                revision: 1,
            },
            FactCause::Root,
        )
        .expect("ready task fact builds");
    attach_task_heads_titled(&mut write, added, task, phase, Vec::new(), TOPIC, title);
    write
}

fn submit_seed_draft(at: Instant) -> JournalWrite {
    let task = task(TASK_MOMENT);
    let mut write = JournalWrite::new(
        Actor::Lane(lane()),
        format!("task:{}", handle(TASK_MOMENT))
            .parse()
            .expect("submitted draft task origin"),
        at,
        Producer::CommandDoor,
    );
    let submitted = write
        .fact(body::TaskSubmitted { task: task.clone() }, FactCause::Root)
        .expect("task submission fact builds");
    let next = phase();
    write.workflow_head(task, submitted, move |_| WorkflowHead {
        flow: vec![wire::Phase::Draft, next],
        phase: wire::Phase::Draft,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: true,
        generation: 1,
    });
    write
}

fn deferral_write(deferred: bool, at: Instant) -> JournalWrite {
    let task = task(TASK_MOMENT);
    let until = instant(20);
    let mut write = JournalWrite::new(
        Actor::Lane(lane()),
        format!("task:{}", handle(TASK_MOMENT))
            .parse()
            .expect("deferral task origin"),
        at,
        Producer::CommandDoor,
    );
    let changed = if deferred {
        write
            .fact(
                body::TaskDeferred {
                    task: task.clone(),
                    until,
                },
                FactCause::Root,
            )
            .expect("deferral fact builds")
    } else {
        write
            .fact(body::TaskWoken { task: task.clone() }, FactCause::Root)
            .expect("wake fact builds")
    };
    let phase = phase();
    write.workflow_head(task, changed, move |_| WorkflowHead {
        flow: vec![wire::Phase::Draft, phase],
        phase,
        status: wire::WorkflowStatus::pending(),
        deferral: deferred.then_some(until),
        submitted: false,
        generation: 0,
    });
    write
}

fn add_vacant_org_members(bus: &Bus, lanes: &[CanonicalUuid]) {
    let observed = bus
        .residency_head()
        .expect("expanded team residency reads")
        .expect("expanded team has residency");
    let mut next = observed.body().clone();
    for member in lanes {
        next.lanes.push(LaneResidency {
            lane: *member,
            team: team(),
            name: None,
            worktree: format!("/fixture/{member}"),
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
        });
    }
    next.lanes.sort_by_key(|row| row.lane);
    let mut write = JournalWrite::new(
        Actor::Spice,
        "recovery:expanded-team"
            .parse()
            .expect("expanded team origin"),
        instant(2),
        Producer::ResidentRecognition,
    );
    let mut cause = FactCause::Root;
    let mut expanded = None;
    for member in lanes {
        let seated = write
            .fact(
                body::LaneSeated {
                    lane: *member,
                    team: team(),
                    capability: *member,
                    name: None,
                    worktree: format!("/fixture/{member}"),
                    life: "drive".to_owned(),
                    vendor: "omp".parse().expect("fixture vendor"),
                    model: None,
                    effort: None,
                },
                cause,
            )
            .expect("expanded team seat fact builds");
        cause = FactCause::Pending(seated);
        expanded = Some(seated);
    }
    write.residency_head(
        expanded.expect("expanded team has at least one added member"),
        move |_| next,
    );
    committed(bus.commit(write).expect("expanded team commits"));
}

fn invalid_late_suffix() -> JournalWrite {
    let at = instant(20);
    let origin = format!("resident:{}", epoch())
        .parse::<Origin>()
        .expect("fixture resident origin");
    let mut write = JournalWrite::new(
        Actor::Lane(lane()),
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let steering = body::SteerSent {
        sender: Actor::Lane(lane()),
        room: None,
        directive: uuid(4),
        fanout: None,
        recipients: Vec::new(),
        key: "1kJK6N3Rx".to_owned(),
        recipient: Actor::Lane(lane()),
        text: "must not survive a later refusal".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: instant(59),
    };
    let sent = write
        .fact(steering.clone(), FactCause::Root)
        .expect("steering fact builds");
    let steering_origin = origin.clone();
    write.steering_head(sent, move |position| {
        Ledger::sent_head(&steering, steering_origin, at, position)
            .expect("fixture steering head builds")
    });
    let invalid_task = task(INVALID_TASK_MOMENT);
    let phase = phase();
    let added = write
        .fact(
            body::TaskAdded {
                preparation: None,
                intent: None,
                task: invalid_task.clone(),
                title: "Invalid late project".to_owned(),
                pitch: "This work keeps the fixture honest.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: "/outside/late".parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: vec![wire::Phase::Draft, phase],
                phase,
                creation: body::TaskCreationContext::unavailable(
                    body::TaskCreationUnavailable::SpiceActor,
                ),
                revision: 1,
            },
            FactCause::Pending(sent),
        )
        .expect("late-invalid task fact builds");
    attach_task_heads_titled(
        &mut write,
        added,
        invalid_task.clone(),
        phase,
        vec![invalid_task],
        "/outside/late",
        SEED_TITLE,
    );
    write
}

fn attach_task_heads(
    write: &mut JournalWrite,
    added: PendingFact,
    task: TaskStamp,
    phase: Phase,
    dependencies: Vec<TaskStamp>,
) {
    attach_task_heads_titled(write, added, task, phase, dependencies, TOPIC, SEED_TITLE);
}

fn attach_task_heads_titled(
    write: &mut JournalWrite,
    added: PendingFact,
    task: TaskStamp,
    phase: Phase,
    dependencies: Vec<TaskStamp>,
    topic: &'static str,
    title: &str,
) {
    let title = title.to_owned();
    let statement_task = task.clone();
    let provenance = wire::TaskProvenance::Task(task.clone());
    write.task_head(added, move |position| TaskHead {
        preparation: None,
        intent: None,
        born: statement_task,
        topic_origin: position,
        title,
        pitch: "This work keeps the fixture honest.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: topic.parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies,
        provenance,
        creation: body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::SpiceActor),
        revision: position,
    });
    write.workflow_head(task, added, move |_| WorkflowHead {
        flow: vec![wire::Phase::Draft, phase],
        phase,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
}

fn unrelated_write(label: &str, count: usize) -> JournalWrite {
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("recovery:{label}")
            .parse()
            .expect("fixture recovery origin"),
        instant(30),
        Producer::ResidentRecognition,
    );
    for index in 0..count {
        write
            .fact(
                body::NarrationCleared {
                    lane: lane(),
                    occupant: lane(),
                    moments: u32::try_from(index).expect("fixture count fits"),
                    prods: 1,
                    relief: body::NarrationRelief::Prose,
                },
                FactCause::Root,
            )
            .expect("unrelated fact builds");
    }
    write
}

fn hold_write() -> JournalWrite {
    let mut write = JournalWrite::new(
        Actor::Spice,
        "recovery:equal-tail"
            .parse()
            .expect("fixture recovery origin"),
        instant(40),
        Producer::BusTimer,
    );
    write
        .fact(
            body::LaneHeld {
                lane: lane(),
                evidence: "equal scale-tail hold".to_owned(),
                until: instant(55),
                vendor_failure: None,
            },
            FactCause::Root,
        )
        .expect("hold fact builds");
    write
}

fn replay_holds<'a>(facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>) -> Holds {
    let mut holds = Holds::new(3);
    for envelope in facts {
        absorb_hold(&mut holds, envelope);
    }
    holds
}

fn absorb_hold(holds: &mut Holds, envelope: &wire::Envelope<wire::AnyFact>) {
    if let Some(held) = envelope.fact.read::<body::LaneHeld>() {
        holds.absorb_held(&held);
    } else if let Some(resumed) = envelope.fact.read::<body::LaneResumed>() {
        holds.absorb_resumed(&resumed);
    }
}

fn committed(outcome: CommitOutcome) {
    assert!(
        matches!(outcome, CommitOutcome::Committed(_)),
        "fixture commit refused: {outcome:?}"
    );
}

fn handle(moment: &str) -> TaskHandle {
    TaskHandle::for_topic(
        &TOPIC.parse::<wire::Topic>().expect("fixture topic"),
        &moment.parse().expect("fixture task moment"),
    )
}

fn task(moment: &str) -> TaskStamp {
    handle(moment).born().expect("fixture task carries a stamp")
}

fn phase() -> Phase {
    "todo".parse().expect("fixture phase")
}

fn lane() -> CanonicalUuid {
    uuid(1)
}

/// The team the fixture lane is placed on when a test seats it the way an
/// operator's placement would, under an identity distinct from the lane's.
fn team() -> CanonicalUuid {
    uuid(7)
}

fn lane_creation() -> body::TaskCreationContext {
    body::TaskCreationContext::Transcript {
        session: "fixture-session".to_owned(),
        boundary: wire::TranscriptSourceHead {
            lane: lane(),
            vendor: "omp".parse().expect("fixture vendor"),
            stream: body::TASK_CREATION_CONTEXT_STREAM.to_owned(),
            source: uuid(90),
            next: body::TranscriptPosition {
                member: 1,
                offset: 1,
            },
        },
        creator_team: Some(team()),
    }
}

fn lane_at(last: u8) -> CanonicalUuid {
    uuid(last)
}

fn epoch() -> CanonicalUuid {
    uuid(2)
}

fn uuid(last: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131165{last:02}")
        .parse()
        .expect("fixture UUID")
}

fn instant(second: u8) -> Instant {
    format!("2030-04-12T18:44:{second:02}.271Z")
        .parse()
        .expect("fixture instant")
}

fn owners(snapshot: &bus::TaskSnapshot) -> std::sync::Arc<board::OwnershipSnapshot> {
    std::sync::Arc::new(board::OwnershipSnapshot::from_residency(
        board::team_policy::Config::default(),
        snapshot.residency().expect("fixture residency").body(),
    ))
}
