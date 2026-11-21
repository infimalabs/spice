use super::*;
use std::collections::BTreeSet;

pub(super) static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);
pub(super) static NEXT_COMMAND: AtomicU64 = AtomicU64::new(100);

/// Complete one real worker pass before exercising a command that needs a
/// ready generation. No background scheduling or sleeps in board tests.
pub(super) fn warm_topics(root: &Path, bus: &Bus) -> std::sync::Arc<topic_tree::TopicTree> {
    let repo = Repository::discover(root).unwrap();
    let tree = topic_tree::TopicTree::shared(repo.admit_state().unwrap().data().to_owned());
    tree.update(&bus.task_snapshot().unwrap());
    tree
}

/// One occupied fixture seat, placed on a team of its own.
///
/// The team carries an identity distinct from the lane, the way an operator's
/// placement leaves it; allocation reads only the team's policy, so the
/// distinct identity matters to fleet rendering, not to what the seat draws.
#[derive(Clone, Debug)]
pub(super) struct Seat {
    pub(super) lane: CanonicalUuid,
    pub(super) team: CanonicalUuid,
    pub(super) occupant: CanonicalUuid,
}

pub(super) fn uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .expect("fixture UUID is canonical")
}

pub(super) fn completion_operation() -> CanonicalUuid {
    uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed))
}

/// Expire a fixture claim the way the resident's lease timer does.
///
/// The timer runs `board::lapse::lapse` inside one bus transaction with the
/// exact expiry the lease carries, so this is the production lapse rather than
/// a hand-written claim head.
pub(super) fn lapse_claim(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) {
    let held = board(bus, epoch, Actor::Lane(seat.lane));
    let expiry = at(&held, task)
        .latest_claim()
        .and_then(|held| held.lease.as_ref())
        .expect("a claim carries its lease")
        .body
        .expiry;
    let request = board::lapse::Request {
        command: completion_operation(),
        occupant: seat.occupant,
        epoch,
        expiry,
        at: expiry,
        intent: TaskRef { task: task.clone() },
    };
    let snapshot = validated_snapshot(bus, None, Actor::Spice).expect("lapse policy snapshot");
    let owners = snapshot.owners;
    let policy = snapshot.policy;
    let authority = journal::JournalGuard::new(move |_| policy.require_current());
    let lapsed = bus
        .transact(
            move |journal| match board::lapse::lapse(journal, request, &owners, authority) {
                Ok(lapsed) => {
                    let (first, last) = (lapsed.first(), lapsed.last());
                    bus::JournalTransaction::committed(Ok(lapsed), first, last)
                }
                Err(error) => bus::JournalTransaction::settled(Err(error)),
            },
        )
        .expect("the lapse reaches the journal writer");
    lapsed.expect("the expired review claim lapses");
    assert_eq!(
        bus.task_snapshot()
            .expect("lapsed snapshot reads")
            .task_head_update_visits(),
        1,
        "claim lapse updates one live head"
    );
}

/// The latest claim's exact witness, as its holder spends it.
pub(super) fn held_claim_witness(heads: &BoardTask) -> Witness {
    heads
        .latest_claim()
        .expect("the task is claimed")
        .witness(&heads.statement.body.born)
}

/// The latest claim's lease witness, as its holder spends it.
pub(super) fn held_lease_witness(heads: &BoardTask) -> Witness {
    heads
        .latest_claim()
        .and_then(|held| held.lease_witness(&heads.statement.body.born))
        .expect("the held claim carries a lease")
}

pub(super) fn stamp(text: &str) -> wire::TaskStamp {
    let value = text.bytes().fold(0_i64, |value, byte| {
        (value * 131 + i64::from(byte)) % 10_000_000_000_000
    });
    wire::TaskStamp::for_micros(value).expect("fixture name fits the task stamp codec")
}

/// One fixture's directory, removed again when the fixture goes out of scope.
///
/// A test that ends on `clean` removes it after verifying the authority; one
/// that ends on an assertion, or never reaches `clean`, still leaves nothing
/// behind in the temp directory. Removal is best effort so an assertion's
/// unwinding is what a failed test reports.
pub(super) struct FixtureRoot(PathBuf);

