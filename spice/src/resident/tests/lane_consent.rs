//! A seated occupant's confirm dialog is decided by policy, recorded at the
//! lane's origin, and answered through the lane's kick door; what is still
//! pending lapses only at an explicit break.

use super::*;
use supervisor::command::Kick;

/// One seated lane with a policy that allows deploys, denies drops, and
/// queues everything else, listening at its own kick door.
struct LaneFixture {
    root: PathBuf,
    paths: RuntimePaths,
    bus: Bus,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    policy: consent::Policy,
    reads: read_materializations::CurrentReads,
    residency: journal::JournalHead<wire::ResidencyHead>,
    door: door::Door,
}

impl LaneFixture {
    fn start() -> Self {
        let (root, paths) = resident_fixture("lane-consent");
        let repository = Repository::discover(&root).expect("fixture repository resolves");
        let surface =
            crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
        let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
        let bus = Bus::start(journal).expect("fixture bus starts");
        let epoch = test_uuid(1);
        commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");
        let lane = test_uuid(2);
        let occupant = test_uuid(3);
        seed_lane(&bus, epoch, lane, occupant, &root);
        let policy = consent::Policy::new(
            consent::Answer::Queue,
            vec![
                consent::Rule::new("deploys", "Deploy*", consent::Answer::Allow, None)
                    .expect("fixture rule"),
                consent::Rule::new("never", "Drop*", consent::Answer::Deny, None)
                    .expect("fixture rule"),
            ],
        )
        .expect("fixture policy");
        let reads = read_materializations::CurrentReads::start(&bus, &paths, &policy)
            .expect("consent materializer starts");
        let door_path = supervisor::command::kick_path(&paths, lane);
        let door = door::Door::bind(&door_path, 256).expect("the lane's kick door binds");
        let residency = current_residency(&bus, epoch).expect("current residency");
        Self {
            root,
            paths,
            bus,
            lane,
            occupant,
            policy,
            reads,
            residency,
            door,
        }
    }

    /// Ask as `occupant` and return what the diagnostic said about it.
    fn ask(&self, occupant: CanonicalUuid, request: u8, operation: &str) -> String {
        let mut diagnostic = Vec::new();
        consent_surface::ask(
            &self.bus,
            &self.reads,
            &self.paths,
            &self.policy,
            self.residency.body(),
            consent_surface::LaneAsk {
                lane: self.lane,
                occupant,
                request: test_uuid(request),
                operation: operation.to_owned(),
            },
            &mut diagnostic,
        )
        .expect("a lane ask never faults the resident");
        String::from_utf8(diagnostic).expect("diagnostic is text")
    }

    /// The kick waiting at the door, if any.
    fn answered(&self) -> Option<Kick> {
        self.door
            .recv(Some(std::time::Duration::ZERO))
            .ok()
            .map(|kick| serde_json::from_slice(&kick).expect("a kick"))
    }

    fn consent_facts(&self) -> Vec<wire::Envelope<wire::AnyFact>> {
        let snapshot = self
            .bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read through bus");
        snapshot
            .facts()
            .iter()
            .filter(|fact| consent::Queue::observes(fact.fact.kind()))
            .cloned()
            .collect()
    }

    fn stop(self) {
        drop(self.door);
        drop(self.reads);
        self.bus.shutdown().expect("fixture bus stops");
        fs::remove_dir_all(self.root).expect("lane consent fixture cleanup");
    }
}

#[test]
fn a_lane_ask_is_answered_at_once_by_a_rule_or_waits_on_the_operator() {
    let lane = LaneFixture::start();
    let occupant = lane.occupant;

    assert_eq!(lane.ask(occupant, 11, "Deploy: to prod?"), "");
    assert_eq!(
        lane.answered(),
        Some(Kick::Answer {
            request: test_uuid(11),
            confirmed: true,
        }),
        "a rule that allows answers the dialog at once"
    );
    assert_eq!(lane.ask(occupant, 12, "Drop: the table?"), "");
    assert_eq!(
        lane.answered(),
        Some(Kick::Answer {
            request: test_uuid(12),
            confirmed: false,
        })
    );
    assert_eq!(lane.ask(occupant, 13, "Rename: it?"), "");
    assert_eq!(
        lane.answered(),
        None,
        "a queued request waits on the operator"
    );
    let generation = lane.reads.current().expect("consent pages settle");
    assert!(generation.consent().pending(test_uuid(13)).is_some());

    let seen = lane.ask(occupant, 11, "Deploy: again?");
    assert!(seen.contains("consent::request-seen"), "{seen}");
    assert_eq!(
        lane.answered(),
        None,
        "a seen identity is refused, not re-answered"
    );
    lane.ask(test_uuid(4), 14, "Deploy: from a closed seat?");
    assert_eq!(lane.answered(), None);

    lane.stop();
}

