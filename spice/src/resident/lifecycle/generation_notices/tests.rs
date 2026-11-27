use super::*;
use crate::resident::lane_surface::{LaunchFixture, seed_task_claim, seed_task_claim_in_topic};
use bus::CommitOutcome;
use journal::{Journal, JournalWrite};
use wire::{LaneResidency, Origin, Producer, ResidentState, TaskHandle};

mod departure;
mod peer_resumption;

#[test]
fn trailing_generations_project_private_claimant_notices_without_steering() {
    let fixture = LaunchFixture::new();
    let (bus, task) = claimed_fixture(&fixture);
    wake(&bus, &task, Actor::Lane(fixture.lane));
    wake(&bus, &task, Actor::TEST_PRINCIPAL);
    wake(&bus, &task, Actor::Lane(fixture.lane));
    let snapshot = bus.task_snapshot().expect("noted snapshot reads");
    let claim = snapshot.tasks()[0]
        .latest_claim()
        .map(|held| held.claim())
        .expect("claim head")
        .body();

    let projected = notices(&bus, &snapshot).expect("private notices project");
    assert_eq!(
        projected
            .iter()
            .map(|(_, notice)| notice.generation)
            .collect::<Vec<_>>(),
        [2, 3],
        "the holder's current change was acknowledged, but changes after it trailed were not",
    );
    let handle = TaskHandle::for_topic(
        &"/task/claim".parse::<wire::Topic>().expect("fixture topic"),
        &task,
    )
    .to_string();
    let lane_editor = Actor::Lane(fixture.lane).to_string();
    let principal_editor = Actor::TEST_PRINCIPAL.to_string();
    assert_eq!(
        projected
            .iter()
            .map(|(lane, notice)| (
                *lane,
                notice.occupant,
                notice.task.as_str(),
                notice.claim,
                notice.editor.as_str(),
            ))
            .collect::<Vec<_>>(),
        [
            (
                fixture.lane,
                fixture.occupant,
                handle.as_str(),
                claim.claimed.get(),
                principal_editor.as_str(),
            ),
            (
                fixture.lane,
                fixture.occupant,
                handle.as_str(),
                claim.claimed.get(),
                lane_editor.as_str(),
            ),
        ],
    );
    assert_eq!(
        notices(&bus, &snapshot).expect("duplicate observation projects"),
        projected,
        "the occupant driver can deduplicate a stable authority identity",
    );
    assert!(
        !reconcile(
            &bus,
            &fixture.paths,
            fixture.epoch,
            &snapshot,
            super::super::review_feedback::FindingWindow {
                cursor: bus.cursor().expect("cursor reads"),
                held: &mut BTreeSet::new(),
                now: current_instant().expect("clock reads"),
                lifetime: Duration::from_secs(3_600),
            },
        )
        .expect("private notice reconciliation succeeds"),
        "private delivery does not mutate durable authority",
    );
    assert!(
        bus.read(move |read| crate::steering_read::pending(read, Some(fixture.lane)))
            .expect("steering index reads")
            .is_empty(),
        "task authority reminders never enter the steering inbox",
    );
    finish(bus);
}

#[test]
fn acknowledgement_clears_prior_generations_and_a_later_edit_gets_a_new_identity() {
    let fixture = LaunchFixture::new();
    let (bus, task) = claimed_fixture(&fixture);
    wake(&bus, &task, Actor::TEST_PRINCIPAL);
    wake(&bus, &task, Actor::TEST_PRINCIPAL);
    let before = bus.task_snapshot().expect("changed snapshot reads");
    let prior = notices(&bus, &before).expect("prior notices project");
    assert_eq!(prior.len(), 2);

    acknowledge(&bus, &task);
    let acknowledged = bus.task_snapshot().expect("acknowledged snapshot reads");
    assert!(
        notices(&bus, &acknowledged)
            .expect("acknowledged notices project")
            .is_empty(),
        "successful acknowledgement retires every prior reminder",
    );

    wake(&bus, &task, Actor::TEST_PRINCIPAL);
    let changed = bus.task_snapshot().expect("later snapshot reads");
    let [(_, later)] = notices(&bus, &changed)
        .expect("later notice projects")
        .try_into()
        .expect("one later notice");
    assert!(
        prior
            .iter()
            .all(|(_, notice)| notice.generation != later.generation),
        "a later generation has a new delivery identity",
    );

    end_claim(&bus, &task);
    let released = bus.task_snapshot().expect("released snapshot reads");
    assert!(
        notices(&bus, &released)
            .expect("released notices project")
            .is_empty(),
        "a released claim receives no authority reminder",
    );
    finish(bus);
}

