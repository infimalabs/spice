use std::fs;
use std::path::PathBuf;

use journal::{FactCause, Journal, JournalWrite};
use wire::{Producer, ResidencyHead};

use super::*;
use crate::resident::new_uuid;

mod claim;

struct Fixture {
    root: PathBuf,
    paths: RuntimePaths,
    request: Request,
    journal: Option<Journal>,
    tape: Tape,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("spice-archival-{}", new_uuid()));
        crate::test_repository::initialize(&root);
        let root = fs::canonicalize(root).expect("fixture root");
        let paths = RuntimePaths::discover(&root).expect("fixture paths");
        fs::create_dir_all(&paths.data).expect("fixture data");
        let request = Request {
            version: super::super::REQUEST_VERSION,
            worktree: root.display().to_string(),
            vendor: "omp".to_owned(),
            epoch: new_uuid(),
            lane: new_uuid(),
            occupant: new_uuid(),
            resume: None,
            fast_mode: false,
            model: None,
            effort: None,
            state: paths.supervisors.join("unused-state.json"),
        };
        let tape = Tape::open(paths.lane_tape(request.lane).join("omp")).expect("tape opens");
        let journal = Journal::open(&paths.journal).expect("journal opens");
        let mut fixture = Self {
            root,
            paths,
            request,
            journal: Some(journal),
            tape,
        };
        fixture.seat(true);
        fixture
    }

    fn seat(&mut self, occupied: bool) {
        let request = &self.request;
        self.residency(vec![wire::LaneResidency {
            lane: request.lane,
            team: request.lane,
            name: None,
            worktree: request.worktree.clone(),
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("vendor"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: occupied.then_some(request.occupant),
            lineage: vec![request.occupant],
            capabilities: Vec::new(),
            session: None,
            disposition: if occupied { "occupied" } else { "vacant" }.to_owned(),
        }]);
    }

    fn unseat(&mut self) {
        self.residency(Vec::new());
    }

    fn residency(&mut self, lanes: Vec<wire::LaneResidency>) {
        let request = &self.request;
        let head = ResidencyHead {
            epoch: request.epoch,
            state: wire::ResidentState::Seated,
            topology_revision: 0,
            lanes,
            subscriptions: Vec::new(),
            renewals: Vec::new(),
        };
        let mut write = JournalWrite::new(
            Actor::Spice,
            format!("resident:{}", request.epoch)
                .parse()
                .expect("origin"),
            wire::Instant::new(jiff::Timestamp::now()).expect("instant"),
            Producer::CommandDoor,
        );
        let fact = write
            .fact(
                wire::body::UpSeated {
                    epoch: request.epoch,
                },
                FactCause::Root,
            )
            .expect("resident fact");
        write.residency_head(fact, move |_| head);
        self.journal
            .as_mut()
            .expect("journal")
            .apply(write)
            .expect("residency publishes");
    }

    fn fill(&self, length: u64) -> tape::Locus {
        let block = vec![b'x'; 64 * 1024];
        let mut remaining = length;
        let mut locus = None;
        while remaining != 0 {
            let take = usize::try_from(remaining.min(block.len() as u64)).expect("bounded block");
            let appended = self.tape.tee(&block[..take]).expect("capture appends");
            locus.get_or_insert(appended);
            remaining -= take as u64;
        }
        locus.expect("positive capture length")
    }

    fn predecessor(&self, drained: bool) {
        let request = &self.request;
        let session = wire::LaneSession {
            lane: request.lane,
            occupant: request.occupant,
            worktree: request.worktree.clone(),
            vendor: "omp".parse().expect("vendor"),
            launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
            epoch: request.epoch,
            stream: "stdout".to_owned(),
            identity: "fixture-thread".to_owned(),
            locus: wire::TapeLocus {
                member: 1,
                offset: 0,
                length: 1,
            },
        };
        super::super::test_support::record_exited_for_tests(
            &self.paths,
            request.lane,
            request.occupant,
            session,
        )
        .expect("terminal predecessor");
        let path =
            supervisor_directory(&self.paths, request.lane, request.occupant).join("state.json");
        let mut state = read_state(&path).expect("state reads");
        state.capture_drained = drained;
        super::super::publish_json(&path, &state).expect("capture proof publishes");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.journal.take());
        fs::remove_dir_all(&self.root).expect("fixture cleanup");
    }
}

#[test]
fn size_crossing_waits_for_an_owned_boundary_and_preserves_old_references() {
    let mut fixture = Fixture::new();
    let locus = fixture.fill(MINIMUM - 1);
    assert!(shutdown(&fixture.paths, &fixture.request).is_none());
    fixture.tape.tee(b"x").expect("minimum reached");
    assert!(!fixture.tape.extents().expect("extents")[0].sealed);
    fixture.seat(false);
    assert!(
        shutdown(&fixture.paths, &fixture.request).is_none(),
        "vacant is not this occupant's shutdown"
    );
    fixture.seat(true);
    let eligible = shutdown(&fixture.paths, &fixture.request).expect("owned quiescent boundary");
    compress(&eligible);
    assert!(fixture.tape.extents().expect("sealed extents")[0].sealed);
    assert_eq!(
        fixture.tape.read(locus).expect("old locus"),
        vec![b'x'; locus.length as usize]
    );
    // A committed archive can retain plaintext if cleanup was interrupted.
    // That disk usage still earns maintenance without any new capture.
    let residue = fixture.tape.root().join("000000000001.plain");
    fs::write(&residue, vec![b'x'; MINIMUM as usize]).expect("interrupted cleanup residue");
    let eligible = shutdown(&fixture.paths, &fixture.request).expect("residue earns maintenance");
    compress(&eligible);
    assert!(!residue.exists());
    assert_eq!(
        fixture.tape.plaintext_bytes().expect("retained plaintext"),
        0
    );
    let next = fixture.tape.tee(b"next occupant").expect("next capture");
    assert_eq!(next.member, locus.member + 1);
    assert_eq!(
        fixture.tape.read(next).expect("new locus"),
        b"next occupant"
    );
}

