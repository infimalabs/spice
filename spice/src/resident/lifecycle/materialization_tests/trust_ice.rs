use std::fs;

use lane::capture::CaptureExit;
use lane::occupant::Seats;

use super::super::reconcile_retained_trust;
use super::reaped_seat::{fact_kinds, fixture_root, residency_row, session};
use super::*;
use crate::resident::supervisor::test_support::record_exited_for_tests;
use crate::resident::supervisor::{Adoption, adopt};
use crate::resident::{RuntimePaths, lane_surface};

#[test]
fn iced_claim_suppresses_work_wakes_but_not_operator_direction_and_grant_thaws_held_work() {
    let digest = digest();
    let (bus, mut projection) = iced_fixture(&digest);
    let snapshot = bus.task_snapshot().expect("iced snapshot reads");
    let iced_target = target(&snapshot, &projection);
    assert!(!iced_target.pending_inbox);
    assert_eq!(iced_target.held_claim, Available::None);
    assert_eq!(iced_target.drain_queue, Available::None);
    assert_eq!(
        resolve(&[iced_target], &Backoff::default(), instant(50),),
        Resolution::Idle
    );

    committed(bus.commit(grant_write(&digest)).expect("grant commits"));
    let granted = bus.task_snapshot().expect("granted snapshot reads");
    projection
        .advance(&granted)
        .expect("grant projection advances");
    assert_eq!(
        resolve(
            &[target(&granted, &projection)],
            &Backoff::default(),
            instant(50),
        ),
        Resolution::Attempt {
            trigger: Trigger::HeldClaim,
            lane: lane(),
        }
    );
    bus.shutdown().expect("grant fixture stops");

    let (bus, mut projection) = iced_fixture(&digest);
    committed(
        bus.commit(operator_direction_write())
            .expect("operator direction commits"),
    );
    let directed = bus.task_snapshot().expect("directed snapshot reads");
    projection
        .advance(&directed)
        .expect("direction projection advances");
    assert_eq!(
        resolve(
            &[target(&directed, &projection)],
            &Backoff::default(),
            instant(50),
        ),
        Resolution::Attempt {
            trigger: Trigger::PendingInbox,
            lane: lane(),
        },
        "message-driven wake remains available while work-driven wake is iced"
    );
    bus.shutdown().expect("direction fixture stops");
}

#[test]
fn each_park_cause_reaches_the_lane_read_at_the_decision_and_clears_when_it_ends() {
    use wire::body::LanePark;

    let digest = digest();
    let (bus, mut projection) = iced_fixture(&digest);
    let root = fixture_root("parks");
    let paths = super::reaped_seat::runtime_paths(&root);
    let mut runtime = super::super::Runtime::new(&lane::Config::default());
    let journal = |runtime: &super::super::Runtime, targets: &[Target]| {
        let snapshot = bus.task_snapshot().expect("park snapshot reads");
        let mut diagnostic = Vec::new();
        let mut parks = targets
            .iter()
            .map(|target| (target.lane, target.park))
            .collect();
        runtime.reconcile_job_parks(&paths, &snapshot, instant(50), &mut parks);
        runtime.journal_parks(&bus, epoch(), &snapshot, &parks, &mut diagnostic);
        assert!(
            diagnostic.is_empty(),
            "the park commits: {}",
            String::from_utf8_lossy(&diagnostic)
        );
        let (_, read) = lane_surface::current_answer(&bus, &paths, epoch(), &Seats::new())
            .expect("the lane read projects");
        read.lanes[0].parked
    };

    let iced = target(
        &bus.task_snapshot().expect("iced snapshot reads"),
        &projection,
    );
    assert_eq!(journal(&runtime, &[iced]), Some(LanePark::Authority));
    runtime.backoff.parked.insert(lane(), Trigger::HeldClaim);
    assert_eq!(
        journal(&runtime, &[iced]),
        Some(LanePark::Refused),
        "a refused start replaces the park reason"
    );
    runtime.backoff.release(lane());

    committed(bus.commit(grant_write(&digest)).expect("grant commits"));
    let granted = bus.task_snapshot().expect("granted snapshot reads");
    projection
        .advance(&granted)
        .expect("grant projection advances");
    let held = target(&granted, &projection);
    let mut waiting = [held];
    super::super::await_regular_jobs(&mut waiting, instant(50), |_| Some(instant(59)));
    assert_eq!(
        journal(&runtime, &waiting),
        Some(LanePark::Job { until: instant(59) }),
        "a continuation deferred behind a finite job parks at once"
    );
    let unchanged = fact_kinds(&bus).len();
    assert_eq!(
        journal(&runtime, &waiting),
        Some(LanePark::Job { until: instant(59) })
    );
    assert_eq!(
        fact_kinds(&bus).len(),
        unchanged,
        "an unchanged park journals nothing"
    );
    let job = crate::resident::supervisor::terminal::Jobs::for_lane(&paths, lane())
        .record_for_tests("proof matrix", None, Some(600_000));
    assert_eq!(
        journal(&runtime, &[]),
        Some(LanePark::Job { until: instant(59) }),
        "a later reconciliation with no target does not erase a running job's park"
    );
    job.exit_for_tests();
    assert_eq!(
        journal(&runtime, &[]),
        None,
        "the park ends when the job ends"
    );
    bus.shutdown().expect("park fixture stops");
    fs::remove_dir_all(root).expect("park fixture cleanup");
}