#[test]
fn only_the_lane_whose_write_opened_a_hold_hears_of_it_once() {
    let fixture = LaunchFixture::new();
    let (bus, task) = claimed_fixture(&fixture);
    let lane = Actor::Lane(fixture.lane);
    let hold = wire::TasteHold {
        matches: vec![wire::TasteMatch {
            field: wire::StatementField::Title,
            line: 1,
            taste: "taskwording".to_owned(),
            finding: None,
        }],
        rechecks: 0,
        opened: None,
    };

    // The operator's policy read opens the hold; the lane then rewires the
    // task's dependencies, carrying the hold through without reading policy.
    let cursor = bus.cursor().expect("cursor reads");
    restate(
        &bus,
        &task,
        Actor::TEST_PRINCIPAL,
        Hold::Read(Some(hold.clone())),
    );
    restate(&bus, &task, lane.clone(), Hold::CarriedThrough);
    let snapshot = bus.task_snapshot().expect("carried snapshot reads");
    assert!(
        taste_holds(&bus, &snapshot, cursor)
            .expect("carried holds project")
            .is_empty(),
        "a lane that only carried another writer's hold is not told it wrote it",
    );

    // Cleared, then reopened by the lane's own policy-reading write, then
    // carried through once more by that lane's dependency edit.
    restate(&bus, &task, Actor::TEST_PRINCIPAL, Hold::Read(None));
    let cursor = bus.cursor().expect("cursor reads");
    let opening = restate(&bus, &task, lane.clone(), Hold::Read(Some(hold)));
    restate(&bus, &task, lane, Hold::CarriedThrough);
    let snapshot = bus.task_snapshot().expect("reopened snapshot reads");
    let [(told, notice)] = taste_holds(&bus, &snapshot, cursor)
        .expect("hold notices project")
        .try_into()
        .expect("the opening write's lane hears the hold once");
    assert_eq!((told, notice.occupant), (fixture.lane, fixture.occupant));
    assert_eq!(
        notice.position,
        opening.get(),
        "the notice names the write that opened the hold, not the later carrier",
    );
    assert_eq!(
        notice.task,
        TaskHandle::for_topic(
            &"/task/claim".parse::<wire::Topic>().expect("fixture topic"),
            &task
        )
        .to_string()
    );
    assert_eq!(notice.matches, ["title line 1 taste taskwording"]);
    assert!(
        taste_holds(&bus, &snapshot, bus.cursor().expect("cursor reads"))
            .expect("observed holds project")
            .is_empty(),
        "a hold observed by an earlier pass is not announced again",
    );
    assert!(
        taste_holds(&bus, &snapshot, None)
            .expect("first pass projects")
            .is_empty(),
        "the first pass has no earlier observation to announce against",
    );
    finish(bus);
}

#[test]
fn resident_minted_claim_projects_one_occupant_scoped_assignment() {
    let fixture = LaunchFixture::new();
    let (bus, task) = automatic_claimed_fixture(&fixture);
    let snapshot = bus.task_snapshot().expect("automatic claim snapshot reads");
    let claim = snapshot.tasks()[0]
        .latest_claim()
        .map(|held| held.claim())
        .expect("claim head")
        .body();
    let handle = TaskHandle::for_topic(
        &"/task/claim".parse::<wire::Topic>().expect("fixture topic"),
        &task,
    )
    .to_string();

    let projected = assignment_notices(&snapshot);
    assert_eq!(
        projected
            .iter()
            .map(|(lane, notice)| (*lane, notice.occupant, notice.task.as_str(), notice.claim))
            .collect::<Vec<_>>(),
        [(
            fixture.lane,
            fixture.occupant,
            handle.as_str(),
            claim.claimed.get()
        )],
    );
    assert_eq!(
        assignment_notices(&snapshot),
        projected,
        "stable authority can be retried until the occupant driver receives it",
    );

    let observed = bus.cursor().expect("cursor reads");
    end_claim(&bus, &task);
    let released = bus.task_snapshot().expect("released snapshot reads");
    assert!(
        assignment_notices(&released).is_empty(),
        "ended possession is not announced as an assignment",
    );
    assert_eq!(
        assignment_ends(&released, observed),
        [(
            fixture.lane,
            TaskAssignmentEnd {
                occupant: fixture.occupant,
                task: TaskHandle::for_topic(
                    &"/task/claim".parse::<wire::Topic>().expect("fixture topic"),
                    &task
                )
                .to_string(),
                claim: claim.claimed.get(),
            },
        )],
        "an ending after the previous pass is told to the occupant that held the claim",
    );
    assert!(
        assignment_ends(&released, bus.cursor().expect("cursor reads")).is_empty(),
        "an ending the previous pass already observed is not told again",
    );
    assert!(
        assignment_ends(&released, None).is_empty(),
        "a first pass has no earlier observation to tell endings against",
    );
    finish(bus);
}