impl std::ops::Deref for FixtureRoot {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for FixtureRoot {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        POLICY_REPOSITORIES
            .lock()
            .expect("fixture policy registry")
            .retain(|_, root| root != &self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn fixture(
    name: &str,
    lane_count: usize,
) -> (FixtureRoot, Bus, CanonicalUuid, Vec<Seat>) {
    let number = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let root = FixtureRoot(std::env::temp_dir().join(format!(
        "spice-board-command-{name}-{}-{number}",
        std::process::id()
    )));
    fs::create_dir_all(root.join(".git")).expect("fixture repository exists");
    let path = root.join("authority.sqlite3");
    let journal = journal::Journal::open(&path).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    let epoch = uuid(number + 1);
    POLICY_REPOSITORIES
        .lock()
        .expect("fixture policy registry")
        .insert(epoch, root.0.clone());
    let seats = (0..lane_count)
        .map(|index| Seat {
            lane: uuid(number * 100 + index as u64 + 10),
            team: uuid(number * 100 + index as u64 + 70),
            occupant: uuid(number * 100 + index as u64 + 30),
        })
        .collect::<Vec<_>>();
    let lanes = seats
        .iter()
        .enumerate()
        .map(|(index, seat)| wire::LaneResidency {
            lane: seat.lane,
            team: seat.team,
            name: None,
            worktree: root.to_string_lossy().into_owned(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: Some(seat.occupant),
            lineage: vec![seat.occupant],
            capabilities: vec![uuid(number * 100 + index as u64 + 50)],
            session: None,
            disposition: "occupied".to_owned(),
        })
        .collect::<Vec<_>>();
    let subscriptions = lanes
        .iter()
        .map(|lane| wire::TeamSubscription {
            team: lane.team,
            formed_at: None,
            drive: true,
            drain: true,
            name: None,
            fast_mode: false,
        })
        .collect::<Vec<_>>();
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("resident fixture origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("current fixture instant");
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::CommandDoor);
    let seated = write
        .fact(body::UpSeated { epoch }, FactCause::Root)
        .expect("resident seating fact builds");
    write.residency_head(seated, move |_| ResidencyHead {
        epoch,
        state: ResidentState::Seated,
        topology_revision: 0,
        lanes,
        subscriptions,
        renewals: Vec::new(),
    });
    match bus.commit(write).expect("fixture seating reaches writer") {
        CommitOutcome::Committed(_) => {}
        outcome => panic!("fixture seating did not commit: {outcome:?}"),
    }
    (root, bus, epoch, seats)
}

pub(super) fn issue<B: Serialize>(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    kind: CommandKind,
    expected: Vec<Witness>,
    body: &B,
) -> Outcome<CommandAnswer> {
    let command = fixture_command(epoch, actor, kind, expected, body);
    dispatch(bus, &command).expect("board command owns fixture kind")
}

pub(super) fn fixture_command<B: Serialize>(
    epoch: CanonicalUuid,
    actor: Actor,
    kind: CommandKind,
    expected: Vec<Witness>,
    body: &B,
) -> wire::AdmittedCommand {
    admit_command(epoch, actor, kind, expected, body).expect("fixture command admits")
}

/// Admit one fixture command, returning the door's rejection unopened.
pub(super) fn admit_command<B: Serialize>(
    epoch: CanonicalUuid,
    actor: Actor,
    kind: CommandKind,
    expected: Vec<Witness>,
    body: &B,
) -> Result<wire::AdmittedCommand, wire::CommandRejection> {
    let roles = if actor == Actor::TEST_PRINCIPAL {
        wire::principal::PrincipalRoles::new([wire::principal::PrincipalHead {
            principal: actor
                .principal()
                .expect("the fixture operator actor is a principal"),
            born: seq(1),
            profile: wire::principal::PrincipalProfile::default(),
            roles: std::collections::BTreeSet::from([wire::principal::PrincipalRole::Operator]),
            capability: None,
        }])
    } else {
        wire::principal::PrincipalRoles::default()
    };
    Command {
        v: WireVersion::CURRENT,
        id: uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed)),
        epoch,
        kind,
        expected,
        body: serde_json::to_value(body).expect("fixture command body encodes"),
    }
    .admit(
        &Accepted {
            wire: WireVersion::CURRENT,
            actor,
            epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        &roles,
    )
}

pub(super) fn assert_committed(outcome: Outcome<CommandAnswer>) -> CommandAnswer {
    let Outcome::Reply(Reply {
        position: ReplyPosition::Committed(_),
        body,
    }) = outcome
    else {
        panic!("command did not commit: {outcome:?}");
    };
    body
}

pub(super) fn board(bus: &Bus, epoch: CanonicalUuid, actor: Actor) -> BoardAnswer {
    let outcome = issue(
        bus,
        epoch,
        actor,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::All { family: None },
    );
    let Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(position),
        body: CommandAnswer::Board(answer),
    }) = outcome
    else {
        panic!("board read did not return a snapshot: {outcome:?}");
    };
    assert_eq!(position, answer.cursor);
    *answer
}

/// The allocator's next-work answer for `actor`, as `spice task next` reads it.
pub(super) fn next_board(bus: &Bus, epoch: CanonicalUuid, actor: Actor) -> BoardAnswer {
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(answer),
        ..
    }) = issue(
        bus,
        epoch,
        actor,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Next,
    )
    else {
        panic!("next-work read does not return a board answer")
    };
    *answer
}