#[test]
fn startup_requires_both_vacancy_and_proven_capture_drain() {
    let mut fixture = Fixture::new();
    fixture.fill(MINIMUM);
    fixture.predecessor(false);
    fixture.seat(false);
    startup(&fixture.paths, &fixture.request).expect("undrained startup skips");
    assert!(!fixture.tape.extents().expect("extents")[0].sealed);
    fixture.predecessor(true);
    fixture.seat(true);
    startup(&fixture.paths, &fixture.request).expect("occupied startup skips");
    assert!(!fixture.tape.extents().expect("extents")[0].sealed);
    fixture.seat(false);
    let epoch = fixture.request.epoch;
    fixture.request.epoch = new_uuid();
    startup(&fixture.paths, &fixture.request).expect("moved ownership skips");
    assert!(!fixture.tape.extents().expect("extents")[0].sealed);
    fixture.request.epoch = epoch;
    startup(&fixture.paths, &fixture.request).expect("drained vacant startup");
    assert!(fixture.tape.extents().expect("extents")[0].sealed);
}

#[test]
fn unreadable_predecessor_state_skips_opportunistic_startup_archival() {
    let mut fixture = Fixture::new();
    fixture.tape.tee(b"x").expect("one captured byte");
    fixture.predecessor(true);
    fixture.seat(false);
    let state = supervisor_directory(
        &fixture.paths,
        fixture.request.lane,
        fixture.request.occupant,
    )
    .join("state.json");
    fs::write(state, b"not supervisor state").expect("state corrupts");

    startup(&fixture.paths, &fixture.request).expect("archival uncertainty only skips");

    assert!(!fixture.tape.extents().expect("extents")[0].sealed);
}

#[test]
fn even_an_incomplete_job_record_blocks_archival() {
    let fixture = Fixture::new();
    fixture.fill(MINIMUM);
    let pending = spice::repository::lane_jobs(&fixture.paths.supervisors, fixture.request.lane)
        .join("partial");
    fs::create_dir_all(&pending).expect("partial job publication");
    assert!(shutdown(&fixture.paths, &fixture.request).is_none());
    fs::remove_dir_all(pending).expect("abandoned fixture job removed");
    assert!(shutdown(&fixture.paths, &fixture.request).is_some());
}

#[test]
fn journal_removed_lane_is_terminal_without_supervisor_checks() {
    let mut fixture = Fixture::new();
    fixture.unseat();
    let state = supervisor_directory(
        &fixture.paths,
        fixture.request.lane,
        fixture.request.occupant,
    )
    .join("state.json");
    fs::create_dir_all(state.parent().expect("state has a parent"))
        .expect("retained supervisor directory creates");
    fs::write(&state, br#"{"version":2}"#).expect("retained old state plants");

    assert_eq!(
        historical_authority(&fixture.paths, fixture.request.lane)
            .expect("unseated authority reads"),
        HistoricalAuthority::Removed
    );
}

#[test]
fn maintenance_exclusion_is_visible_to_launches_and_read_only_observers() {
    let fixture = Fixture::new();
    assert!(
        !maintenance_contended(&fixture.paths.tape, fixture.request.lane)
            .expect("an absent maintenance lock is available")
    );
    let MaintenanceAttempt::Acquired(first) =
        acquire_maintenance(&fixture.paths.tape, fixture.request.lane)
            .expect("the first owner acquires maintenance")
    else {
        panic!("a fresh maintenance boundary was contended");
    };
    assert!(
        maintenance_contended(&fixture.paths.tape, fixture.request.lane)
            .expect("the held maintenance lock is observable")
    );
    assert!(matches!(
        acquire_maintenance(&fixture.paths.tape, fixture.request.lane)
            .expect("contention is typed"),
        MaintenanceAttempt::Contended
    ));
    drop(first);
    assert!(matches!(
        acquire_maintenance(&fixture.paths.tape, fixture.request.lane)
            .expect("the released boundary reacquires"),
        MaintenanceAttempt::Acquired(_)
    ));
}

#[test]
fn historical_authority_requires_vacancy_jobs_and_drained_supervisors() {
    let mut fixture = Fixture::new();
    assert_eq!(
        historical_authority(&fixture.paths, fixture.request.lane)
            .expect("occupied authority reads"),
        HistoricalAuthority::Active
    );

    fixture.predecessor(true);
    fixture.seat(false);
    assert_eq!(
        historical_authority(&fixture.paths, fixture.request.lane)
            .expect("drained authority reads"),
        HistoricalAuthority::Eligible
    );

    let pending = spice::repository::lane_jobs(&fixture.paths.supervisors, fixture.request.lane)
        .join("partial");
    fs::create_dir_all(&pending).expect("partial job publication");
    assert_eq!(
        historical_authority(&fixture.paths, fixture.request.lane).expect("job authority reads"),
        HistoricalAuthority::JobHeld
    );
    fs::remove_dir_all(pending).expect("partial job removes");

    let state = supervisor_directory(
        &fixture.paths,
        fixture.request.lane,
        fixture.request.occupant,
    )
    .join("state.json");
    fs::write(state, b"not supervisor state").expect("state corrupts");
    assert!(matches!(
        historical_authority(&fixture.paths, fixture.request.lane)
            .expect("uncertain authority is classified"),
        HistoricalAuthority::Unknown(_)
    ));
}