#[test]
fn a_turn_end_parks_held_work_on_its_ungranted_configuration_until_the_grant_lands() {
    let digest = digest();
    let (bus, mut projection) = policy_fixture(true, false);
    assert!(
        reconcile(&bus, PRODUCER, Some(&digest)).is_none(),
        "a lane without a claim parks nothing"
    );
    assert_eq!(trust(&bus, &digest), None, "nor pends for it");
    // A finite lease that ends before the wake decision below: only the ice
    // keeps the claim held, so the grant can wake it.
    committed(
        bus.commit(claim_write_until(&bus, instant(3), instant(40), |_, _| {}))
            .expect("claim commits"),
    );
    assert!(
        reconcile(&bus, PRODUCER, None).is_none(),
        "a configuration without pre-commit commands parks nothing"
    );

    let parked_commit =
        reconcile(&bus, PRODUCER, Some(&digest)).expect("the first trust park commits");
    assert_eq!(
        parked_commit.last() - parked_commit.first() + 1,
        2,
        "the pending trust fact and task requirement are one journal transaction"
    );
    assert_eq!(
        trust(&bus, &digest),
        Some((wire::TrustState::Pended, Some(PRODUCER.to_owned()))),
        "an unseen digest is pended for the operator"
    );
    assert_eq!(required(&bus), Some(digest.clone()));
    let pends = pended_count(&bus);
    assert!(
        reconcile(&bus, PRODUCER, Some(&digest)).is_none(),
        "a repeated turn end records nothing more"
    );
    assert_eq!(pended_count(&bus), pends);
    let parked = bus.task_snapshot().expect("parked snapshot reads");
    let lease = parked.tasks()[0]
        .latest_claim()
        .and_then(|held| held.lease())
        .expect("held lease")
        .body();
    assert!(spice::claim_lease::is_indefinite(lease));
    assert_eq!(lease.start, instant(3));
    projection
        .advance(&parked)
        .expect("parked projection advances");
    assert_eq!(wake(&parked, &projection), Resolution::Idle);

    committed(bus.commit(grant_write(&digest)).expect("grant commits"));
    let granted = bus.task_snapshot().expect("granted snapshot reads");
    projection
        .advance(&granted)
        .expect("grant projection advances");
    assert_eq!(wake(&granted, &projection), held_wake());
    assert!(
        reconcile(&bus, PRODUCER, Some(&digest)).is_none(),
        "a granted configuration changes nothing"
    );
    bus.verify().expect("the parked authority verifies");
    bus.shutdown().expect("parking fixture stops");
    let (bus, _) = policy_fixture(true, false);
    committed(
        bus.commit(pend_write(&digest, PRODUCER, instant(2)))
            .expect("existing pend commits"),
    );
    committed(
        bus.commit(claim_write_until(&bus, instant(3), instant(40), |_, _| {}))
            .expect("claim over existing pend commits"),
    );
    let pends = pended_count(&bus);
    let attached = reconcile(&bus, PRODUCER, Some(&digest))
        .expect("an already-pending digest attaches to the task");
    assert_eq!(
        attached.last() - attached.first() + 1,
        1,
        "attaching an already-pending digest writes only the task edit"
    );
    assert_eq!(
        pended_count(&bus),
        pends,
        "attachment does not duplicate the pending fact"
    );
    assert_eq!(required(&bus), Some(digest.clone()));
    bus.verify().expect("existing-pend authority verifies");
    bus.shutdown().expect("existing-pend fixture stops");

    assert_occupant_exit_reconciles_trust_park();
}