#[test]
fn a_join_waits_until_the_participant_has_a_proved_address() {
    assert_new_shared_round_updates(false, false);
}

#[test]
fn an_addressed_join_projects_one_update_to_each_existing_partner() {
    assert_new_shared_round_updates(true, false);
}

#[test]
fn an_addressed_arrival_projects_one_distinct_update_to_each_active_partner() {
    assert_new_shared_round_updates(true, true);
}

#[test]
fn an_addressed_free_reaches_only_partners_already_in_the_round() {
    assert_addressed_departure(Ending::Free);
}

#[test]
fn an_addressed_lapse_reaches_only_partners_already_in_the_round() {
    assert_addressed_departure(Ending::Lapse);
}

#[test]
fn a_lapse_after_occupant_carry_names_the_carried_participant() {
    assert_addressed_departure(Ending::CarriedLapse);
}

#[derive(Clone, Copy)]
enum Ending {
    Free,
    Lapse,
    CarriedLapse,
}

fn assert_addressed_departure(ending: Ending) {
    let fixture = LaunchFixture::new();
    let (bus, task) = automatic_claimed_fixture(&fixture);
    target_shared_todo(&bus, &task);

    let lane = uuid(30);
    let occupant = uuid(31);
    let mut left_address = add_participant_lane(&bus, &fixture, lane, Some(occupant), "leaver")
        .expect("seated participant has an address");
    let _partner_address = seed_birth(&bus, fixture.lane, fixture.occupant, fixture.epoch);
    join_participant(&bus, &task, lane, occupant, fixture.epoch);
    match ending {
        Ending::Free => end_claim(&bus, &task),
        Ending::Lapse => lapse_claim(&bus, &task),
        Ending::CarriedLapse => {
            let successor = uuid(34);
            left_address = seed_birth(&bus, lane, successor, fixture.epoch);
            carry_claim(&bus, &task, successor);
            lapse_claim(&bus, &task);
        }
    }

    let snapshot = bus.task_snapshot().expect("released snapshot reads");
    let heads = &snapshot.tasks()[0];
    let left_event = heads
        .claims()
        .iter()
        .map(|held| held.claim().body())
        .find(|claim| claim.lane == lane)
        .expect("released participant remains in the ending snapshot")
        .fact
        .get();
    let notices = round_update_notices(&snapshot);
    let departures = notices
        .iter()
        .filter(|(recipient, notice)| *recipient == fixture.lane && notice.event == left_event)
        .map(|(_, notice)| notice)
        .collect::<Vec<_>>();
    let [update] = departures.as_slice() else {
        panic!("the departure reaches the remaining partner once: {departures:?}");
    };
    assert_eq!(update.action, TaskRoundUpdateKind::Left);
    assert_eq!(update.participant, format!("leaver {left_address}"));

    let later_lane = uuid(32);
    let later_occupant = uuid(33);
    add_participant_lane(&bus, &fixture, later_lane, Some(later_occupant), "later")
        .expect("later participant has an address");
    join_participant(&bus, &task, later_lane, later_occupant, fixture.epoch);
    let later_snapshot = bus.task_snapshot().expect("later snapshot reads");
    assert!(
        round_update_notices(&later_snapshot)
            .iter()
            .all(|(recipient, notice)| { *recipient != later_lane || notice.event != left_event }),
        "a claim that joins later does not receive a historical departure"
    );

    finish(bus);
}

#[test]
fn an_addressless_departure_reprojects_after_its_address_is_proved() {
    let fixture = LaunchFixture::new();
    let (bus, task) = automatic_claimed_fixture(&fixture);
    target_shared_todo(&bus, &task);

    let lane = uuid(30);
    let occupant = uuid(31);
    assert!(
        add_participant_lane(&bus, &fixture, lane, None, "leaver").is_none(),
        "the participant begins without an occupant address"
    );
    let _partner_address = seed_birth(&bus, fixture.lane, fixture.occupant, fixture.epoch);
    join_participant(&bus, &task, lane, occupant, fixture.epoch);
    end_claim(&bus, &task);

    let unproved = bus
        .task_snapshot()
        .expect("unproved departure snapshot reads");
    assert!(
        round_update_notices(&unproved)
            .iter()
            .all(|(_, notice)| notice.action != TaskRoundUpdateKind::Left),
        "an addressless participant is not announced"
    );

    let address = seed_birth(&bus, lane, occupant, fixture.epoch);
    let proved = bus
        .task_snapshot()
        .expect("proved departure snapshot reads");
    let notices = round_update_notices(&proved);
    let departure = notices
        .iter()
        .find(|(recipient, notice)| {
            *recipient == fixture.lane && notice.action == TaskRoundUpdateKind::Left
        })
        .map(|(_, notice)| notice)
        .expect("the durable departure reprojects after address proof");
    assert_eq!(departure.participant, format!("leaver {address}"));
    assert_eq!(
        round_update_notices(&proved),
        notices,
        "resident restart can rehydrate the same durable current-round event"
    );

    finish(bus);
}