/// Wake `design` if deferred, then capture it for `seat` by name.
pub(super) fn capture_design(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, design: &TaskHandle) {
    wake_design(bus, epoch, design);
    super::capture_for(bus, epoch, seat, design);
}

/// Explicitly wake a deferred Design through its own workflow authority.
pub(super) fn wake_design(bus: &Bus, epoch: CanonicalUuid, design: &TaskHandle) {
    let current = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&current, design);
    if heads.workflow.body.deferral.is_some() {
        assert_committed(issue(
            bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            CommandKind::TaskWake,
            vec![workflow_witness(heads), deferral_witness(heads)],
            &TaskWake {
                task: design.clone(),
                claim: false,
            },
        ));
    }
}

/// The rows an Open read reports as blocked execution trouble, after proving
/// they are exactly the resident classifier's blocked partition.
pub(super) fn open_blocked(bus: &Bus, epoch: CanonicalUuid) -> BTreeSet<TaskStamp> {
    let (open, entries) = open_and_full(bus, epoch);
    let environment = wire::Environment::journal_schema(journal::SCHEMA_VERSION)
        .expect("compiled journal schema");
    let classified = board::work_waits(&entries, &environment)
        .into_iter()
        .filter(|(_, wait)| *wait == board::WorkWait::Blocked)
        .map(|(task, _)| task.clone())
        .collect::<BTreeSet<_>>();
    let blocked = open.blocked.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        blocked, classified,
        "open read blocked rows equal the resident classification"
    );
    blocked
}

/// The rows an Open read reports as normal Plan/Design workflow waits, after
/// proving they are exactly the resident classifier's neutral partition.
pub(super) fn open_workflow_waiting(bus: &Bus, epoch: CanonicalUuid) -> BTreeSet<TaskStamp> {
    let (open, entries) = open_and_full(bus, epoch);
    let environment = wire::Environment::journal_schema(journal::SCHEMA_VERSION)
        .expect("compiled journal schema");
    let classified = board::work_waits(&entries, &environment)
        .into_iter()
        .filter(|(_, wait)| *wait == board::WorkWait::Workflow)
        .map(|(task, _)| task.clone())
        .collect::<BTreeSet<_>>();
    let waiting = open
        .workflow_waiting
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        waiting, classified,
        "open read workflow waits equal the resident classification"
    );
    waiting
}