#[test]
fn a_dormant_grant_admits_new_work_without_joining_the_live_projection() {
    let digest = digest();
    let (bus, _) = policy_fixture(true, false);
    committed(
        bus.commit(pend_write(&digest, PRODUCER, instant(2)))
            .expect("trust pend commits"),
    );
    committed(
        bus.commit(grant_write(&digest))
            .expect("trust grant commits"),
    );
    assert!(
        bus.task_snapshot()
            .expect("dormant grant snapshot reads")
            .trust()
            .is_empty(),
        "history alone does not grow live task authority"
    );
    committed(
        bus.commit(claim_write_until(&bus, instant(3), instant(40), |_, _| {}))
            .expect("claim commits"),
    );
    assert!(
        reconcile(&bus, PRODUCER, Some(&digest)).is_none(),
        "the exact durable grant admits a newly observed need"
    );
    assert_eq!(required(&bus), None, "the task does not park on that grant");
    bus.verify().expect("dormant grant authority verifies");
    bus.shutdown().expect("parking fixture stops");
}

/// A retained seat's turn ends without its occupant exiting, so the settlement
/// that would continue the claim must park it on the ungranted configuration
/// its worktree needs, and a parked claim's next settlement records nothing.
#[test]
fn a_retained_turn_end_parks_its_held_claim_instead_of_continuing_it() {
    let root = fixture_root("trust-park-retained");
    fs::write(
        root.join(wire::TRACKED_POLICY_FILE),
        "[policy]\npre_commit = [{ label = 'exact', run = ['/bin/true'] }]\n",
    )
    .expect("executable configuration writes");
    let root = fs::canonicalize(root).expect("trust fixture root canonicalizes");
    let repository =
        spice::repository::Repository::discover(&root).expect("trust fixture repository resolves");
    let expected = crate::trust_command::Surface::load(&repository)
        .expect("trust surface loads")
        .digest()
        .clone();
    let (bus, _) = policy_fixture(true, false);
    committed(
        bus.commit(claim_write_until(&bus, instant(3), instant(40), |_, _| {}))
            .expect("claim commits"),
    );
    committed(
        bus.commit(occupy_write(&bus, &root, uuid(82)))
            .expect("occupied residency commits"),
    );
    let before = fact_kinds(&bus).len();
    let mut diagnostic = Vec::new();
    let snapshot = bus.task_snapshot().expect("retained snapshot reads");
    assert!(
        reconcile_retained_trust(&bus, &snapshot, lane(), &mut diagnostic),
        "{}",
        String::from_utf8_lossy(&diagnostic)
    );
    assert_eq!(
        fact_kinds(&bus)[before..],
        [wire::FactKind::TrustPended, wire::FactKind::TaskEdited]
    );
    assert_eq!(required(&bus), Some(expected));
    let parked = bus.task_snapshot().expect("parked snapshot reads");
    assert!(!reconcile_retained_trust(
        &bus,
        &parked,
        lane(),
        &mut diagnostic
    ));
    assert!(
        diagnostic.is_empty(),
        "{}",
        String::from_utf8_lossy(&diagnostic)
    );
    bus.verify().expect("retained trust authority verifies");
    bus.shutdown().expect("retained trust fixture stops");
    fs::remove_dir_all(root).expect("retained trust fixture cleans up");
}