fn assert_new_shared_round_updates(seated: bool, arrived: bool) {
    let fixture = LaunchFixture::new();
    let (bus, task) = automatic_claimed_fixture(&fixture);
    target_shared_todo(&bus, &task);

    let lane = uuid(30);
    let occupant = uuid(31);
    let joined_address =
        add_participant_lane(&bus, &fixture, lane, seated.then_some(occupant), "joiner");
    let _partner_address = seed_birth(&bus, fixture.lane, fixture.occupant, fixture.epoch);
    join_participant(&bus, &task, lane, occupant, fixture.epoch);

    if arrived {
        arrive_participant(&bus, &task, lane);
    }
    let snapshot = bus.task_snapshot().expect("joined snapshot reads");
    let heads = &snapshot.tasks()[0];
    let joined_claim = heads
        .claims()
        .iter()
        .map(|held| held.claim().body())
        .find(|claim| claim.lane == lane)
        .expect("joining claim remains active");
    let notices = round_update_notices(&snapshot);
    let joined_update = notices
        .iter()
        .find(|(recipient, notice)| {
            *recipient == fixture.lane && notice.event == joined_claim.claimed.get()
        })
        .map(|(_, notice)| notice);
    if let Some(joined_address) = joined_address {
        let notice = joined_update.expect("the addressed join reaches the existing partner");
        assert_eq!(notice.occupant, fixture.occupant);
        assert_eq!(
            notice.holder_claim,
            heads.claims()[0].claim().body().claimed.get()
        );
        assert_eq!(notice.action, TaskRoundUpdateKind::Joined);
        assert_eq!(notice.participant, format!("joiner {joined_address}"));
        assert_eq!(
            round_update_notices(&snapshot),
            notices,
            "the driver receives the same durable events until it deduplicates them",
        );
    } else {
        assert!(
            joined_update.is_none(),
            "the existing partner is not told about an addressless joiner"
        );
    }
    if arrived {
        let arrival = heads
            .completion()
            .body()
            .arrivals
            .iter()
            .find(|arrival| arrival.lane == lane)
            .expect("participant arrival is retained");
        let arrival_update = notices
            .iter()
            .find(|(recipient, notice)| {
                *recipient == fixture.lane && notice.event == arrival.fact.get()
            })
            .map(|(_, notice)| notice)
            .expect("addressed arrival reaches the active partner");
        assert_eq!(arrival_update.action, TaskRoundUpdateKind::Arrived);
        assert_eq!(
            arrival_update.participant,
            format!("joiner {}", joined_address.unwrap())
        );
        assert_eq!(
            notices
                .iter()
                .filter(|(recipient, notice)| {
                    *recipient == fixture.lane && notice.event == arrival.fact.get()
                })
                .count(),
            1,
            "one completion produces an arrival update without a departure duplicate"
        );
    }

    finish(bus);
}