#[test]
fn a_lane_ask_and_its_outcome_are_one_pair_at_the_lane_origin() {
    let lane = LaneFixture::start();
    let occupant = lane.occupant;

    assert_eq!(lane.ask(occupant, 11, "Deploy: to prod?"), "");
    assert_eq!(lane.ask(occupant, 12, "Drop: the table?"), "");
    assert_eq!(lane.ask(occupant, 13, "Rename: it?"), "");

    let consent_facts = lane.consent_facts();
    assert_eq!(
        consent_facts
            .iter()
            .map(|fact| fact.fact.kind())
            .collect::<Vec<_>>(),
        [
            wire::FactKind::ConsentAsked,
            wire::FactKind::ConsentGranted,
            wire::FactKind::ConsentAsked,
            wire::FactKind::ConsentDenied,
            wire::FactKind::ConsentAsked,
            wire::FactKind::ConsentQueued,
        ]
    );
    for pair in consent_facts.chunks(2) {
        let (asked, outcome) = (&pair[0], &pair[1]);
        assert_eq!(asked.cause, None);
        assert_eq!(outcome.cause, Some(asked.seq));
        assert_eq!(asked.origin.as_str(), format!("lane:{}", lane.lane));
        assert_eq!(outcome.origin, asked.origin);
        assert_eq!(asked.actor, Actor::Lane(lane.lane));
    }

    lane.stop();
}

#[test]
fn a_break_lapses_what_the_lane_still_has_pending_and_nothing_ruled() {
    let lane = LaneFixture::start();
    let occupant = lane.occupant;

    assert_eq!(lane.ask(occupant, 11, "Deploy: to prod?"), "");
    assert_eq!(lane.ask(occupant, 13, "Rename: it?"), "");
    assert_eq!(lane.ask(occupant, 15, "Merge: it?"), "");
    consent_surface::commit_lapse(&lane.bus, &lane.reads, test_uuid(4))
        .expect("another lane has nothing to lapse");
    consent_surface::commit_lapse(&lane.bus, &lane.reads, lane.lane)
        .expect("the break lapses the lane's pending requests");
    consent_surface::commit_lapse(&lane.bus, &lane.reads, lane.lane)
        .expect("a second break finds nothing pending");

    let generation = lane.reads.current().expect("consent pages settle");
    assert_eq!(generation.consent().pending_of(lane.lane).count(), 0);
    let consent_facts = lane.consent_facts();
    let lapsed = consent_facts
        .iter()
        .filter(|fact| fact.fact.kind() == wire::FactKind::ConsentLapsed)
        .collect::<Vec<_>>();
    let queued = consent_facts
        .iter()
        .filter(|fact| fact.fact.kind() == wire::FactKind::ConsentQueued)
        .map(|fact| fact.seq)
        .collect::<Vec<_>>();
    assert_eq!(
        lapsed.iter().map(|fact| fact.cause).collect::<Vec<_>>(),
        queued.into_iter().map(Some).collect::<Vec<_>>(),
        "each lapse points at the queue fact it closes"
    );
    assert_eq!(lapsed.len(), 2, "the granted request was ruled, not lapsed");
    assert!(
        lapsed.iter().all(|fact| fact.actor == Actor::Spice
            && fact.origin.as_str() == format!("lane:{}", lane.lane)),
        "a lapse is the resident's recognition, closing the request's own thread"
    );
    assert_eq!(
        lapsed[0].seq.get() + 1,
        lapsed[1].seq.get(),
        "one break is one commit"
    );

    lane.stop();
}