#[test]
fn a_turn_end_drops_a_park_its_worktree_no_longer_needs() {
    let (abandoned, granted) = (digest(), other_digest());
    let (bus, mut projection) = policy_fixture(true, false);
    committed(
        bus.commit(claim_write_until(&bus, instant(3), instant(40), |_, _| {}))
            .expect("claim commits"),
    );
    assert!(reconcile(&bus, PRODUCER, Some(&abandoned)).is_some());
    committed(bus.commit(grant_write(&granted)).expect("grant commits"));

    let before = super::reaped_seat::fact_kinds(&bus).len();
    assert!(
        reconcile(&bus, PRODUCER, Some(&granted)).is_some(),
        "a configuration backed out to a granted digest drops the park"
    );
    assert_eq!(required(&bus), None);
    assert_eq!(
        super::reaped_seat::fact_kinds(&bus)[before..],
        [wire::FactKind::TaskEdited],
        "the drop writes no steering input"
    );
    let dropped = bus.task_snapshot().expect("dropped snapshot reads");
    projection
        .advance(&dropped)
        .expect("dropped projection advances");
    assert_eq!(wake(&dropped, &projection), held_wake());

    assert!(reconcile(&bus, PRODUCER, Some(&abandoned)).is_some());
    assert_eq!(required(&bus), Some(abandoned.clone()));
    assert!(
        reconcile(&bus, PRODUCER, None).is_some(),
        "a configuration that no longer needs trust drops the park"
    );
    assert_eq!(required(&bus), None);
    bus.verify().expect("the dropped authority verifies");
    bus.shutdown().expect("drop fixture stops");
}

#[test]
fn a_turn_end_moves_its_task_request_whichever_worktree_pended_the_digest() {
    let (older, newer) = (digest(), other_digest());
    let (bus, _) = policy_fixture(true, false);
    committed(
        bus.commit(held_claim_write(&bus, instant(3), |_, _| {}))
            .expect("claim commits"),
    );
    assert!(reconcile(&bus, PRODUCER, Some(&older)).is_some());
    assert!(reconcile(&bus, PRODUCER, Some(&newer)).is_some());
    assert_eq!(required(&bus), Some(newer.clone()));
    assert_eq!(
        trust(&bus, &newer),
        Some((wire::TrustState::Pended, Some(PRODUCER.to_owned()))),
        "the replacement digest remains independently reviewable"
    );
    bus.shutdown().expect("replacement fixture stops");

    let (bus, _) = policy_fixture(true, false);
    committed(
        bus.commit(pend_write(&older, "/other/worktree", instant(2)))
            .expect("foreign pend commits"),
    );
    committed(
        bus.commit(held_claim_write(&bus, instant(3), |_, _| {}))
            .expect("claim commits"),
    );
    assert!(
        reconcile(&bus, PRODUCER, Some(&older)).is_some(),
        "a digest another worktree pended still parks this task"
    );
    assert_eq!(required(&bus), Some(older.clone()));
    assert_eq!(pended_count(&bus), 1, "the shared digest is pended once");
    assert_eq!(
        trust(&bus, &older),
        Some((wire::TrustState::Pended, Some("/other/worktree".to_owned()))),
        "the first producer stays provenance"
    );
    assert!(reconcile(&bus, PRODUCER, Some(&newer)).is_some());
    assert_eq!(required(&bus), Some(newer.clone()));
    assert!(
        reconcile(&bus, PRODUCER, None).is_some(),
        "a request the worktree no longer needs expires"
    );
    assert_eq!(required(&bus), None);
    bus.verify().expect("the moved request verifies");
    bus.shutdown().expect("foreign fixture stops");
}