fn join_participant(
    bus: &Bus,
    task: &TaskStamp,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    epoch: CanonicalUuid,
) {
    let at = now();
    let expiry = shifted(at, 60);
    let maximum = shifted(at, 3_600);
    let actor = Actor::Lane(lane);
    let mut join = JournalWrite::new(actor.clone(), task_origin(task), at, Producer::CommandDoor);
    let won = join
        .fact(
            wire::body::ClaimWon {
                task: task.clone(),
                actor: actor.clone(),
                lane,
                occupant,
                epoch,
                cell: wire::ClaimCell {
                    topic: Some("/task/claim".parse().expect("fixture topic")),
                    phase: "todo".parse().expect("fixture phase"),
                },
                until: expiry,
                git: None,
                recovery: None,
                source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Root,
        )
        .expect("join fact builds");
    join.claim_head(task.clone(), won, move |position| wire::ClaimHead {
        actor,
        fact: position,
        claimed: position,
        cell: wire::ClaimCell {
            topic: Some("/task/claim".parse().expect("fixture topic")),
            phase: "todo".parse().expect("fixture phase"),
        },
        lane,
        occupant,
        epoch,
        recovery: None,
        disposition: ClaimDisposition::Active,
        acknowledged: 0,
    });
    join.lease_head(task.clone(), won, move |claimed| wire::LeaseHead {
        claimed,
        start: at,
        renewal: at,
        expiry,
        maximum,
        epoch,
    });
    committed(bus.commit(join).expect("join commits"));
}

fn arrive_participant(bus: &Bus, task: &TaskStamp, lane: CanonicalUuid) {
    let snapshot = bus.task_snapshot().expect("pre-arrival snapshot reads");
    let heads = &snapshot.tasks()[0];
    let mut claim = heads
        .claims()
        .iter()
        .map(|held| held.claim().body())
        .find(|claim| claim.lane == lane)
        .expect("arriving claim exists")
        .clone();
    let mut completion = heads.completion().body().clone();
    let actor = claim.actor.clone();
    let claimed = claim.claimed;
    let mut write = JournalWrite::new(
        actor.clone(),
        task_origin(task),
        now(),
        Producer::CommandDoor,
    );
    let arrived = write
        .fact(
            wire::body::TaskArrived {
                task: task.clone(),
                operation: None,
                claimed,
                lane,
                occupant: claim.occupant,
                phase: claim.cell.phase,
                round: completion.round,
                validation: "participant arrived".to_owned(),
                git: None,
                source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
                disposition: None,
            },
            FactCause::Root,
        )
        .expect("arrival fact builds");
    let completion_task = task.clone();
    write.completion_head(completion_task, arrived, move |fact| {
        completion.arrivals.push(wire::Arrival {
            claimed,
            actor,
            lane,
            fact,
        });
        completion
    });
    claim.disposition = ClaimDisposition::Freed;
    write.claim_head(task.clone(), arrived, move |fact| wire::ClaimHead {
        fact,
        ..claim
    });
    committed(bus.commit(write).expect("arrival commits"));
}

fn target_shared_todo(bus: &Bus, task: &TaskStamp) {
    let phase: wire::Phase = "todo".parse().expect("fixture phase");
    let mut completion = bus.task_snapshot().expect("target snapshot reads").tasks()[0]
        .completion()
        .body()
        .clone();
    let mut target = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        task_origin(task),
        now(),
        Producer::CommandDoor,
    );
    let targeted = target
        .fact(
            wire::body::CompletionTargeted {
                task: task.clone(),
                phase,
                target: std::num::NonZeroU32::new(2).expect("positive target"),
            },
            FactCause::Root,
        )
        .expect("target fact builds");
    let target_task = task.clone();
    target.completion_head(target_task, targeted, move |_| {
        completion.set_target(
            phase,
            std::num::NonZeroU32::new(2).expect("positive target"),
        );
        completion
    });
    committed(bus.commit(target).expect("target commits"));
}

fn add_participant_lane(
    bus: &Bus,
    fixture: &LaunchFixture,
    lane: CanonicalUuid,
    occupant: Option<CanonicalUuid>,
    name: &str,
) -> Option<wire::OccupantAddress> {
    let (position, mut residency) = {
        let snapshot = bus.task_snapshot().expect("pre-participant snapshot reads");
        let head = snapshot.residency().expect("resident remains seated");
        (head.position(), head.body().clone())
    };
    let capability = uuid(32);
    let row = LaneResidency {
        lane,
        team: lane,
        name: Some(name.to_owned()),
        worktree: fixture.root.join(name).display().to_string(),
        life: "drive".to_owned(),
        vendor: fixture.vendor.clone(),
        model: None,
        effort: None,
        fast_mode: false,
        occupant,
        lineage: occupant.into_iter().collect(),
        capabilities: vec![capability],
        session: None,
        disposition: if occupant.is_some() {
            "occupied".to_owned()
        } else {
            "vacant".to_owned()
        },
    };
    residency.topology_revision = residency.topology_revision.saturating_add(1);
    residency.lanes.push(row);
    let origin: Origin = format!("resident:{}", fixture.epoch)
        .parse()
        .expect("resident origin");
    let mut write = JournalWrite::new(Actor::Spice, origin, now(), Producer::ResidentRecognition);
    let seated = write
        .fact(
            wire::body::LaneSeated {
                lane,
                team: lane,
                capability,
                name: Some(name.to_owned()),
                worktree: fixture.root.join(name).display().to_string(),
                life: "drive".to_owned(),
                vendor: fixture.vendor.clone(),
                model: None,
                effort: None,
            },
            FactCause::Existing(position),
        )
        .expect("participant seating fact builds");
    write.residency_head(seated, move |_| residency);
    committed(bus.commit(write).expect("participant seating commits"));
    occupant.map(|occupant| seed_birth(bus, lane, occupant, fixture.epoch))
}