/// Every open row whose allocator gate remains closed, independent of its
/// mutually exclusive presentation category.
pub(super) fn open_gated(bus: &Bus, epoch: CanonicalUuid) -> BTreeSet<TaskStamp> {
    let (open, _) = open_and_full(bus, epoch);
    open.blocked
        .into_iter()
        .chain(open.workflow_waiting)
        .chain(
            open.design_cycles
                .into_iter()
                .filter(|(_, cycle)| *cycle == wire::rooms::DesignCycle::AwaitingDecision)
                .map(|(task, _)| task),
        )
        .collect()
}

/// The Designs an Open read reports awaiting selection, after proving they are
/// exactly the resident classifier's selection partition.
pub(super) fn open_awaiting_selection(bus: &Bus, epoch: CanonicalUuid) -> BTreeSet<TaskStamp> {
    let (open, entries) = open_and_full(bus, epoch);
    let environment = wire::Environment::journal_schema(journal::SCHEMA_VERSION)
        .expect("compiled journal schema");
    let classified = board::work_waits(&entries, &environment)
        .into_iter()
        .filter(|(_, wait)| *wait == board::WorkWait::Selection)
        .map(|(task, _)| task.clone())
        .collect::<BTreeSet<_>>();
    let awaiting = open
        .design_cycles
        .into_iter()
        .filter(|(_, cycle)| *cycle == wire::rooms::DesignCycle::AwaitingDecision)
        .map(|(task, _)| task)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        awaiting, classified,
        "open read selection waits equal the resident classification"
    );
    awaiting
}

fn open_and_full(bus: &Bus, epoch: CanonicalUuid) -> (BoardAnswer, Vec<Entry>) {
    let outcome = issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Open {
            recent_finished: 0,
            after: None,
        },
    );
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(open),
        ..
    }) = outcome
    else {
        panic!("open board read did not answer: {outcome:?}");
    };
    let full = board(bus, epoch, Actor::TEST_PRINCIPAL);
    (
        (*open),
        actor_entries(&full).expect("full board entries validate"),
    )
}

pub(super) fn add(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    task: &str,
    title: &str,
    flow: Option<&str>,
) -> TaskHandle {
    add_task_in_topic(bus, epoch, actor, task, title, "/board/test", flow)
}

/// Add one ordinary task under its named flow, or `todo,review`, carrying the
/// one acceptance criterion every successful completion needs.
pub(super) fn add_task_in_topic(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    task: &str,
    title: &str,
    topic: &str,
    flow: Option<&str>,
) -> TaskHandle {
    issue_add(
        bus,
        epoch,
        actor,
        task,
        authored_add(
            title,
            topic,
            flow.or(Some("todo,review")),
            &["the fixture work is verified"],
        ),
    )
}

/// An admitted authored creation cause for fixtures that need any one.
pub(super) fn fixture_origin() -> wire::Origin {
    "ack:1kJK6N3Rx".parse().expect("fixture origin is valid")
}

/// Add one operator-authored task carrying `term`; without items the
/// flow defaults to the ordinary two phases.
pub(super) fn add_task_with_term(
    bus: &Bus,
    epoch: CanonicalUuid,
    task: &str,
    title: &str,
    topic: &str,
    flow: Option<&str>,
    term: &[&str],
) -> TaskHandle {
    issue_add(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        task,
        authored_add(title, topic, flow, term),
    )
}

pub(super) fn authored_add(title: &str, topic: &str, flow: Option<&str>, term: &[&str]) -> TaskAdd {
    TaskAdd {
        topic: Some(topic.parse().expect("fixture topic")),
        term: term.iter().map(|item| (*item).to_owned()).collect(),
        origin: Some(fixture_origin()),
        flow: flow
            .or_else(|| term.is_empty().then_some("todo,review"))
            .map(str::to_owned),
        ..adding(title.to_owned(), "This work is enough.")
    }
}

pub(super) fn issue_add(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    task: &str,
    add: TaskAdd,
) -> TaskHandle {
    let born = stamp(task);
    let Some(wire::TopicInput::Absolute(topic)) = &add.topic else {
        panic!("authored fixture names its public topic");
    };
    let task = TaskHandle::for_topic(topic, &born);
    assert_committed(issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &add,
    ));
    task
}