#[test]
fn a_moved_surface_writes_nothing_and_a_retry_parks_only_the_current_digest() {
    let root = fixture_root("trust-park-surface-move");
    let policy = root.join(wire::TRACKED_POLICY_FILE);
    fs::write(
        &policy,
        "[policy]\npre_commit = [{ label = 'before', run = ['/bin/true'] }]\n",
    )
    .expect("first executable configuration writes");
    let root = fs::canonicalize(root).expect("trust fixture root canonicalizes");
    let worktree = root.to_str().expect("fixture root is UTF-8").to_owned();
    let repository =
        spice::repository::Repository::discover(&root).expect("trust fixture repository resolves");
    let stale_surface =
        crate::trust_command::Surface::load(&repository).expect("first trust surface loads");
    let stale = stale_surface.digest().clone();
    let (bus, _) = policy_fixture(true, false);
    committed(
        bus.commit(claim_write_until(&bus, instant(3), instant(40), |_, _| {}))
            .expect("claim commits"),
    );

    fs::write(
        &policy,
        "[policy]\npre_commit = [{ label = 'after', run = ['/bin/true'] }]\n",
    )
    .expect("second executable configuration writes");
    let current = crate::trust_command::Surface::load(&repository)
        .expect("second trust surface loads")
        .digest()
        .clone();
    assert_ne!(stale, current);
    let before = fact_kinds(&bus);
    assert!(
        lane_surface::reconcile_trust_park_with_surface(
            &bus,
            lane(),
            &worktree,
            repository,
            stale_surface,
            true,
            instant(10),
        )
        .is_err(),
        "journal admission refuses a captured surface that moved"
    );
    assert_eq!(
        fact_kinds(&bus),
        before,
        "the refused admission writes no fact or head"
    );
    assert_eq!(trust(&bus, &stale), None);
    assert_eq!(required(&bus), None);

    let committed = lane_surface::reconcile_trust_park(&bus, lane(), &worktree, instant(11))
        .expect("the stable retry decides")
        .expect("the stable retry parks the task");
    assert_eq!(committed.last() - committed.first() + 1, 2);
    assert_eq!(trust(&bus, &stale), None);
    assert_eq!(
        trust(&bus, &current),
        Some((wire::TrustState::Pended, Some(worktree))),
    );
    assert_eq!(required(&bus), Some(current));
    let parked = bus.task_snapshot().expect("parked snapshot reads");
    let lease = parked.tasks()[0]
        .latest_claim()
        .and_then(|held| held.lease())
        .expect("held lease")
        .body();
    assert!(spice::claim_lease::is_indefinite(lease));
    bus.verify().expect("surface guard authority verifies");
    bus.shutdown().expect("surface guard fixture stops");
    fs::remove_dir_all(root).expect("surface guard fixture cleans up");
}

/// Worktree `grant_write` names as the digest's producer.
const PRODUCER: &str = "/producer/worktree";

fn reconcile(bus: &Bus, worktree: &str, need: Option<&wire::Digest>) -> Option<bus::JournalCommit> {
    lane_surface::reconcile_trust_park_with_need(bus, lane(), worktree, need, instant(10))
        .expect("turn-end reconciliation decides")
}
fn assert_occupant_exit_reconciles_trust_park() {
    let root = fixture_root("trust-park-exit");
    fs::write(
        root.join(wire::TRACKED_POLICY_FILE),
        "[policy]\npre_commit = [{ label = 'exact', run = ['/bin/true'] }]\n",
    )
    .expect("executable configuration writes");
    let root = fs::canonicalize(root).expect("trust fixture root canonicalizes");
    let repository =
        spice::repository::Repository::discover(&root).expect("trust fixture repository resolves");
    let expected = crate::trust_command::Surface::load(&repository)
        .expect("trust surface loads")
        .digest()
        .clone();
    let paths = RuntimePaths::discover(&root).expect("trust fixture runtime paths resolve");
    let occupant = uuid(81);
    let (bus, _) = policy_fixture(true, false);
    committed(
        bus.commit(claim_write_until(&bus, instant(3), instant(40), |_, _| {}))
            .expect("claim commits"),
    );
    committed(
        bus.commit(occupy_write(&bus, &root, occupant))
            .expect("occupied residency commits"),
    );
    record_exited_for_tests(&paths, lane(), occupant, session(&root, occupant))
        .expect("supervisor exit record writes");
    let row = residency_row(&bus);
    let Some(Adoption::Exited(launch)) = adopt(&paths, &row).expect("published state adopts")
    else {
        panic!("published exit was not adoptable");
    };
    let mut seats = Seats::new();
    seats
        .seat_detached(lane(), occupant, launch.process)
        .expect("published occupant seats");
    let before = fact_kinds(&bus).len();
    let mut diagnostic = Vec::new();
    lane_surface::occupant_exited(
        &bus,
        &paths,
        &steer::Config::default(),
        (
            epoch(),
            crate::board_command::JoiningFixture::discover(&root).joining(),
        ),
        &mut seats,
        CaptureExit {
            lane: lane(),
            occupant,
        },
        &mut diagnostic,
    )
    .expect("occupant exit reconciles");
    assert_eq!(
        fact_kinds(&bus)[before..],
        [
            wire::FactKind::LaneExited,
            wire::FactKind::TrustPended,
            wire::FactKind::TaskEdited,
        ],
        "the actual exit path vacates the seat, then atomically parks its held task"
    );
    assert_eq!(
        trust(&bus, &expected).map(|(state, _)| state),
        Some(wire::TrustState::Pended)
    );
    assert_eq!(required(&bus), Some(expected));
    bus.verify().expect("exit trust authority verifies");
    bus.shutdown().expect("exit trust fixture stops");
    fs::remove_dir_all(root).expect("exit trust fixture cleans up");
}