fn seed_birth(
    bus: &Bus,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    epoch: CanonicalUuid,
) -> wire::OccupantAddress {
    let origin: Origin = format!("resident:{epoch}")
        .parse()
        .expect("resident origin");
    let mut write = JournalWrite::new(Actor::Spice, origin, now(), Producer::ResidentRecognition);
    write
        .fact(
            wire::body::LaneStarted {
                lane,
                occupant,
                epoch,
                group: 7,
                fresh_session: true,
            },
            FactCause::Root,
        )
        .expect("participant birth fact builds");
    match bus.commit(write).expect("participant birth commits") {
        CommitOutcome::Committed(committed) => wire::OccupantAddress::new(
            committed
                .cursor()
                .position()
                .expect("participant birth has one position"),
        ),
        outcome => panic!("participant birth did not commit: {outcome:?}"),
    }
}

fn claimed_fixture(fixture: &LaunchFixture) -> (Bus, TaskStamp) {
    let at = now();
    claimed_fixture_with_deadlines(
        fixture,
        shifted(at, -60),
        shifted(at, 3_600),
        shifted(at, 7_200),
        "/task/claim".parse().expect("fixture topic"),
    )
}

fn claimed_fixture_in_topic(fixture: &LaunchFixture, topic: wire::TaskTopic) -> (Bus, TaskStamp) {
    let at = now();
    claimed_fixture_with_deadlines(
        fixture,
        shifted(at, -60),
        shifted(at, 3_600),
        shifted(at, 7_200),
        topic,
    )
}

fn automatic_claimed_fixture(fixture: &LaunchFixture) -> (Bus, TaskStamp) {
    let at = now();
    claimed_fixture_with_deadlines(
        fixture,
        at,
        shifted(at, 60),
        shifted(at, 3_600),
        "/task/claim".parse().expect("fixture topic"),
    )
}