fn occupy_write(bus: &Bus, root: &std::path::Path, occupant: CanonicalUuid) -> JournalWrite {
    let current = bus
        .residency_head()
        .expect("residency reads")
        .expect("residency exists");
    let mut next = current.body().clone();
    let row = next
        .lanes
        .iter_mut()
        .find(|row| row.lane == lane())
        .expect("fixture lane exists");
    row.worktree = root.to_string_lossy().into_owned();
    row.occupant = Some(occupant);
    row.lineage.push(occupant);
    row.session = Some(session(root, occupant));
    row.disposition = "occupied".to_owned();
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("resident:{}", epoch())
            .parse()
            .expect("resident origin"),
        instant(4),
        Producer::ResidentRecognition,
    );
    let started = write
        .fact(
            wire::body::LaneStarted {
                lane: lane(),
                occupant,
                epoch: epoch(),
                group: 1,
                fresh_session: true,
            },
            FactCause::Root,
        )
        .expect("lane start builds");
    write.residency_head(started, move |_| next);
    write
}

fn trust(bus: &Bus, digest: &wire::Digest) -> Option<(wire::TrustState, Option<String>)> {
    bus.trust_head(digest)
        .expect("trust head reads")
        .map(|head| (head.body().state, head.body().worktree.clone()))
}

fn pended_count(bus: &Bus) -> usize {
    super::reaped_seat::fact_kinds(bus)
        .into_iter()
        .filter(|kind| *kind == wire::FactKind::TrustPended)
        .count()
}

fn wake(snapshot: &bus::TaskSnapshot, projection: &Projection) -> Resolution {
    resolve(
        &[target(snapshot, projection)],
        &Backoff::default(),
        instant(50),
    )
}

fn held_wake() -> Resolution {
    Resolution::Attempt {
        trigger: Trigger::HeldClaim,
        lane: lane(),
    }
}

fn required(bus: &Bus) -> Option<wire::Digest> {
    let snapshot = bus.task_snapshot().expect("requirement snapshot reads");
    snapshot.tasks()[0]
        .statement()
        .body()
        .requirements
        .as_slice()
        .iter()
        .find_map(|requirement| match requirement {
            wire::EnvironmentRequirement::TrustGrant { value } => Some(value.clone()),
            _ => None,
        })
}

fn pend_write(digest: &wire::Digest, worktree: &str, at: Instant) -> JournalWrite {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "repo:genesis".parse().expect("fixture origin"),
        at,
        Producer::CommandDoor,
    );
    let pended = write
        .fact(
            body::TrustPended {
                surface: "spice".to_owned(),
                digest: digest.clone(),
                worktree: Some(worktree.to_owned()),
                previous: None,
                documents: None,
            },
            FactCause::Root,
        )
        .expect("trust pend builds");
    let (digest, worktree) = (digest.clone(), worktree.to_owned());
    write.trust_head(pended, move |_| wire::TrustHead {
        digest,
        state: wire::TrustState::Pended,
        worktree: Some(worktree),
    });
    write
}

fn other_digest() -> wire::Digest {
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        .parse()
        .expect("fixture digest")
}

#[test]
fn held_recovery_preserves_the_ready_deadline_across_projection_restart() {
    let digest = digest();
    let (bus, mut projection) = iced_fixture(&digest);
    committed(bus.commit(grant_write(&digest)).expect("grant commits"));
    assert_recovery_deadline(&bus, &mut projection, 4);
    bus.shutdown().expect("held recovery fixture stops");
}

#[test]
fn reclaiming_a_released_task_preserves_its_new_ready_origin() {
    let digest = digest();
    let (bus, mut projection) = iced_fixture(&digest);
    committed(bus.commit(grant_write(&digest)).expect("grant commits"));
    let snapshot = bus.task_snapshot().expect("held snapshot reads");
    let heads = &snapshot.tasks()[0];
    let task = heads.statement().body().born.clone();
    let claim = heads
        .latest_claim()
        .map(|held| held.claim())
        .expect("active claim")
        .body()
        .clone();
    let mut release = JournalWrite::new(
        Actor::Lane(lane()),
        format!("task:{}", handle(TASK_MOMENT))
            .parse()
            .expect("task origin"),
        instant(5),
        Producer::CommandDoor,
    );
    let freed = release
        .fact(
            body::ClaimFreed {
                task: task.clone(),
                actor: claim.actor.clone(),
                parked: None,
            },
            FactCause::Root,
        )
        .expect("release fact builds");
    release.claim_head(task, freed, move |position| wire::ClaimHead {
        fact: position,
        disposition: wire::ClaimDisposition::Freed,
        ..claim
    });
    committed(bus.commit(release).expect("release commits"));
    projection
        .advance(&bus.task_snapshot().expect("released snapshot"))
        .expect("release projects");
    committed(
        bus.commit(iced_claim_write(&bus, &digest, instant(6)))
            .expect("new claim commits"),
    );
    assert_recovery_deadline(&bus, &mut projection, 8);
    bus.shutdown().expect("reclaimed fixture stops");
}

fn assert_recovery_deadline(bus: &Bus, projection: &mut Projection, deadline: u8) {
    let snapshot = bus.task_snapshot().expect("held snapshot reads");
    projection
        .advance(&snapshot)
        .expect("grant projection advances");
    let mut restarted = Projection::new();
    restarted
        .advance(&snapshot)
        .expect("restarted projection rehydrates");

    for projection in [&*projection, &restarted] {
        for second in [deadline - 1, deadline, 50] {
            let now = instant(second);
            let targets = project_targets(
                &snapshot,
                projection,
                now,
                &Holds::new(3),
                &board::NextConfig::default(),
                None,
                &owners(&snapshot),
            )
            .expect("held recovery target projects");
            let expected = if second < deadline {
                Resolution::Wait {
                    trigger: Trigger::HeldClaim,
                    lane: lane(),
                    deadline: instant(deadline),
                }
            } else {
                Resolution::Attempt {
                    trigger: Trigger::HeldClaim,
                    lane: lane(),
                }
            };
            assert_eq!(resolve(&targets, &Backoff::default(), now,), expected);
        }
    }
}

fn iced_fixture(digest: &wire::Digest) -> (Bus, Projection) {
    let (bus, mut projection) = policy_fixture(true, false);
    committed(
        bus.commit(iced_claim_write(&bus, digest, instant(2)))
            .expect("iced claim commits"),
    );
    projection
        .advance(&bus.task_snapshot().expect("iced snapshot reads"))
        .expect("iced projection advances");
    (bus, projection)
}

pub(super) fn target<'a>(snapshot: &bus::TaskSnapshot, projection: &'a Projection) -> Target<'a> {
    let targets = project_targets(
        snapshot,
        projection,
        instant(50),
        &Holds::new(3),
        &board::NextConfig::default(),
        None,
        &owners(snapshot),
    )
    .expect("ice targets project");
    assert_eq!(targets.len(), 1);
    targets[0]
}

fn iced_claim_write(bus: &Bus, digest: &wire::Digest, at: Instant) -> JournalWrite {
    held_claim_write(bus, at, |statement, _| {
        statement.requirements = wire::EnvironmentRequirements::one(
            wire::EnvironmentRequirement::trust_grant(digest.clone()),
        );
    })
}