fn claimed_fixture_with_deadlines(
    fixture: &LaunchFixture,
    start: Instant,
    expiry: Instant,
    maximum: Instant,
    topic: wire::TaskTopic,
) -> (Bus, TaskStamp) {
    let bus = Bus::start(Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    let row = LaneResidency {
        lane: fixture.lane,
        team: fixture.lane,
        name: Some("claimant".to_owned()),
        worktree: fixture.root.display().to_string(),
        life: "drive".to_owned(),
        vendor: fixture.vendor.clone(),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: Some(fixture.occupant),
        lineage: vec![fixture.occupant],
        capabilities: vec![fixture.capability],
        session: None,
        disposition: "occupied".to_owned(),
    };
    let origin: Origin = format!("resident:{}", fixture.epoch)
        .parse()
        .expect("resident origin");
    let mut write = JournalWrite::new(Actor::Spice, origin, start, Producer::CommandDoor);
    let seated = write
        .fact(
            wire::body::UpSeated {
                epoch: fixture.epoch,
            },
            FactCause::Root,
        )
        .expect("resident seating fact builds");
    let epoch = fixture.epoch;
    let seated_row = row.clone();
    write.residency_head(seated, move |_| wire::ResidencyHead {
        epoch,
        state: ResidentState::Seated,
        topology_revision: 1,
        lanes: vec![seated_row],
        subscriptions: Vec::new(),
        renewals: Vec::new(),
    });
    committed(bus.commit(write).expect("resident seating commits"));
    let task: TaskStamp = "1kKRRsDQx".parse().expect("task stamp");
    seed_task_claim_in_topic(
        &bus,
        &task,
        &row,
        fixture.epoch,
        (start, expiry, maximum),
        topic,
    );
    (bus, task)
}

/// Change `task` as `actor` with a wake, which mutates it and nothing else.
fn wake(bus: &Bus, task: &TaskStamp, actor: Actor) {
    let mut write = JournalWrite::new(actor, task_origin(task), now(), Producer::CommandDoor);
    write
        .fact(
            wire::body::TaskWoken { task: task.clone() },
            FactCause::Root,
        )
        .expect("task wake builds");
    committed(bus.commit(write).expect("task wake commits"));
}

/// How one statement write treats the taste hold.
enum Hold {
    /// A policy read that leaves this hold, opening it if new, or clears it.
    Read(Option<wire::TasteHold>),
    /// A write that reads no policy, such as a dependency edit, and keeps the
    /// standing hold as it is.
    CarriedThrough,
}

/// Revise the statement as `actor`, returning the revision's fact.
fn restate(bus: &Bus, task: &TaskStamp, actor: Actor, hold: Hold) -> Seq {
    let snapshot = bus.task_snapshot().expect("pre-edit snapshot reads");
    let mut statement = snapshot.tasks()[0].statement().body().clone();
    let reads_policy = match hold {
        Hold::Read(taste) => {
            statement.taste = taste;
            true
        }
        Hold::CarriedThrough => false,
    };
    let revision = u32::try_from(statement.revision.get())
        .expect("fixture revision fits")
        .saturating_add(1);
    let mut write = JournalWrite::new(actor, task_origin(task), now(), Producer::CommandDoor);
    let edited = write
        .fact(
            wire::body::TaskEdited {
                task: task.clone(),
                revision,
                dependencies: (!reads_policy).then(|| statement.dependencies.clone()),
                flow: None,
                intent: None,
            },
            FactCause::Root,
        )
        .expect("task edit builds");
    write.task_head(edited, move |position| {
        let mut statement = wire::TaskHead {
            revision: position,
            ..statement
        };
        if let Some(hold) = statement.taste.as_mut().filter(|_| reads_policy) {
            hold.record_opening(position);
        }
        statement
    });
    committed(bus.commit(write).expect("task edit commits"));
    bus.task_snapshot().expect("edited snapshot reads").tasks()[0]
        .statement()
        .position()
}

fn acknowledge(bus: &Bus, task: &TaskStamp) {
    let snapshot = bus
        .task_snapshot()
        .expect("pre-acknowledgement snapshot reads");
    let heads = &snapshot.tasks()[0];
    let claim = heads
        .latest_claim()
        .map(|held| held.claim())
        .expect("claim head")
        .body()
        .clone();
    let generation = heads.workflow().body().generation;
    let mut write = JournalWrite::new(
        claim.actor.clone(),
        task_origin(task),
        now(),
        Producer::CommandDoor,
    );
    let acknowledged = write
        .fact(
            wire::body::ClaimAcknowledged {
                task: task.clone(),
                generation,
            },
            FactCause::Root,
        )
        .expect("claim acknowledgement fact builds");
    write.claim_head(task.clone(), acknowledged, move |fact| wire::ClaimHead {
        fact,
        acknowledged: generation,
        ..claim
    });
    committed(bus.commit(write).expect("acknowledgement commits"));
}

fn lapse_claim(bus: &Bus, task: &TaskStamp) {
    let snapshot = bus.task_snapshot().expect("pre-lapse snapshot reads");
    let claim = snapshot.tasks()[0]
        .latest_claim()
        .map(|held| held.claim())
        .expect("claim head")
        .body()
        .clone();
    let at = now();
    let mut write = JournalWrite::new(
        claim.actor.clone(),
        task_origin(task),
        at,
        Producer::BusTimer,
    );
    let lapsed = write
        .fact(
            wire::body::ClaimLapsed {
                task: task.clone(),
                occupant: claim.occupant,
                expiry: at,
            },
            FactCause::Root,
        )
        .expect("claim lapse fact builds");
    write.claim_head(task.clone(), lapsed, move |position| wire::ClaimHead {
        fact: position,
        disposition: ClaimDisposition::Lapsed,
        ..claim
    });
    committed(bus.commit(write).expect("claim lapse commits"));
}

/// Free the latest claim as `task free`, self-deferral, and automatic release
/// write it: a root fact naming the holder, with no cause.
fn end_claim(bus: &Bus, task: &TaskStamp) {
    let snapshot = bus.task_snapshot().expect("pre-release snapshot reads");
    let claim = snapshot.tasks()[0]
        .latest_claim()
        .map(|held| held.claim())
        .expect("claim head")
        .body()
        .clone();
    let mut write = JournalWrite::new(
        claim.actor.clone(),
        task_origin(task),
        now(),
        Producer::CommandDoor,
    );
    let freed = write
        .fact(
            wire::body::ClaimFreed {
                task: task.clone(),
                actor: claim.actor.clone(),
                parked: None,
            },
            FactCause::Root,
        )
        .expect("claim release fact builds");
    write.claim_head(task.clone(), freed, move |position| wire::ClaimHead {
        fact: position,
        disposition: ClaimDisposition::Freed,
        ..claim
    });
    committed(bus.commit(write).expect("claim release commits"));
}

/// Carry the latest claim to `occupant` as resident restart writes it.
fn carry_claim(bus: &Bus, task: &TaskStamp, occupant: CanonicalUuid) {
    let snapshot = bus.task_snapshot().expect("pre-carry snapshot reads");
    let claim = snapshot.tasks()[0]
        .latest_claim()
        .map(|held| held.claim())
        .expect("claim head")
        .body()
        .clone();
    let mut write = JournalWrite::new(
        Actor::Spice,
        task_origin(task),
        now(),
        Producer::ResidentRecognition,
    );
    let carried = write
        .fact(
            wire::body::ClaimCarried {
                task: task.clone(),
                lane: claim.lane,
                occupant,
                ancestor: claim.occupant,
                epoch: claim.epoch,
                source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Root,
        )
        .expect("claim carry fact builds");
    write.claim_head(task.clone(), carried, move |position| wire::ClaimHead {
        fact: position,
        occupant,
        ..claim
    });
    committed(bus.commit(write).expect("claim carry commits"));
}

fn task_origin(task: &TaskStamp) -> Origin {
    format!("task:{task}").parse().expect("task origin")
}

fn finish(bus: Bus) {
    bus.verify().expect("notice authority verifies");
    bus.shutdown().expect("notice bus stops");
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
    assert!(
        matches!(outcome, CommitOutcome::Committed(_)),
        "fixture write did not commit: {outcome:?}",
    );
}

fn peer_wakes(authority: &bus::TaskSnapshot, directive: CanonicalUuid) -> Option<CanonicalUuid> {
    let envelope = authority
        .facts()
        .iter()
        .find(|row| {
            row.fact
                .read::<wire::body::SteerSent>()
                .is_some_and(|sent| sent.directive == directive)
        })
        .expect("peer send fact exists");
    crate::resident::steering::sent_wakes(
        &wire::principal::PrincipalRoles::default(),
        authority,
        envelope,
    )
}

#[test]
fn only_a_typed_invitation_from_a_live_claim_wakes_a_nonparticipant() {
    let fixture = LaunchFixture::new();
    let (bus, task) = automatic_claimed_fixture(&fixture);
    let address = seed_birth(&bus, fixture.lane, fixture.occupant, fixture.epoch);
    let invitee = uuid(60);
    let snapshot = bus.task_snapshot().expect("claimed snapshot reads");
    let heads = &snapshot.tasks()[0];
    let workflow = heads.workflow().body().clone();
    let claim = heads.claims()[0].claim().body().claimed;
    let invitation = |key: &'static str| {
        let control = wire::TaskInviteControl::new(wire::TaskInviteBinding {
            key,
            task: &task,
            phase: &workflow.phase,
            generation: workflow.generation,
            inviter_lane: fixture.lane,
            inviter_occupant: address,
            inviter_claim: claim,
            recipient_lane: invitee,
        });
        send_peer(
            &bus,
            fixture.lane,
            invitee,
            Some(task.clone()),
            (key, Some(control.to_string())),
        )
    };
    let invited = invitation("1kKRRsDBx");
    let authority = bus.task_snapshot().expect("invitation snapshot reads");
    assert_eq!(peer_wakes(&authority, invited), Some(invited));

    end_claim(&bus, &task);
    let authority = bus.task_snapshot().expect("released snapshot reads");
    assert_eq!(
        peer_wakes(&authority, invited),
        None,
        "an invitation loses its wake with the claim that issued it",
    );
    finish(bus);
}

fn send_peer(
    bus: &Bus,
    from: CanonicalUuid,
    lane: CanonicalUuid,
    room: Option<TaskStamp>,
    (key, control): (&str, Option<String>),
) -> CanonicalUuid {
    let directive = CanonicalUuid::new(uuid::Uuid::now_v7());
    let at = now();
    let origin: Origin = "resident:test".parse().expect("fixture origin");
    let body = wire::body::SteerSent {
        directive,
        fanout: None,
        recipients: Vec::new(),
        key: key.to_owned(),
        recipient: Actor::Lane(lane),
        text: "coordinate".to_owned(),
        sender: Actor::Lane(from),
        room,
        control,
        attachments: Vec::new(),
        expires: shifted(at, 60),
    };
    let mut write = JournalWrite::new(Actor::Lane(from), origin.clone(), at, Producer::CommandDoor);
    let sent = write
        .fact(body.clone(), FactCause::Root)
        .expect("peer send builds");
    write.steering_head(sent, move |position| {
        steer::Ledger::sent_head(&body, origin, at, position).expect("peer steering head builds")
    });
    committed(bus.commit(write).expect("peer send commits"));
    directive
}

fn uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .expect("fixture uuid")
}