/// Claim the seeded task for `lane()` on an indefinite lease after `edit`
/// revises its statement and workflow.
pub(super) fn held_claim_write(
    bus: &Bus,
    at: Instant,
    edit: impl FnOnce(&mut TaskHead, &mut wire::WorkflowHead),
) -> JournalWrite {
    claim_write_until(bus, at, wire::Instant::indefinite(), edit)
}

/// Claim the seeded task for `lane()` on a lease expiring at `horizon` after
/// `edit` revises its statement and workflow.
fn claim_write_until(
    bus: &Bus,
    at: Instant,
    horizon: Instant,
    edit: impl FnOnce(&mut TaskHead, &mut wire::WorkflowHead),
) -> JournalWrite {
    let snapshot = bus.task_snapshot().expect("seed snapshot reads");
    let heads = &snapshot.tasks()[0];
    let task = heads.statement().body().born.clone();
    let mut statement = heads.statement().body().clone();
    let original_workflow = heads.workflow().body().clone();
    let mut workflow = original_workflow.clone();
    edit(&mut statement, &mut workflow);
    let workflow_changed = workflow != original_workflow;
    let phase = workflow.phase;
    let occupant = uuid(9);
    let mut write = JournalWrite::new(
        Actor::Lane(lane()),
        format!("task:{}", handle(TASK_MOMENT))
            .parse()
            .expect("fixture task origin"),
        at,
        Producer::CommandDoor,
    );
    let edited = write
        .fact(
            body::TaskEdited {
                intent: statement
                    .intent
                    .map(|intent| wire::TaskIntentEdit::Set { intent }),
                task: task.clone(),
                revision: 2,
                dependencies: None,
                flow: None,
            },
            FactCause::Root,
        )
        .expect("requirement edit builds");
    let won = write
        .fact(
            body::ClaimWon {
                task: task.clone(),
                actor: Actor::Lane(lane()),
                lane: lane(),
                occupant,
                epoch: epoch(),
                cell: wire::ClaimCell {
                    topic: Some(TOPIC.parse().expect("fixture topic")),
                    phase,
                },
                until: horizon,
                git: None,
                recovery: None,
                source: body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Pending(edited),
        )
        .expect("claim win builds");
    write.task_head(edited, move |position| TaskHead {
        revision: position,
        ..statement
    });
    if workflow_changed {
        write.workflow_head(task.clone(), edited, move |_| workflow);
    }
    let claim_task = task.clone();
    write.claim_head(claim_task, won, move |position| wire::ClaimHead {
        actor: Actor::Lane(lane()),
        fact: position,
        claimed: position,
        cell: wire::ClaimCell {
            topic: Some(TOPIC.parse().expect("fixture topic")),
            phase,
        },
        lane: lane(),
        occupant,
        epoch: epoch(),
        recovery: None,
        disposition: wire::ClaimDisposition::Active,
        acknowledged: 0,
    });
    write.lease_head(task, won, move |claimed| wire::LeaseHead {
        claimed,
        start: at,
        renewal: at,
        expiry: horizon,
        maximum: horizon,
        epoch: epoch(),
    });
    write
}

fn grant_write(digest: &wire::Digest) -> JournalWrite {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "repo:genesis".parse().expect("fixture origin"),
        instant(3),
        Producer::CommandDoor,
    );
    let pended = write
        .fact(
            body::TrustPended {
                surface: "spice".to_owned(),
                digest: digest.clone(),
                worktree: Some("/producer/worktree".to_owned()),
                previous: None,
                documents: None,
            },
            FactCause::Root,
        )
        .expect("trust pend builds");
    let granted = write
        .fact(
            body::TrustGranted {
                surface: "spice".to_owned(),
                digest: digest.clone(),
            },
            FactCause::Pending(pended),
        )
        .expect("trust grant builds");
    let digest = digest.clone();
    write.trust_head(granted, move |_| wire::TrustHead {
        digest,
        state: wire::TrustState::Granted,
        worktree: Some("/producer/worktree".to_owned()),
    });
    write
}

fn digest() -> wire::Digest {
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        .parse()
        .expect("fixture digest")
}
